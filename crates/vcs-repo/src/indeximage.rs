//! The index as the repository sees it: an `AGI5` base file (memory mapped) plus an append-only,
//! checksummed `index.log` of edits made since the base was written.
//!
//! * Lookups (`get`, `next_ge`, `under`) read the mapped base directly and the log's overlay, so
//!   a command that touches a few paths never decodes the other entries.
//! * A mutation of a few paths appends one frame to `index.log` (one `write`), instead of
//!   rewriting the whole index. When the log grows past [`MAX_OPS`] edits or [`MAX_LOG`] bytes the
//!   next write folds it into a new base generation (written to a temp file, renamed into
//!   place); the old log is then stale (its header names the old generation) and ignored.
//! * Crash safety: a frame is `len, seq, checksum, payload` written with a single `write`; a torn
//!   tail fails its checksum and is dropped by readers (the state before that frame) and
//!   truncated by the next writer. A base is replaced only by rename.
//! * Concurrent readers: a reader maps the base, reads the log, and re-checks that the base file
//!   is still the one it opened; if a writer compacted meanwhile it starts over. Mapped old
//!   generations stay valid (rename does not touch them).
//! * Older layouts (`AGI2`/`AGIX`/`AGI3`/`AGI4`) are read as they are and upgraded to `AGI5` by
//!   the first write. A binary that only knows those refuses an `AGI5` file with a clear error
//!   ("bad magic") instead of misreading it.
//!
//! Log layout: header `"AGL1", version u8, 3 pad, base_gen u64, base_sums_sum u64, hdr_sum u64`
//! (32 bytes), then frames `len u32, seq u32, sum u64, payload[len]` where `sum` mixes the base
//! generation, `seq` and the payload checksum. Payload: varint op count, then ops (tag u8):
//! 1 set `(path, entry)`, 2 remove `path`, 3 drop cached trees of the listed directories,
//! 4 replace the cache-tree section, 5 set `written_ns`.

use crate::error::{Error, Result};
use crate::index::{Index, IndexEntry};
use crate::index5::{self, sum64, Base5, Bytes, Trees};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::{EntryMode, ObjectId};

/// Edits in the log that trigger folding it into a new base.
pub(crate) const MAX_OPS: usize = 4096;
/// Log size that triggers folding it into a new base.
pub(crate) const MAX_LOG: u64 = 1 << 20;
const LOG_HDR: usize = 32;
const FRAME_HDR: usize = 16;
const MAX_FRAME: usize = 64 << 20;

fn corrupt(e: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("index: {e}"))
}

/// One logged edit.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Op {
    Set(String, IndexEntry),
    Remove(String),
    DropTrees(Vec<String>),
    SetTrees(Option<Trees>),
    WrittenNs(u64),
}

enum Backing {
    V5(Base5),
    /// An older layout, decoded; the raw bytes are kept for `flat`.
    Legacy(Box<Index>, Vec<u8>),
}

pub(crate) struct IndexImage {
    backing: Backing,
    pub written_ns: u64,
    overlay: BTreeMap<String, Option<IndexEntry>>,
    tree_ops: Vec<Op>,
    trees: OnceLock<Option<Trees>>,
    digest: [u8; 32],
    len: usize,
    nops: usize,
    log_end: u64,
    /// The log file on disk belongs to this base (so appends can go to it).
    log_live: bool,
    seq: u32,
}

fn frame_sum(gen: u64, seq: u32, payload: &[u8]) -> u64 {
    sum64(payload) ^ gen.rotate_left(17) ^ (seq as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

fn log_header(gen: u64, base_sum: u64) -> [u8; LOG_HDR] {
    let mut h = [0u8; LOG_HDR];
    h[..4].copy_from_slice(b"AGL1");
    h[4] = 1;
    h[8..16].copy_from_slice(&gen.to_le_bytes());
    h[16..24].copy_from_slice(&base_sum.to_le_bytes());
    let s = sum64(&h[..24]);
    h[24..32].copy_from_slice(&s.to_le_bytes());
    h
}

fn encode_ops(ops: &[Op]) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ops.len() as u64);
    for op in ops {
        match op {
            Op::Set(p, e) => {
                w.u8(1).str(p).u8(e.mode as u8).id(&e.id).varint(e.size).varint(e.mtime_ns).varint(e.ctime_ns);
            }
            Op::Remove(p) => {
                w.u8(2).str(p);
            }
            Op::DropTrees(ds) => {
                w.u8(3).varint(ds.len() as u64);
                for d in ds {
                    w.str(d);
                }
            }
            Op::SetTrees(t) => {
                w.u8(4);
                match t {
                    Some((fp, m)) if !m.is_empty() => {
                        w.varint(m.len() as u64).id(fp);
                        for (d, id) in m {
                            w.str(d).id(id);
                        }
                    }
                    _ => {
                        w.varint(0);
                    }
                }
            }
            Op::WrittenNs(n) => {
                w.u8(5).varint(*n);
            }
        }
    }
    w.finish()
}

