//! Ref-update journal: one `fdatasync` per ref-changing operation (docs/durability.md).
//!
//! A ref-only operation (`tag`, `branch`, `commit`, `merge` fast-forward, ...) used to cost four
//! serial fsyncs: the `refs` file, the operation object (plus its directory) and `op_head`.
//! Now it appends ONE record to `.vcs/oplog.journal` and `fdatasync`s it; that is the commit
//! point. The record holds the complete new `refs` file and the encoded `Operation`. After it,
//! `refs`, the operation object and `op_head` are updated by plain renames with no fsync.
//!
//! Recovery is a roll-forward of the newest record: the journal is the source of truth for the
//! files that were not synced, so any mix of old and new among them is repaired by rewriting them
//! from the record. It runs
//! - in `Repo::lock` for every writer (a process that died between the commit point and its last
//!   rename is repaired before the next writer reads `refs`), and
//! - at open after a reboot (the journal header stores the boot id of the boot that started it; a
//!   power cut loses unsynced renames, a mere process crash does not).
//!
//! Invariant making the roll-forward safe: while the journal holds records, `refs` and `op_head`
//! change ONLY through [`Repo::commit_refs_op`]. Every other writer goes through
//! [`Repo::write_refs`] / `write_op_head`, which first [`checkpoint`](Repo::journal_checkpoint):
//! make the journalled state durable (fsync `refs`, `op_head`, the operation objects and the
//! directory), then start a new generation, which empties the journal. A checkpoint also happens
//! when the fixed-size journal is full. The journal file is preallocated with zeros so an append
//! overwrites existing blocks and `fdatasync` needs no metadata commit.
//!
//! Not available without a boot id (non-Linux/macOS unix) or on Windows: the old fully synced
//! protocol is used there.

use crate::error::Result;
use crate::meta::Refs;
use crate::repo::Repo;
use vcs_objects::{ObjectId, Signature};

pub(crate) const JOURNAL_FILE: &str = "oplog.journal";

impl Repo {
    /// Durably replace `refs` (the fully synced way) after making any journalled state durable.
    /// Every writer of the `refs` file outside [`Repo::commit_refs_op`] must use this.
    pub(crate) fn write_refs(&self, refs: &Refs) -> Result<()> {
        self.journal_checkpoint()?;
        crate::meta::write_atomic(&self.common.join("refs"), &refs.encode())
    }

    /// Change `refs` and record the operation as ONE durable step (one `fdatasync`), see the
    /// module docs. Caller holds the repository lock. Falls back to the fully synced protocol
    /// when the journal is unavailable.
    pub(crate) fn commit_refs_op(&self, refs: &Refs, description: &str, author: Option<Signature>) -> Result<ObjectId> {
        if let Some(id) = self.try_journaled_op(refs, description, &author)? {
            return Ok(id);
        }
        self.write_refs(refs)?;
        self.record_op(description, author)
    }
}

#[cfg(not(unix))]
impl Repo {
    fn try_journaled_op(&self, _: &Refs, _: &str, _: &Option<Signature>) -> Result<Option<ObjectId>> {
        Ok(None)
    }
    pub(crate) fn journal_checkpoint(&self) -> Result<()> {
        Ok(())
    }
    pub(crate) fn journal_verify(&self) -> Result<()> {
        Ok(())
    }
    pub(crate) fn journal_open_check(&self) {}
}

#[cfg(unix)]
mod imp {
    use super::*;
    use crate::error::{io, Error};
    use crate::meta::{frame, write_cache_nosync};
    use std::fs;
    use std::os::unix::fs::FileExt;
    use std::path::Path;
    use std::sync::OnceLock;
    use vcs_objects::{Object, OpView, Operation};
    use vcs_store::ObjectStore;

    const MAGIC: &[u8; 4] = b"AGJ1";
    const HDR: usize = 64;
    /// Fixed journal size. Records are ~100 bytes plus the refs file; a full journal checkpoints.
    pub(super) const CAP: usize = 64 * 1024;
    const REC_HDR: usize = 20;

    fn h8(parts: &[&[u8]]) -> [u8; 8] {
        let mut h = blake3::Hasher::new();
        for p in parts {
            h.update(p);
        }
        let mut o = [0u8; 8];
        o.copy_from_slice(&h.finalize().as_bytes()[..8]);
        o
    }

