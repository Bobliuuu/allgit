//! Refs as a base plus an append-only delta log (feature `refs-delta`, issue #436 item 2c).
//!
//! The classic `refs` file is read and rewritten whole by every operation (45 MB at 1M tags). In a
//! repository that crossed `VCS_REFS_DELTA_MIN` tags (default 50,000) the state is instead:
//!
//! * `refs` (magic `AGR2`): branches and snapshots, and a pointer to the tag table: the blake3 id and
//!   count of the content-addressed file `reftags.<hex>` (the tag section, written once, never changed);
//! * `refs.log`: header `AGRL` + hash8 of the `refs` file it extends, then checksummed records, each
//!   one operation's changes to branches, snapshots and tags (absolute values, so a replay is
//!   idempotent). One record is one atomic step: a torn tail fails its checksum and is dropped; a
//!   record whose sequence number is not the next one ends the replay.
//!
//! Reading `refs` reads the small file and the log; the tag table is read only when a command looks at
//! tags (`Tags::get_key` scans the base without allocating; `Tags::set`/`unset` never read it).
//! Compaction (new `reftags.<hex>` first, then ONE atomic rename of `refs`) happens when the log
//! exceeds a fraction of the base; the old log is then stale (its header names the old `refs` hash) and
//! is ignored. A crash at any point leaves either the old or the new state. Older binaries refuse the
//! repository through `features` (and the `AGR2` magic).
//!
//! Operation log: an operation stores the tag changes (`OpView::tags_delta`) instead of the whole
//! table, with a full copy every `DELTA_DEPTH` operations (see `ops.rs`).

use crate::error::{io, Error, Result};
use crate::meta::{frame, unframe, write_atomic, Refs};
use crate::reftags::{base_name, Lazy, TagOp, Tags};
use crate::repo::Repo;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::names::{validate_branch_name, validate_stored_branch_name, validate_zone_name};
use vcs_objects::ObjectId;

pub(crate) const FEATURE: &str = "refs-delta";
pub(crate) const LOG: &str = "refs.log";
const REFS2: &[u8; 4] = b"AGR2";
const LOG_MAGIC: &[u8; 4] = b"AGRL";
const LOG_HDR: usize = 20;
const REC_HDR: usize = 12;
/// An operation records a full tag copy again after this many tag-delta operations.
pub(crate) const DELTA_DEPTH: u32 = 64;

type Key = (String, String);
type BranchOp = (Key, Option<ObjectId>);
type SnapOp = (String, Option<ObjectId>);