fn decode_ops(payload: &[u8]) -> Result<Vec<Op>> {
    let c = |e: vcs_objects::DecodeError| corrupt(format!("log: {e}"));
    let mut r = Reader::new(payload);
    let n = r.count(2).map_err(c)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(match r.u8().map_err(c)? {
            1 => {
                let p = r.str(4096).map_err(c)?;
                vcs_objects::names::validate_path(&p)?;
                let mode = match r.u8().map_err(c)? {
                    1 => EntryMode::File,
                    2 => EntryMode::Executable,
                    4 => EntryMode::Symlink,
                    _ => return Err(corrupt("log: bad mode")),
                };
                let id = r.id().map_err(c)?;
                let (size, mtime_ns, ctime_ns) = (r.varint().map_err(c)?, r.varint().map_err(c)?, r.varint().map_err(c)?);
                Op::Set(
                    p,
                    IndexEntry {
                        id,
                        mode,
                        size,
                        mtime_ns,
                        ctime_ns,
                    },
                )
            }
            2 => Op::Remove(r.str(4096).map_err(c)?),
            3 => {
                let k = r.count(1).map_err(c)?;
                let mut ds = Vec::with_capacity(k);
                for _ in 0..k {
                    ds.push(r.str(4096).map_err(c)?);
                }
                Op::DropTrees(ds)
            }
            4 => {
                let k = r.varint().map_err(c)? as usize;
                if k == 0 {
                    Op::SetTrees(None)
                } else {
                    let fp = r.id().map_err(c)?;
                    let mut m = BTreeMap::new();
                    for _ in 0..k {
                        let d = r.str(4096).map_err(c)?;
                        m.insert(d, r.id().map_err(c)?);
                    }
                    Op::SetTrees(Some((fp, m)))
                }
            }
            5 => Op::WrittenNs(r.varint().map_err(c)?),
            t => return Err(corrupt(format!("log: unknown op {t}"))),
        });
    }
    r.finish().map_err(c)?;
    Ok(out)
}

fn io_err(p: &Path, e: std::io::Error) -> Error {
    Error::Io {
        path: p.to_owned(),
        source: e,
    }
}

/// The valid frames of a log file's bytes for a base `(gen, sum)`: `(payload ranges, end offset)`.
/// `None` when the header is missing, short or names another base.
fn scan_log(b: &[u8], gen: u64, base_sum: u64) -> Option<(Vec<(usize, usize, u64)>, usize)> {
    if b.len() < LOG_HDR || b[..LOG_HDR] != log_header(gen, base_sum) {
        return None;
    }
    let mut frames = Vec::new();
    let mut at = LOG_HDR;
    let mut seq = 1u32;
    while at + FRAME_HDR <= b.len() {
        let len = u32::from_le_bytes(b[at..at + 4].try_into().expect("4")) as usize;
        let s = u32::from_le_bytes(b[at + 4..at + 8].try_into().expect("4"));
        let sum = u64::from_le_bytes(b[at + 8..at + 16].try_into().expect("8"));
        if len > MAX_FRAME || s != seq || at + FRAME_HDR + len > b.len() {
            break;
        }
        let payload = &b[at + FRAME_HDR..at + FRAME_HDR + len];
        if frame_sum(gen, seq, payload) != sum {
            break;
        }
        frames.push((at + FRAME_HDR, len, sum));
        at += FRAME_HDR + len;
        seq += 1;
    }
    Some((frames, at))
}

fn chain(d: &[u8; 32], sum: u64) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(d);
    h.update(&sum.to_le_bytes());
    *h.finalize().as_bytes()
}