    /// Identifies this boot of the machine; `None` where we cannot tell (journal then disabled).
    fn boot_id() -> Option<[u8; 16]> {
        static ID: OnceLock<Option<[u8; 16]>> = OnceLock::new();
        *ID.get_or_init(|| {
            #[cfg(target_os = "linux")]
            let s = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
            #[cfg(target_os = "macos")]
            let s = {
                let mut buf = [0u8; 64];
                let mut len = buf.len();
                let name = c"kern.bootsessionuuid";
                // SAFETY: buf/len describe a valid writable buffer; name is NUL terminated.
                let rc = unsafe { libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
                if rc != 0 {
                    return None;
                }
                String::from_utf8_lossy(&buf[..len]).into_owned()
            };
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let s = String::new();
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            let mut o = [0u8; 16];
            o.copy_from_slice(&blake3::hash(s.as_bytes()).as_bytes()[..16]);
            Some(o)
        })
    }

    fn new_gen() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let mut g = [0u8; 8];
        g.copy_from_slice(&blake3::hash(format!("{t}/{}/{n}", std::process::id()).as_bytes()).as_bytes()[..8]);
        u64::from_le_bytes(g)
    }

    /// Header: magic, generation, boot id, hash of the `refs` file when the generation started
    /// (`base`), checksum. `base` lets recovery tell "refs is an older state of ours" (repair)
    /// from "something else wrote refs" (leave it alone).
    fn header(gen: u64, boot: &[u8; 16], base: &[u8; 8]) -> [u8; HDR] {
        let mut h = [0u8; HDR];
        h[..4].copy_from_slice(MAGIC);
        h[4..12].copy_from_slice(&gen.to_le_bytes());
        h[12..28].copy_from_slice(boot);
        h[28..36].copy_from_slice(base);
        let c = h8(&[&h[..36]]);
        h[36..44].copy_from_slice(&c);
        h
    }

    pub(super) struct Rec {
        op: ObjectId,
        refs: std::ops::Range<usize>,
        opb: std::ops::Range<usize>,
    }

    pub(super) struct Scan {
        buf: Vec<u8>,
        gen: u64,
        boot: [u8; 16],
        base: [u8; 8],
        recs: Vec<Rec>,
        tail: usize,
    }

    impl Scan {
        fn parse(buf: Vec<u8>) -> Option<Scan> {
            if buf.len() < HDR || &buf[..4] != MAGIC || buf[36..44] != h8(&[&buf[..36]]) {
                return None;
            }
            let gen = u64::from_le_bytes(buf[4..12].try_into().ok()?);
            let boot: [u8; 16] = buf[12..28].try_into().ok()?;
            let base: [u8; 8] = buf[28..36].try_into().ok()?;
            let mut recs = Vec::new();
            let mut off = HDR;
            while off + REC_HDR <= buf.len() {
                let plen = u32::from_le_bytes(buf[off..off + 4].try_into().ok()?) as usize;
                if plen < 36 || off + REC_HDR + plen > buf.len() {
                    break;
                }
                let g = &buf[off + 4..off + 12];
                let payload = &buf[off + REC_HDR..off + REC_HDR + plen];
                if g != gen.to_le_bytes() || buf[off + 12..off + 20] != h8(&[g, payload]) {
                    break;
                }
                let rl = u32::from_le_bytes(payload[32..36].try_into().ok()?) as usize;
                if 36 + rl > plen {
                    break;
                }
                let op = ObjectId::from_bytes(payload[..32].try_into().ok()?);
                let base = off + REC_HDR;
                recs.push(Rec {
                    op,
                    refs: base + 36..base + 36 + rl,
                    opb: base + 36 + rl..base + plen,
                });
                off += REC_HDR + plen;
            }
            Some(Scan {
                buf,
                gen,
                boot,
                base,
                recs,
                tail: off,
            })
        }
    }

    fn fsync_path(p: &Path) -> Result<()> {
        let f = io(p, fs::File::open(p))?;
        io(p, f.sync_all())
    }

    impl Repo {
        fn journal_path(&self) -> std::path::PathBuf {
            self.common.join(JOURNAL_FILE)
        }