/// Number of tags from which a repository switches to delta mode (`VCS_REFS_DELTA=0` disables it).
pub(crate) fn delta_min() -> usize {
    if matches!(std::env::var("VCS_REFS_DELTA").as_deref(), Ok("0" | "off" | "false")) {
        return usize::MAX;
    }
    std::env::var("VCS_REFS_DELTA_MIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50_000)
}

fn h8(parts: &[&[u8]]) -> [u8; 8] {
    let mut h = blake3::Hasher::new();
    for p in parts {
        h.update(p);
    }
    let mut o = [0u8; 8];
    o.copy_from_slice(&h.finalize().as_bytes()[..8]);
    o
}

/// What the on-disk state was when this `Refs` was loaded (the baseline of the next delta).
#[derive(Clone, Debug)]
pub(crate) struct DeltaState {
    refs_hash: [u8; 8],
    orig_branches: BTreeMap<Key, ObjectId>,
    orig_snaps: BTreeMap<String, ObjectId>,
    tag_id: Option<[u8; 32]>,
    /// Chain over the checksums of the log records that changed tags.
    tag_chain: [u8; 32],
    log_seq: u32,
}

impl Refs {
    /// Identifies the tag state (base file plus tag-changing log records) without reading the table.
    pub(crate) fn tag_token(&self) -> Option<ObjectId> {
        let d = self.delta.as_ref()?;
        let mut h = blake3::Hasher::new();
        h.update(&d.tag_id.unwrap_or([0; 32])).update(&d.tag_chain);
        Some(ObjectId::from_bytes(*h.finalize().as_bytes()))
    }
}

#[derive(Default)]
struct Body {
    branches: Vec<BranchOp>,
    snaps: Vec<SnapOp>,
    tags: Vec<TagOp>,
}

fn put_opt(w: &mut Writer, id: &Option<ObjectId>) {
    match id {
        Some(id) => {
            w.u8(1).id(id);
        }
        None => {
            w.u8(0);
        }
    }
}

fn get_opt(r: &mut Reader) -> std::result::Result<Option<ObjectId>, vcs_objects::codec::DecodeError> {
    match r.u8()? {
        0 => Ok(None),
        1 => Ok(Some(r.id()?)),
        _ => Err(vcs_objects::codec::DecodeError::Invalid("delta flag")),
    }
}

impl Body {
    fn is_empty(&self) -> bool {
        self.branches.is_empty() && self.snaps.is_empty() && self.tags.is_empty()
    }
    fn encode(&self, seq: u32) -> Vec<u8> {
        let mut w = Writer::new();
        w.raw(&seq.to_le_bytes());
        w.varint(self.branches.len() as u64);
        for ((a, b), id) in &self.branches {
            w.str(a).str(b);
            put_opt(&mut w, id);
        }
        w.varint(self.snaps.len() as u64);
        for (n, id) in &self.snaps {
            w.str(n);
            put_opt(&mut w, id);
        }
        w.varint(self.tags.len() as u64);
        for ((a, b), id) in &self.tags {
            w.str(a).str(b);
            put_opt(&mut w, id);
        }
        w.finish()
    }
    /// Decode a record payload (after the sequence number). Names are validated like the base file's.
    fn decode(payload: &[u8]) -> std::result::Result<(u32, Body), vcs_objects::codec::DecodeError> {
        let mut r = Reader::new(payload);
        let seq = u32::from_le_bytes(r.raw(4)?.try_into().expect("4 bytes"));
        let mut b = Body::default();
        let bad = |_| vcs_objects::codec::DecodeError::Invalid("name");
        for _ in 0..r.count(4)? {
            let a = r.str(64)?;
            let n = r.str(200)?;
            validate_zone_name(&a).map_err(bad)?;
            validate_stored_branch_name(&n).map_err(bad)?;
            b.branches.push(((a, n), get_opt(&mut r)?));
        }
        for _ in 0..r.count(3)? {
            let n = r.str(200)?;
            validate_branch_name(&n).map_err(bad)?;
            b.snaps.push((n, get_opt(&mut r)?));
        }
        for _ in 0..r.count(4)? {
            let a = r.str(64)?;
            let n = r.str(200)?;
            validate_zone_name(&a).map_err(bad)?;
            validate_branch_name(&n).map_err(bad)?;
            b.tags.push(((a, n), get_opt(&mut r)?));
        }
        r.finish()?;
        Ok((seq, b))
    }
}

fn log_header(refs_hash: &[u8; 8]) -> [u8; LOG_HDR] {
    let mut h = [0u8; LOG_HDR];
    h[..4].copy_from_slice(LOG_MAGIC);
    h[4..12].copy_from_slice(refs_hash);
    let c = h8(&[&h[..12]]);
    h[12..].copy_from_slice(&c);
    h
}

fn record(refs_hash: &[u8; 8], payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(REC_HDR + payload.len());
    v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    v.extend_from_slice(&h8(&[refs_hash, payload]));
    v.extend_from_slice(payload);
    v
}

/// The valid prefix of a log extending the `refs` file with hash `refs_hash`.
struct Replay {
    recs: Vec<(Body, [u8; 8])>,
    /// Bytes of the valid prefix (header included); 0 when there is no usable header.
    valid_len: usize,
}

fn replay(buf: &[u8], refs_hash: &[u8; 8]) -> Replay {
    let none = Replay {
        recs: vec![],
        valid_len: 0,
    };
    if buf.len() < LOG_HDR || buf[..LOG_HDR] != log_header(refs_hash) {
        return none;
    }
    let mut recs = Vec::new();
    let mut off = LOG_HDR;
    while off + REC_HDR <= buf.len() {
        let plen = u32::from_le_bytes(buf[off..off + 4].try_into().expect("4")) as usize;
        if plen < 4 || off + REC_HDR + plen > buf.len() {
            break;
        }
        let payload = &buf[off + REC_HDR..off + REC_HDR + plen];
        let chk: [u8; 8] = buf[off + 4..off + 12].try_into().expect("8");
        if chk != h8(&[refs_hash, payload]) {
            break;
        }
        let Ok((seq, body)) = Body::decode(payload) else { break };
        if seq as usize != recs.len() {
            break;
        }
        recs.push((body, chk));
        off += REC_HDR + plen;
    }
    Replay { recs, valid_len: off }
}

thread_local! {
    /// (repo dir, tag token before the write, tag changes of the write): handed from the ref write to the
    /// `record_op` that follows it, so an operation can store the changes instead of the whole table.
    static STASH: RefCell<Option<(PathBuf, ObjectId, Vec<TagOp>)>> = const { RefCell::new(None) };
}

pub(crate) fn take_stash(dir: &std::path::Path) -> Option<(ObjectId, Vec<TagOp>)> {
    STASH.with(|s| {
        let v = s.borrow_mut().take()?;
        (v.0 == dir).then_some((v.1, v.2))
    })
}

fn injected(path: PathBuf) -> Error {
    Error::Io {
        path,
        source: std::io::Error::other("injected crash"),
    }
}

impl Repo {
    /// Load `refs`, in either format.
    pub(crate) fn load_refs(&self) -> Result<Refs> {
        let bytes = self.rd("refs")?;
        if bytes.len() < 4 || &bytes[..4] != REFS2 {
            return Refs::decode(&bytes);
        }
        let body = unframe(REFS2, &bytes, "refs")?;
        let mut r = Reader::new(body);
        let e = |x| Error::Corrupt(format!("refs: {x}"));
        let mut refs = Refs::default();
        for _ in 0..r.count(34).map_err(e)? {
            let a = r.str(64).map_err(e)?;
            let b = r.str(200).map_err(e)?;
            validate_zone_name(&a)?;
            validate_stored_branch_name(&b)?;
            refs.branches.insert((a, b), r.id().map_err(e)?);
        }
        for _ in 0..r.count(33).map_err(e)? {
            let n = r.str(200).map_err(e)?;
            validate_branch_name(&n)?;
            refs.snapshots.insert(n, r.id().map_err(e)?);
        }
        let tcount = r.varint().map_err(e)?;
        let tag_id = if tcount > 0 {
            let mut a = [0u8; 32];
            a.copy_from_slice(r.raw(32).map_err(e)?);
            Some(a)
        } else {
            None
        };
        r.finish().map_err(e)?;
        let refs_hash = h8(&[&bytes]);
        let log = fs::read(self.common.join(LOG)).unwrap_or_default();
        let rp = replay(&log, &refs_hash);
        let mut tag_chain = [0u8; 32];
        let mut logops = Vec::new();
        for (body, chk) in &rp.recs {
            for (k, v) in &body.branches {
                match v {
                    Some(id) => refs.branches.insert(k.clone(), *id),
                    None => refs.branches.remove(k),
                };
            }
            for (k, v) in &body.snaps {
                match v {
                    Some(id) => refs.snapshots.insert(k.clone(), *id),
                    None => refs.snapshots.remove(k),
                };
            }
            if !body.tags.is_empty() {
                logops.extend(body.tags.iter().cloned());
                tag_chain = *blake3::hash(&[&tag_chain[..], &chk[..]].concat()).as_bytes();
            }
        }
        refs.tags = Tags::lazy(Lazy::new(self.common.clone(), tag_id.unwrap_or([0; 32]), tcount, logops));
        refs.delta = Some(Arc::new(DeltaState {
            refs_hash,
            orig_branches: refs.branches.clone(),
            orig_snaps: refs.snapshots.clone(),
            tag_id,
            tag_chain,
            log_seq: rp.recs.len() as u32,
        }));
        Ok(refs)
    }

    /// Persist `refs` in delta mode when it applies (the repository is in it, or just crossed the
    /// threshold). Returns false when the caller must write the classic file.
    pub(crate) fn try_write_refs_delta(&self, refs: &Refs) -> Result<bool> {
        let in_mode = refs.delta.is_some();
        if !in_mode && refs.tags.len_hint() < delta_min() {
            return Ok(false);
        }
        if let Some(d) = &refs.delta {
            if let Some(ops) = refs.tags.delta_ops() {
                if self.append_delta(refs, d, ops)? {
                    return Ok(true);
                }
            }
        }
        self.write_delta_base(refs)?;
        Ok(true)
    }

    /// Append one record. Returns false when a compaction (new base) is needed instead.
    fn append_delta(&self, refs: &Refs, d: &DeltaState, tag_ops: Vec<TagOp>) -> Result<bool> {
        let mut body = Body {
            tags: tag_ops,
            ..Body::default()
        };
        for (k, v) in &refs.branches {
            if d.orig_branches.get(k) != Some(v) {
                body.branches.push((k.clone(), Some(*v)));
            }
        }
        for k in d.orig_branches.keys() {
            if !refs.branches.contains_key(k) {
                body.branches.push((k.clone(), None));
            }
        }
        for (k, v) in &refs.snapshots {
            if d.orig_snaps.get(k) != Some(v) {
                body.snaps.push((k.clone(), Some(*v)));
            }
        }
        for k in d.orig_snaps.keys() {
            if !refs.snapshots.contains_key(k) {
                body.snaps.push((k.clone(), None));
            }
        }
        if body.is_empty() {
            return Ok(true);
        }
        let refs_p = self.common.join("refs");
        let cur = io(&refs_p, fs::read(&refs_p))?;
        if h8(&[&cur]) != d.refs_hash {
            return Ok(false);
        }
        let log_p = self.common.join(LOG);
        let buf = fs::read(&log_p).unwrap_or_default();
        let rp = replay(&buf, &d.refs_hash);
        if rp.recs.len() as u32 != d.log_seq {
            return Ok(false); // changed under us: rewrite from the caller's complete state
        }
        let rec = record(&d.refs_hash, &body.encode(d.log_seq));
        let base_bytes = refs.tags.lazy_src().map_or(0, |l| l.count as usize * 40);
        let floor: usize = std::env::var("VCS_REFS_LOG_FLOOR")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(256 * 1024);
        if rp.valid_len + rec.len() > floor && rp.valid_len + rec.len() > base_bytes / 4 {
            return Ok(false);
        }
        if !body.tags.is_empty() {
            if let Some(tok) = refs.tag_token() {
                STASH.with(|s| *s.borrow_mut() = Some((self.common.clone(), tok, body.tags.clone())));
            }
        }
        if crate::failpoint::tick() {
            // simulated crash mid-append: half a record is left behind (or half a new log)
            if rp.valid_len == 0 {
                let _ = fs::write(&log_p, &log_header(&d.refs_hash)[..LOG_HDR / 2]);
            } else if let Ok(mut f) = fs::OpenOptions::new().write(true).open(&log_p) {
                let _ = f.seek(SeekFrom::Start(rp.valid_len as u64));
                let _ = f.write_all(&rec[..rec.len() / 2]);
            }
            return Err(injected(log_p));
        }
        if rp.valid_len == 0 {
            let tmp = log_p.with_extension(format!("tmp{}", std::process::id()));
            let mut v = log_header(&d.refs_hash).to_vec();
            v.extend_from_slice(&rec);
            {
                let mut f = io(&tmp, fs::File::create(&tmp))?;
                io(&tmp, f.write_all(&v))?;
                io(&tmp, f.sync_all())?;
            }
            io(&log_p, fs::rename(&tmp, &log_p))?;
        } else {
            let mut f = io(&log_p, fs::OpenOptions::new().write(true).open(&log_p))?;
            io(&log_p, f.set_len(rp.valid_len as u64))?; // drop a torn tail first
            io(&log_p, f.seek(SeekFrom::Start(rp.valid_len as u64)))?;
            io(&log_p, f.write_all(&rec))?;
            io(&log_p, f.sync_data())?;
        }
        Ok(true)
    }

    /// Write a complete base: the tag table (content addressed, first), then `refs` in one rename.
    fn write_delta_base(&self, refs: &Refs) -> Result<()> {
        let tags = &refs.tags;
        let old_tag = refs.delta.as_ref().and_then(|d| d.tag_id);
        let pre = refs.tag_token();
        let id: Option<[u8; 32]> = if tags.is_empty() {
            None
        } else {
            let mut w = Writer::new();
            tags.encode_into(&mut w);
            let bytes = w.finish();
            let id = *blake3::hash(&bytes).as_bytes();
            let p = self.common.join(base_name(&id));
            if !p.exists() {
                write_atomic(&p, &bytes)?;
            }
            Some(id)
        };
        self.note_feature(FEATURE)?;
        let mut w = Writer::new();
        w.varint(refs.branches.len() as u64);
        for ((a, b), i) in &refs.branches {
            w.str(a).str(b).id(i);
        }
        w.varint(refs.snapshots.len() as u64);
        for (n, i) in &refs.snapshots {
            w.str(n).id(i);
        }
        w.varint(tags.len() as u64);
        if let Some(id) = &id {
            w.raw(id);
        }
        let refs_p = self.common.join("refs");
        write_atomic(&refs_p, &frame(REFS2, w.finish()))?;
        let _ = fs::remove_file(self.common.join(LOG)); // stale anyway: its header names the old refs hash
                                                        // Old tag tables stay for a grace period: a concurrent reader may still be about to read one.
        if old_tag != id {
            if let Ok(rd) = fs::read_dir(&self.common) {
                let keep = id.map(|i| base_name(&i));
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if n.starts_with("reftags.") && Some(&n) != keep.as_ref() {
                        let old = e
                            .metadata()
                            .and_then(|m| m.modified())
                            .ok()
                            .and_then(|t| t.elapsed().ok())
                            .is_some_and(|d| d.as_secs() > 300);
                        if old {
                            let _ = fs::remove_file(e.path());
                        }
                    }
                }
            }
        }
        let _ = pre;
        Ok(())
    }

    /// Test and bench hook: the size of the log and whether the repository is in delta mode.
    #[doc(hidden)]
    pub fn refs_delta_info(&self) -> (bool, u64) {
        let on = self.rd("refs").map(|b| b.starts_with(REFS2)).unwrap_or(false);
        (on, fs::metadata(self.common.join(LOG)).map(|m| m.len()).unwrap_or(0))
    }
}