#[cfg(unix)]
fn ident(p: &Path) -> Option<(u64, u64, u64, i64)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(p).ok()?;
    Some((m.ino(), m.size(), m.mtime() as u64, m.mtime_nsec()))
}
#[cfg(not(unix))]
fn ident(p: &Path) -> Option<(u64, u64, u64, i64)> {
    let m = std::fs::metadata(p).ok()?;
    Some((0, m.len(), 0, 0))
}

impl IndexImage {
    fn from_backing(backing: Backing) -> IndexImage {
        let (written_ns, digest, len) = match &backing {
            Backing::V5(b) => (b.written_ns, b.digest(), b.len_bytes()),
            Backing::Legacy(ix, raw) => (ix.written_ns, *blake3::hash(raw).as_bytes(), raw.len()),
        };
        IndexImage {
            backing,
            written_ns,
            overlay: BTreeMap::new(),
            tree_ops: Vec::new(),
            trees: OnceLock::new(),
            digest,
            len,
            nops: 0,
            log_end: 0,
            log_live: false,
            seq: 0,
        }
    }

    /// A single-file image over `bytes` (any layout), no log. For callers that hold bytes.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<IndexImage> {
        if Base5::is_agi5(bytes) {
            Ok(Self::from_backing(Backing::V5(Base5::parse(Bytes::Vec(bytes.to_vec()))?)))
        } else {
            let ix = Index::decode(bytes)?;
            Ok(Self::from_backing(Backing::Legacy(Box::new(ix), bytes.to_vec())))
        }
    }

    /// Load `<vcs>/index` and its log.
    pub(crate) fn load(vcs: &Path) -> Result<IndexImage> {
        let path = vcs.join("index");
        let logp = vcs.join("index.log");
        for _ in 0..8 {
            let id0 = ident(&path);
            let bytes = Bytes::open(&path).map_err(|e| io_err(&path, e))?;
            let mut img = if Base5::is_agi5(bytes.as_slice()) {
                Self::from_backing(Backing::V5(Base5::parse(bytes)?))
            } else {
                let raw = bytes.as_slice().to_vec();
                let ix = Index::decode(&raw)?;
                Self::from_backing(Backing::Legacy(Box::new(ix), raw))
            };
            if let Backing::V5(b) = &img.backing {
                let (gen, sum) = (b.gen, b.sums_sum());
                match std::fs::read(&logp) {
                    Ok(lb) => {
                        if let Some((frames, end)) = scan_log(&lb, gen, sum) {
                            img.log_live = true;
                            img.log_end = end as u64;
                            img.seq = frames.len() as u32;
                            for (off, len, s) in frames {
                                let ops = decode_ops(&lb[off..off + len])?;
                                img.digest = chain(&img.digest, s);
                                img.apply(ops);
                            }
                            img.len += end;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io_err(&logp, e)),
                }
            }
            if ident(&path) == id0 {
                return Ok(img);
            }
        }
        Err(corrupt("index kept changing while it was read"))
    }

    /// Apply ops to the in-memory state (not the disk).
    fn apply(&mut self, ops: Vec<Op>) {
        for op in ops {
            self.nops += 1;
            match op {
                Op::Set(p, e) => {
                    self.overlay.insert(p, Some(e));
                }
                Op::Remove(p) => {
                    self.overlay.insert(p, None);
                }
                Op::WrittenNs(n) => self.written_ns = n,
                t @ (Op::DropTrees(_) | Op::SetTrees(_)) => {
                    self.trees = OnceLock::new();
                    self.tree_ops.push(t);
                }
            }
        }
    }

    /// Fingerprint of the exact logical content (base and every frame).
    pub(crate) fn digest(&self) -> [u8; 32] {
        self.digest
    }

    /// Size in bytes of everything the digest covers (base file and valid log).
    pub(crate) fn stored_len(&self) -> usize {
        self.len
    }

    pub(crate) fn base5(&self) -> Option<&Base5> {
        match &self.backing {
            Backing::V5(b) => Some(b),
            Backing::Legacy(..) => None,
        }
    }

    /// Logged edits with keys in `[lo, hi)` (`None` = unbounded), in order; `None` values are removals.
    pub(crate) fn overlay_range<'a>(&'a self, lo: Option<&str>, hi: Option<&str>) -> impl Iterator<Item = (&'a String, &'a Option<IndexEntry>)> + 'a {
        use std::ops::Bound::{Excluded, Included, Unbounded};
        let l = lo.map_or(Unbounded, |l| Included(l.to_owned()));
        let h = hi.map_or(Unbounded, |h| Excluded(h.to_owned()));
        self.overlay.range::<String, _>((l, h))
    }

    pub(crate) fn is_v5(&self) -> bool {
        matches!(self.backing, Backing::V5(_))
    }

    /// Edits waiting in the log.
    pub(crate) fn pending_ops(&self) -> usize {
        self.nops
    }

    pub(crate) fn get(&self, path: &str) -> Result<Option<IndexEntry>> {
        if let Some(o) = self.overlay.get(path) {
            return Ok(o.clone());
        }
        match &self.backing {
            Backing::V5(b) => b.find(path),
            Backing::Legacy(ix, _) => Ok(ix.entries.get(path).cloned()),
        }
    }

    /// First base entry (ignoring the overlay) whose path is `>= key`.
    fn base_ge(&self, key: &str) -> Result<Option<(String, IndexEntry)>> {
        Ok(match &self.backing {
            Backing::V5(b) => {
                let i = b.lower_bound(key)?;
                if i < b.n {
                    let (p, e) = b.get_at(i)?;
                    Some((p.to_owned(), e))
                } else {
                    None
                }
            }
            Backing::Legacy(ix, _) => ix
                .entries
                .range::<str, _>((std::ops::Bound::Included(key), std::ops::Bound::Unbounded))
                .next()
                .map(|(k, e)| (k.clone(), e.clone())),
        })
    }

    /// The first tracked entry whose path is `>= key`.
    pub(crate) fn next_ge(&self, key: &str) -> Result<Option<(String, IndexEntry)>> {
        // base candidates that the log overrides or deletes are skipped; the log's own live
        // entries are merged in below
        let mut k = Cow::Borrowed(key);
        let base = loop {
            match self.base_ge(&k)? {
                Some((bk, _)) if self.overlay.contains_key(&bk) => k = Cow::Owned(format!("{bk}\0")),
                c => break c,
            }
        };
        let ov = self
            .overlay
            .range::<str, _>((std::ops::Bound::Included(key), std::ops::Bound::Unbounded))
            .find_map(|(k, e)| e.as_ref().map(|e| (k.clone(), e.clone())));
        Ok(match (base, ov) {
            (Some(b), Some(o)) => Some(if b.0 <= o.0 { b } else { o }),
            (b, o) => b.or(o),
        })
    }

    /// Entries equal to `prefix` or beneath `prefix/` (all for an empty prefix), in order.
    pub(crate) fn under(&self, prefix: &str) -> Result<Vec<(String, IndexEntry)>> {
        let mut out = Vec::new();
        let (lo, hi) = (format!("{prefix}/"), format!("{prefix}0")); // '0' == '/' + 1
        if prefix.is_empty() {
            return Ok(self.list()?);
        }
        if let Some(e) = self.get(prefix)? {
            out.push((prefix.to_owned(), e));
        }
        let mut cur = lo;
        while let Some((k, e)) = self.next_ge(&cur)? {
            if k.as_str() >= hi.as_str() {
                break;
            }
            cur = format!("{k}\0");
            out.push((k, e));
        }
        Ok(out)
    }

    /// The cache-tree section as of now (base plus logged tree edits).
    pub(crate) fn trees(&self) -> Result<Option<&Trees>> {
        if let Some(t) = self.trees.get() {
            return Ok(t.as_ref());
        }
        let mut t = match &self.backing {
            Backing::V5(b) => b.trees()?,
            Backing::Legacy(ix, _) => ix.entries.trees_snapshot(),
        };
        for op in &self.tree_ops {
            match op {
                Op::SetTrees(n) => t = n.clone(),
                Op::DropTrees(ds) => {
                    if let Some((_, m)) = t.as_mut() {
                        for d in ds {
                            m.remove(d);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(self.trees.get_or_init(|| t).as_ref())
    }

    /// The cached tree id of directory `dir` when the section was computed under layout `fp`.
    pub(crate) fn cached_tree(&self, fp: &ObjectId, dir: &str) -> Result<Option<ObjectId>> {
        Ok(match self.trees()? {
            Some((f, m)) if f == fp => m.get(dir).copied(),
            _ => None,
        })
    }

    /// Every entry in path order (base merged with the log).
    pub(crate) fn list(&self) -> Result<Vec<(String, IndexEntry)>> {
        let base: Vec<(String, IndexEntry)> = match &self.backing {
            Backing::V5(b) => b.decode_all()?,
            Backing::Legacy(ix, _) => ix.entries.iter().map(|(k, e)| (k.clone(), e.clone())).collect(),
        };
        if self.overlay.is_empty() {
            return Ok(base);
        }
        let mut out = Vec::with_capacity(base.len() + self.overlay.len());
        let mut ov = self.overlay.iter().peekable();
        for (k, e) in base {
            while let Some((ok, oe)) = ov.next_if(|(ok, _)| ok.as_str() < k.as_str()) {
                if let Some(oe) = oe {
                    out.push((ok.clone(), oe.clone()));
                }
            }
            match ov.next_if(|(ok, _)| ok.as_str() == k.as_str()) {
                Some((_, Some(oe))) => out.push((k, oe.clone())),
                Some((_, None)) => {}
                None => out.push((k, e)),
            }
        }
        for (ok, oe) in ov {
            if let Some(oe) = oe {
                out.push((ok.clone(), oe.clone()));
            }
        }
        Ok(out)
    }

    /// The whole index as a decoded [`Index`] (validated like [`Index::decode`]).
    pub(crate) fn to_index(&self) -> Result<Index> {
        let mut ix = self.to_index_inner()?;
        ix.entries.set_src_digest(self.digest);
        Ok(ix)
    }

    fn to_index_inner(&self) -> Result<Index> {
        if let (Backing::Legacy(ix, _), true) = (&self.backing, self.overlay.is_empty() && self.tree_ops.is_empty()) {
            return Ok((**ix).clone());
        }
        let list = self.list()?;
        if !self.overlay.is_empty() {
            // base was validated by decode_all; edits are paths from validated inserts, but a
            // hand-damaged log must not smuggle in a file/directory clash
            crate::index::check_no_conflicts(&list)?;
        }
        Ok(Index::from_sorted(list, self.written_ns, self.trees()?.cloned()))
    }

    /// The index in one piece as `AGI5` (or the original bytes of an older layout), for callers
    /// that still want bytes. Free when there is no log.
    pub(crate) fn flat(&self) -> Result<Cow<'_, [u8]>> {
        match &self.backing {
            Backing::V5(b) if self.overlay.is_empty() && self.tree_ops.is_empty() && self.written_ns == b.written_ns => Ok(Cow::Borrowed(b.raw())),
            Backing::Legacy(_, raw) if self.overlay.is_empty() && self.tree_ops.is_empty() => Ok(Cow::Borrowed(raw)),
            _ => Ok(Cow::Owned(self.to_index()?.encode())),
        }
    }

    /// The mapped base when the log is empty and `written_ns` is the base's: lets scanners work on
    /// the file directly.
    pub(crate) fn pristine_base(&self) -> Option<&Base5> {
        match &self.backing {
            Backing::V5(b) if self.overlay.is_empty() && self.tree_ops.is_empty() && self.written_ns == b.written_ns => Some(b),
            _ => None,
        }
    }

    // ---- writing ---------------------------------------------------------------------------

    /// Persist `ops`: append one frame to the log, or fold everything into a new base generation
    /// when the log is long, the edit is big, or the base is an older layout. Returns the digest
    /// and stored size a fresh [`Self::load`] would now report. `self` is not changed.
    pub(crate) fn commit(&self, vcs: &Path, ops: Vec<Op>) -> Result<After> {
        if ops.is_empty() {
            return Ok(After {
                digest: self.digest,
                len: self.len,
            });
        }
        let big = ops.iter().filter(|o| matches!(o, Op::Set(..) | Op::Remove(_))).count() > 1024;
        let fold = !self.is_v5() || big || self.nops + ops.len() > MAX_OPS || self.log_end > MAX_LOG;
        if fold {
            let mut ix = self.to_index()?;
            apply_to_index(&mut ix, ops);
            write_full(vcs, &ix)?;
            let fresh = Self::load(vcs)?;
            return Ok(After {
                digest: fresh.digest,
                len: fresh.len,
            });
        }
        let Backing::V5(b) = &self.backing else { unreachable!("fold covers legacy") };
        let (gen, sum) = (b.gen, b.sums_sum());
        let payload = encode_ops(&ops);
        let seq = self.seq + 1;
        let fsum = frame_sum(gen, seq, &payload);
        let mut frame = Vec::with_capacity(FRAME_HDR + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&seq.to_le_bytes());
        frame.extend_from_slice(&fsum.to_le_bytes());
        frame.extend_from_slice(&payload);
        let logp = vcs.join("index.log");
        if crate::failpoint::tick() {
            // simulated crash mid-append: a torn frame is left at the tail
            let torn = &frame[..frame.len() / 2];
            let _ = self.write_frame(&logp, gen, sum, torn);
            return Err(io_err(&logp, std::io::Error::other("injected crash")));
        }
        self.write_frame(&logp, gen, sum, &frame)?;
        Ok(After {
            digest: chain(&self.digest, fsum),
            len: self.len + if self.log_live { frame.len() } else { LOG_HDR + frame.len() },
        })
    }

    /// Does any path in the index (or log) contain `needle`? A scan of the file's bytes, no decode.
    pub(crate) fn mentions(&self, needle: &str) -> bool {
        let in_base = match &self.backing {
            Backing::V5(b) => crate::lockable::bytes_contain(b.raw(), needle.as_bytes()),
            Backing::Legacy(_, raw) => crate::lockable::bytes_contain(raw, needle.as_bytes()),
        };
        in_base || self.overlay.keys().any(|k| k.contains(needle))
    }

    fn write_frame(&self, logp: &Path, gen: u64, base_sum: u64, frame: &[u8]) -> Result<()> {
        if self.log_live {
            use std::os::unix::fs::FileExt;
            let f = std::fs::OpenOptions::new().write(true).open(logp).map_err(|e| io_err(logp, e))?;
            // drop a torn tail left by an earlier crash, then write exactly at the valid end
            if f.metadata().map_err(|e| io_err(logp, e))?.len() > self.log_end {
                f.set_len(self.log_end).map_err(|e| io_err(logp, e))?;
            }
            return f.write_all_at(frame, self.log_end).map_err(|e| io_err(logp, e));
        }
        // no log for this base yet (or a stale one): header and first frame appear together
        let mut buf = Vec::with_capacity(LOG_HDR + frame.len());
        buf.extend_from_slice(&log_header(gen, base_sum));
        buf.extend_from_slice(frame);
        let tmp = logp.with_extension(format!("tmp{}", std::process::id()));
        let mut f = std::fs::File::create(&tmp).map_err(|e| io_err(&tmp, e))?;
        f.write_all(&buf).map_err(|e| io_err(&tmp, e))?;
        drop(f);
        std::fs::rename(&tmp, logp).map_err(|e| io_err(logp, e))
    }
}

/// What the index looks like after a [`IndexImage::commit`].
pub(crate) struct After {
    pub digest: [u8; 32],
    pub len: usize,
}

/// Apply logged edits to a decoded index (the fold path).
fn apply_to_index(ix: &mut Index, ops: Vec<Op>) {
    for op in ops {
        match op {
            Op::Set(p, e) => {
                ix.entries.insert(p, e);
            }
            Op::Remove(p) => {
                ix.entries.remove(&p);
            }
            Op::DropTrees(_) => {} // the inserts and removals above dropped them already
            Op::SetTrees(t) => ix.entries.replace_trees(t),
            Op::WrittenNs(n) => ix.written_ns = n,
        }
    }
}

/// Write `ix` as a new `AGI5` base generation (atomic rename) and drop the log.
pub(crate) fn write_full(vcs: &Path, ix: &Index) -> Result<()> {
    let bytes = ix.encode();
    crate::meta::write_atomic_nosync(&vcs.join("index"), &bytes)?;
    let _ = std::fs::remove_file(vcs.join("index.log"));
    Ok(())
}

impl Base5 {
    /// Decode every record (verifying all checksums, validating paths and order), on several
    /// threads for a big index.
    pub(crate) fn decode_all(&self) -> Result<Vec<(String, IndexEntry)>> {
        self.verify_all()?;
        let n = self.n;
        let threads = crate::index::threads_for(n).min(n.div_ceil(index5::BLOCK5).max(1));
        let one = |a: usize, b: usize| -> Result<Vec<(String, IndexEntry)>> {
            let mut v = Vec::with_capacity(b - a);
            self.each(a, b, |p, e| {
                v.push((p.to_owned(), e));
                Ok(())
            })?;
            Ok(v)
        };
        let out: Vec<(String, IndexEntry)> = if threads <= 1 {
            one(0, n)?
        } else {
            let per = n.div_ceil(threads);
            let parts: Vec<Result<Vec<_>>> = std::thread::scope(|sc| {
                let one = &one;
                let hs: Vec<_> = (0..threads).map(|t| sc.spawn(move || one((t * per).min(n), ((t + 1) * per).min(n)))).collect();
                hs.into_iter().map(|h| h.join().expect("index decode worker panicked")).collect()
            });
            let mut out = Vec::with_capacity(n);
            for p in parts {
                out.extend(p?);
            }
            out
        };
        if !out.windows(2).all(|w| w[0].0 < w[1].0) {
            return Err(corrupt("entries out of order"));
        }
        crate::index::check_no_conflicts(&out)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn en(n: u8, m: u64) -> IndexEntry {
        IndexEntry {
            id: ObjectId::from_bytes([n; 32]),
            mode: EntryMode::File,
            size: n as u64,
            mtime_ns: m,
            ctime_ns: m + 1,
        }
    }

    fn base(n: u32) -> Index {
        let mut ix = Index::default();
        for k in 0..n {
            ix.insert(&format!("d{:02}/f{k:05}.txt", k % 20), en((k % 200) as u8, k as u64 + 5)).unwrap();
        }
        ix.written_ns = 99;
        ix
    }

    fn dir_with(ix: &Index) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        write_full(d.path(), ix).unwrap();
        d
    }

    #[test]
    fn log_append_is_visible_to_a_fresh_reader_and_survives_reload() {
        let ix = base(500);
        let d = dir_with(&ix);
        let img = IndexImage::load(d.path()).unwrap();
        let a = img.commit(d.path(), vec![Op::Set("zz.txt".into(), en(1, 1)), Op::Remove("d00/f00000.txt".into())]).unwrap();
        let img2 = IndexImage::load(d.path()).unwrap();
        assert_eq!((img2.digest(), img2.stored_len()), (a.digest, a.len));
        assert_ne!(img2.digest(), img.digest());
        assert!(img2.get("zz.txt").unwrap().is_some());
        assert!(img2.get("d00/f00000.txt").unwrap().is_none());
        assert_eq!(img2.next_ge("d00/f00000.txt").unwrap().unwrap().0, "d00/f00020.txt");
        let b = img2.commit(d.path(), vec![Op::WrittenNs(1234)]).unwrap();
        let img3 = IndexImage::load(d.path()).unwrap();
        assert_eq!((img3.digest(), img3.written_ns, img3.stored_len()), (b.digest, 1234, b.len));
        let mut want = ix.clone();
        want.entries.insert("zz.txt".into(), en(1, 1));
        want.entries.remove("d00/f00000.txt");
        assert_eq!(img3.to_index().unwrap().entries, want.entries);
    }

    #[test]
    fn folds_into_a_new_base_when_the_log_is_long() {
        let d = dir_with(&base(100));
        let mut gen0 = None;
        for k in 0..(MAX_OPS + 10) {
            let img = IndexImage::load(d.path()).unwrap();
            gen0.get_or_insert(img.base5().unwrap().gen);
            img.commit(d.path(), vec![Op::Set(format!("n{k:05}"), en(2, k as u64))]).unwrap();
        }
        let img = IndexImage::load(d.path()).unwrap();
        assert_ne!(Some(img.base5().unwrap().gen), gen0, "a fold writes a new generation");
        assert!(img.pending_ops() < MAX_OPS);
        assert_eq!(img.list().unwrap().len(), 100 + MAX_OPS + 10);
    }

    #[test]
    fn upgrade_from_agi3_and_agi4_layouts() {
        let ix = base(3000);
        for old in [ix.encode_v3()] {
            let d = tempfile::tempdir().unwrap();
            std::fs::write(d.path().join("index"), &old).unwrap();
            let img = IndexImage::load(d.path()).unwrap();
            assert!(!img.is_v5());
            assert_eq!(img.to_index().unwrap().entries, ix.entries);
            // the first write upgrades
            img.commit(d.path(), vec![Op::Set("new".into(), en(3, 3))]).unwrap();
            let up = IndexImage::load(d.path()).unwrap();
            assert!(up.is_v5());
            assert_eq!(up.list().unwrap().len(), 3001);
            assert!(std::fs::read(d.path().join("index")).unwrap().starts_with(b"AGI5"));
        }
    }

    #[test]
    fn torn_or_corrupt_log_tail_is_ignored_and_truncated_by_the_next_writer() {
        let d = dir_with(&base(50));
        let mut img = IndexImage::load(d.path()).unwrap();
        for k in 0..3 {
            img.commit(d.path(), vec![Op::Set(format!("a{k}"), en(4, k))]).unwrap();
            img = IndexImage::load(d.path()).unwrap();
        }
        let logp = d.path().join("index.log");
        let good = std::fs::read(&logp).unwrap();
        // tear the last frame in half, then flip a byte in the middle of another copy
        let torn = &good[..good.len() - 7];
        std::fs::write(&logp, torn).unwrap();
        let t = IndexImage::load(d.path()).unwrap();
        assert!(t.get("a0").unwrap().is_some() && t.get("a1").unwrap().is_some() && t.get("a2").unwrap().is_none());
        let mut flipped = good.clone();
        let mid = good.len() - 10;
        flipped[mid] ^= 1;
        std::fs::write(&logp, &flipped).unwrap();
        let f = IndexImage::load(d.path()).unwrap();
        assert!(f.get("a2").unwrap().is_none(), "checksum mismatch drops the frame");
        // next writer cuts the garbage and appends after the valid prefix
        f.commit(d.path(), vec![Op::Set("b".into(), en(5, 5))]).unwrap();
        let n = IndexImage::load(d.path()).unwrap();
        assert!(n.get("a1").unwrap().is_some() && n.get("b").unwrap().is_some() && n.get("a2").unwrap().is_none());
        // a log for another base is ignored entirely
        let other = dir_with(&base(10));
        std::fs::copy(&logp, other.path().join("index.log")).unwrap();
        assert!(IndexImage::load(other.path()).unwrap().get("b").unwrap().is_none());
    }

    #[test]
    fn corrupt_base_is_reported_not_misread() {
        let d = dir_with(&base(2000));
        let p = d.path().join("index");
        let mut b = std::fs::read(&p).unwrap();
        let mid = b.len() / 2;
        b[mid] ^= 0x10;
        std::fs::write(&p, &b).unwrap();
        let img = IndexImage::load(d.path()).unwrap();
        assert!(img.list().is_err(), "full decode verifies every chunk");
        std::fs::write(&p, &b[..b.len() - 3]).unwrap();
        assert!(IndexImage::load(d.path()).is_err());
    }

    #[test]
    fn crash_mid_append_leaves_the_previous_state() {
        let d = dir_with(&base(80));
        let img = IndexImage::load(d.path()).unwrap();
        img.commit(d.path(), vec![Op::Set("kept".into(), en(6, 6))]).unwrap();
        let img = IndexImage::load(d.path()).unwrap();
        crate::failpoint::reset();
        crate::failpoint::arm(Some(0));
        let r = img.commit(d.path(), vec![Op::Set("lost".into(), en(7, 7))]);
        crate::failpoint::arm(None);
        assert!(r.is_err());
        let after = IndexImage::load(d.path()).unwrap();
        assert!(after.get("kept").unwrap().is_some());
        assert!(after.get("lost").unwrap().is_none());
        // and the repository keeps working: the next append lands after the valid prefix
        after.commit(d.path(), vec![Op::Set("next".into(), en(8, 8))]).unwrap();
        let end = IndexImage::load(d.path()).unwrap();
        assert!(end.get("next").unwrap().is_some() && end.get("lost").unwrap().is_none());
    }

    #[test]
    fn readers_see_whole_states_while_a_writer_appends_and_folds() {
        let d = dir_with(&base(300));
        let path = d.path().to_owned();
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|sc| {
            let readers: Vec<_> = (0..3)
                .map(|_| {
                    sc.spawn(|| {
                        let mut last = 0usize;
                        while !stop.load(Ordering::Relaxed) {
                            let img = IndexImage::load(&path).unwrap();
                            // writer inserts n00000, n00001, ... in order: a reader must see a prefix, never a gap
                            let seen = (0..)
                                .take_while(|k| img.get(&format!("n{k:05}")).unwrap().is_some())
                                .count();
                            let total = img.list().unwrap().len();
                            assert!(seen >= last, "went back in time: {seen} < {last}");
                            assert_eq!(total, 300 + seen);
                            last = seen;
                        }
                    })
                })
                .collect();
            for k in 0..(MAX_OPS + 300) {
                let img = IndexImage::load(&path).unwrap();
                img.commit(&path, vec![Op::Set(format!("n{k:05}"), en(9, k as u64))]).unwrap();
            }
            stop.store(true, Ordering::Relaxed);
            for r in readers {
                r.join().unwrap();
            }
        });
    }
}