        fn journal_scan(&self) -> Result<Option<Scan>> {
            let p = self.journal_path();
            match fs::read(&p) {
                Ok(b) => Ok(Scan::parse(b)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(source) => Err(Error::Io { path: p, source }),
            }
        }

        fn crash_point(&self) -> Result<()> {
            if crate::failpoint::tick() {
                return Err(Error::Io {
                    path: self.journal_path(),
                    source: std::io::Error::other("injected crash"),
                });
            }
            Ok(())
        }

        /// Create a fresh, empty, durable journal (a new generation).
        fn journal_init(&self, boot: &[u8; 16]) -> Result<()> {
            // Older binaries refuse a repository whose `features` names something they do not know;
            // the mark is durable before the journal can hold a record.
            self.note_feature(crate::repo::FEATURE_REF_JOURNAL)?;
            let p = self.journal_path();
            let tmp = p.with_extension(format!("init{}", std::process::id()));
            let mut buf = vec![0u8; CAP];
            let base = h8(&[&fs::read(self.common.join("refs")).unwrap_or_default()]);
            buf[..HDR].copy_from_slice(&header(new_gen(), boot, &base));
            io(&tmp, fs::write(&tmp, &buf))?;
            fsync_path(&tmp)?;
            io(&p, fs::rename(&tmp, &p))?;
            fsync_path(&self.common)
        }

        /// Roll the newest record forward: rewrite whatever of `refs`, the operation object and
        /// `op_head` differs from it. `full` also re-creates every journalled operation object
        /// (after a reboot any unsynced loose file may be gone).
        ///
        /// Returns `true` when `refs` is neither the newest record nor an older state of this
        /// journal: another writer changed it (an older vcs, a hand edit). Then nothing is
        /// rewritten but the missing operation objects, and the caller drops the journal.
        fn journal_apply(&self, s: &Scan, full: bool) -> Result<bool> {
            let Some(last) = s.recs.last() else { return Ok(false) };
            let refs_p = self.common.join("refs");
            let want = &s.buf[last.refs.clone()];
            let cur = fs::read(&refs_p).ok();
            let foreign = match &cur {
                Some(c) if c == want => false,
                Some(c) => {
                    let h = h8(&[c]);
                    h != s.base && !s.recs.iter().any(|r| h8(&[&s.buf[r.refs.clone()]]) == h)
                }
                None => false,
            };
            if !foreign && cur.as_deref() != Some(want) {
                write_cache_nosync(&refs_p, want)?;
            }
            let from = if full { 0 } else { s.recs.len() - 1 };
            for r in &s.recs[from..] {
                if !self.store.contains(&r.op)? {
                    let obj = Object::decode(&s.buf[r.opb.clone()]).map_err(|e| Error::Corrupt(format!("operation journal: {e}")))?;
                    if self.store.put_unsynced(&obj)? != r.op {
                        return Err(Error::Corrupt("operation journal: record does not hash to its id".into()));
                    }
                }
            }
            if !foreign && !matches!(self.op_head(), Ok(Some(h)) if h == last.op) {
                write_cache_nosync(&self.common.join("op_head"), &frame(b"AGOH", last.op.as_bytes().to_vec()))?;
            }
            Ok(foreign)
        }

        /// Make everything the journal covers durable, then start an empty generation.
        fn journal_checkpoint_scanned(&self, s: &Scan, boot: &[u8; 16]) -> Result<()> {
            let ids: Vec<ObjectId> = s.recs.iter().map(|r| r.op).collect();
            self.store.make_durable(&ids)?;
            fsync_path(&self.common.join("refs"))?;
            fsync_path(&self.common.join("op_head"))?;
            fsync_path(&self.common)?;
            self.crash_point()?; // everything durable, journal not yet emptied: replay is a no-op
            let f = io(&self.journal_path(), fs::OpenOptions::new().write(true).open(self.journal_path()))?;
            let base = h8(&[&fs::read(self.common.join("refs")).unwrap_or_default()]);
            io(&self.journal_path(), f.write_all_at(&header(new_gen(), boot, &base), 0))?;
            io(&self.journal_path(), f.sync_data())
        }

        /// Empty the journal after making its state durable. Caller holds the lock.
        pub(crate) fn journal_checkpoint(&self) -> Result<()> {
            let Some(boot) = boot_id() else { return Ok(()) };
            let Some(s) = self.journal_scan()? else { return Ok(()) };
            if s.recs.is_empty() {
                return Ok(());
            }
            self.journal_apply(&s, true)?;
            self.journal_checkpoint_scanned(&s, &boot)
        }

        /// Called right after taking the lock: finish a record whose renames did not all happen
        /// (or did not survive a power cut) before anyone reads `refs`.
        pub(crate) fn journal_verify(&self) -> Result<()> {
            let Some(boot) = boot_id() else { return Ok(()) };
            let Some(s) = self.journal_scan()? else { return Ok(()) };
            if s.recs.is_empty() {
                return Ok(());
            }
            if s.boot != boot {
                self.journal_apply(&s, true)?;
                return self.journal_checkpoint_scanned(&s, &boot);
            }
            if self.journal_apply(&s, false)? {
                return self.journal_checkpoint_scanned(&s, &boot); // foreign writer: drop the journal
            }
            Ok(())
        }

        /// At open: a journal written before a reboot is repaired at once, so even a reader sees
        /// the committed state. Costs one failed `open` when there is no journal.
        pub(crate) fn journal_open_check(&self) {
            let Some(boot) = boot_id() else { return };
            let mut h = [0u8; HDR];
            let Ok(f) = fs::File::open(self.journal_path()) else { return };
            if f.read_exact_at(&mut h, 0).is_err() || &h[..4] != MAGIC || h[12..28] == boot[..] {
                return;
            }
            // Best effort: another process may hold the lock (it verifies for itself).
            let _ = self.lock();
        }

        pub(super) fn try_journaled_op(&self, refs: &Refs, description: &str, author: &Option<Signature>) -> Result<Option<ObjectId>> {
            let Some(boot) = boot_id() else { return Ok(None) };
            // A large tag set is stored once and omitted from unchanged operations (ops.rs `record_op`);
            // the journal record always carries the full view, so large sets take the synced path.
            if refs.tags.len() >= crate::ops::OP_TAGS_DELTA_MIN {
                return Ok(None);
            }
            // Data objects the new state names (annotation, commits, ...) are durable first.
            self.store.sync()?;
            let ws = self.workspace()?;
            let op = Operation {
                parents: self.op_head()?.into_iter().collect(),
                view: OpView {
                    branches: refs.branches.clone(),
                    workspace: ws.subs,
                    tags: Some(refs.tags.clone()),
                },
                description: description.lines().next().unwrap_or("").to_owned(),
                author: author.clone().unwrap_or_else(crate::ops::env_signature),
            };
            let obj = Object::Operation(op);
            let opb = obj.encode();
            let id = vcs_objects::id_of_encoded(&opb);
            let refs_bytes = refs.encode();
            let plen = 36 + refs_bytes.len() + opb.len();
            if HDR + REC_HDR + plen > CAP {
                return Ok(None);
            }
            if self.journal_scan()?.is_some_and(|s| s.boot != boot && !s.recs.is_empty()) {
                self.journal_verify()?; // records from before a reboot must be rolled forward, never overwritten
            }
            let mut scan = self.journal_scan()?.filter(|s| s.boot == boot);
            if scan.as_ref().is_some_and(|s| s.tail + REC_HDR + plen > CAP) {
                self.journal_checkpoint()?;
                scan = None;
            }
            let (gen, tail) = match scan {
                Some(s) => (s.gen, s.tail),
                None => {
                    self.journal_init(&boot)?;
                    let s = self
                        .journal_scan()?
                        .ok_or_else(|| Error::Corrupt("journal vanished after creation".into()))?;
                    (s.gen, s.tail)
                }
            };
            let mut rec = Vec::with_capacity(REC_HDR + plen);
            rec.extend_from_slice(&(plen as u32).to_le_bytes());
            rec.extend_from_slice(&gen.to_le_bytes());
            let mut payload = Vec::with_capacity(plen);
            payload.extend_from_slice(id.as_bytes());
            payload.extend_from_slice(&(refs_bytes.len() as u32).to_le_bytes());
            payload.extend_from_slice(&refs_bytes);
            payload.extend_from_slice(&opb);
            rec.extend_from_slice(&h8(&[&gen.to_le_bytes(), &payload]));
            rec.extend_from_slice(&payload);

            self.crash_point()?; // before the commit point: the operation never happened
            let jp = self.journal_path();
            let f = io(&jp, fs::OpenOptions::new().write(true).open(&jp))?;
            io(&jp, f.write_all_at(&rec, tail as u64))?;
            io(&jp, f.sync_data())?; // COMMIT POINT
            drop(f);
            self.crash_point()?; // committed, nothing applied yet
            write_cache_nosync(&self.common.join("refs"), &refs_bytes)?;
            self.crash_point()?; // refs applied
            self.store.put_unsynced(&obj)?;
            self.crash_point()?; // operation object applied
            write_cache_nosync(&self.common.join("op_head"), &frame(b"AGOH", id.as_bytes().to_vec()))?;
            Ok(Some(id))
        }
    }
}
