//! Pack files: many encoded objects in one file plus a sorted index. Format in docs/object-model.md.
//!
//! Two on-disk versions exist. v1 (`VCSPACK1`/`VCSPIDX1`) stores encoded objects as they are; v2
//! (`VCSPACK2`/`VCSPIDX2`) prefixes every entry with a codec byte (raw or zstd level 3) and adds a
//! 256-entry fanout table to the index. Readers accept all versions forever; writers emit v2 unless told otherwise.
//!
//! A fourth version, v4 (`VCSPACK4`/`VCSPIDX4`), is v2 plus codec 2: a delta against an earlier
//! entry of the same pack (see `delta.rs` and docs/research/storage/delta-gate.md). Opt-in
//! (`VCS_PACK_FORMAT=v4`, `vcs repack --format v4`), local packs only, per-object entries like
//! v2 (random reads stay cheap, unlike v3 blocks); `vcs repack --format v2` rewrites it back.
//!
//! A third version, v3 (`VCSPACK3`/`VCSPIDX3`), is written only by compaction (remote
//! `vcs gc --remote --compact`, or `vcs repack --format v3`). Its pack body is a sequence of
//! independently compressed *blocks* (about [`BLOCK_TARGET`] of uncompressed objects each) in the
//! order the compactor chose (commits and trees first, then file content grouped by path), so
//! related versions of a file share a compression window. The index is the v2 index (same entry
//! size, fanout, ordering) except that an entry's `(offset, len)` is the BLOCK holding the object:
//! many ids share one. A block frame is `[u32 len][codec byte][body]`; the decoded body is
//! `[u32 n][n x (32-byte id, u32 len)][object bytes in table order]`. Pack header count = number
//! of blocks. Older binaries refuse a v3 index ("bad index header") instead of misreading it.

use crate::{io, StoreError};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use vcs_objects::ObjectId;

pub const PACK_MAGIC: &[u8; 8] = b"VCSPACK1";
pub const IDX_MAGIC: &[u8; 8] = b"VCSPIDX1";
pub const PACK_MAGIC_V2: &[u8; 8] = b"VCSPACK2";
pub const IDX_MAGIC_V2: &[u8; 8] = b"VCSPIDX2";
/// v2 entry codecs (first byte of every entry payload).
pub const CODEC_RAW: u8 = 0;
pub const CODEC_ZSTD: u8 = 1;
pub const PACK_MAGIC_V4: &[u8; 8] = b"VCSPACK4";
pub const IDX_MAGIC_V4: &[u8; 8] = b"VCSPIDX4";
/// v4 only: `[2][varint distance][zstd(delta)]`; the base entry starts `distance` bytes before this one.
pub const CODEC_DELTA: u8 = 2;
/// Longest delta chain a reader follows (a hostile pack cannot make reads arbitrarily slow).
pub const MAX_READ_DEPTH: usize = 50;
/// Longest chain the writer builds (the gate in docs/research/storage/delta-gate.md allows <= 8).
pub const MAX_WRITE_DEPTH: u32 = 8;
const DELTA_WINDOW: usize = 8;
pub const ZSTD_LEVEL: i32 = 3;
pub const PACK_MAGIC_V3: &[u8; 8] = b"VCSPACK3";
pub const IDX_MAGIC_V3: &[u8; 8] = b"VCSPIDX3";
/// v3 block codecs (first byte of a block payload): zstd or raw body, and whether the block holds
/// only commits and trees (metadata), which lets a reader stream the metadata prefix of a pack
/// without decompressing file content.
pub const CODEC_BLOCK_ZSTD_META: u8 = 2;
pub const CODEC_BLOCK_ZSTD_DATA: u8 = 3;
pub const CODEC_BLOCK_RAW_META: u8 = 4;
pub const CODEC_BLOCK_RAW_DATA: u8 = 5;
/// Uncompressed size a v3 block is filled to (research: 1 to 4 MiB for text).
pub const BLOCK_TARGET: usize = 4 * 1024 * 1024;
/// Upper bound on a decoded v3 block (a lying frame header cannot demand more).
const MAX_BLOCK: u64 = 256 * 1024 * 1024;
/// Upper bound on one decoded object, so a lying zstd frame header cannot demand more.
const MAX_DECODED: u64 = u32::MAX as u64;
const FANOUT_BYTES: usize = 256 * 4;
const HEADER: u64 = 12; // magic + u32 count
const IDX_ENTRY: usize = 32 + 8 + 4;

fn bad(path: &Path, why: impl Into<String>) -> StoreError {
    StoreError::Io {
        path: path.to_owned(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, why.into()),
    }
}

/// Which on-disk format to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PackFormat {
    V1,
    #[default]
    V2,
    /// Independently compressed blocks; written by compaction only (see the module docs).
    V3,
    /// v2 entries plus delta entries (codec 2); local only, opt-in (see the module docs).
    V4,
}

impl PackFormat {
    /// The format for new packs: v2 unless `VCS_PACK_FORMAT=v1` (rollback switch).
    pub fn from_env() -> PackFormat {
        match std::env::var("VCS_PACK_FORMAT").as_deref() {
            Ok("v1") | Ok("1") => PackFormat::V1,
            Ok("v3") | Ok("3") => PackFormat::V3,
            Ok("v4") | Ok("4") => PackFormat::V4,
            _ => PackFormat::V2,
        }
    }
    /// The format for compaction output: v3 (blocks) unless `VCS_PACK_FORMAT` names another.
    /// `VCS_PACK_FORMAT=v2` (or v1) is the rollback: compaction still merges and orders, but
    /// writes per-object entries every released binary reads.
    pub fn for_compaction() -> PackFormat {
        match std::env::var("VCS_PACK_FORMAT").as_deref() {
            Ok("v1") | Ok("1") => PackFormat::V1,
            Ok("v2") | Ok("2") => PackFormat::V2,
            _ => PackFormat::V3,
        }
    }
    pub fn version(self) -> u8 {
        match self {
            PackFormat::V1 => 1,
            PackFormat::V2 => 2,
            PackFormat::V3 => 3,
            PackFormat::V4 => 4,
        }
    }
    pub fn parse(s: &str) -> Option<PackFormat> {
        match s {
            "v1" | "1" => Some(PackFormat::V1),
            "v2" | "2" => Some(PackFormat::V2),
            "v3" | "3" => Some(PackFormat::V3),
            "v4" | "4" => Some(PackFormat::V4),
            _ => None,
        }
    }
}

/// Turn an encoded object into a pack entry payload. v1: the bytes as they are. v2: a codec byte
/// then zstd level 3 output, or the raw bytes when compression does not make them smaller (so an
/// entry never grows by more than the one codec byte).
pub fn encode_payload(fmt: PackFormat, enc: &[u8]) -> Vec<u8> {
    match fmt {
        PackFormat::V1 => enc.to_vec(),
        // v3 stores blocks, not entries; a single object is sized as a v2 entry
        PackFormat::V2 | PackFormat::V3 | PackFormat::V4 => {
            if let Ok(c) = zstd::bulk::compress(enc, ZSTD_LEVEL) {
                if c.len() < enc.len() {
                    let mut out = Vec::with_capacity(c.len() + 1);
                    out.push(CODEC_ZSTD);
                    out.extend_from_slice(&c);
                    return out;
                }
            }
            let mut out = Vec::with_capacity(enc.len() + 1);
            out.push(CODEC_RAW);
            out.extend_from_slice(enc);
            out
        }
    }
}

/// Recover the encoded object from an entry payload of a pack of `version`. The caller still
/// checks that the result hashes to the expected id.
pub fn decode_payload(version: u8, payload: &[u8]) -> Result<std::borrow::Cow<'_, [u8]>, String> {
    decode_payload_max(version, payload, MAX_DECODED)
}

/// [`decode_payload`] with a caller-chosen cap on the decoded size (for untrusted input).
pub fn decode_payload_max(version: u8, payload: &[u8], max: u64) -> Result<std::borrow::Cow<'_, [u8]>, String> {
    use std::borrow::Cow;
    if version == 1 {
        return Ok(Cow::Borrowed(payload));
    }
    if version == 3 {
        return Err("a v3 entry is a block: use PackIndex::decode_entry".into());
    }
    match payload.split_first() {
        None => Err("empty entry".into()),
        Some((&CODEC_RAW, rest)) => Ok(Cow::Borrowed(rest)),
        Some((&CODEC_ZSTD, rest)) => {
            let n = zstd::zstd_safe::get_frame_content_size(rest)
                .map_err(|_| "bad zstd frame header".to_string())?
                .ok_or_else(|| "zstd frame without content size".to_string())?;
            if n > max.min(MAX_DECODED) {
                return Err("zstd frame claims an oversized object".into());
            }
            // One decompression context per thread: creating one per object is a visible cost
            // when a checkout reads 100k small blobs.
            thread_local! {
                static DCTX: std::cell::RefCell<Option<zstd::bulk::Decompressor<'static>>> = const { std::cell::RefCell::new(None) };
            }
            let out = DCTX
                .with(|c| {
                    let mut c = c.borrow_mut();
                    if c.is_none() {
                        *c = zstd::bulk::Decompressor::new().ok();
                    }
                    match c.as_mut() {
                        Some(d) => d.decompress(rest, n as usize),
                        None => zstd::bulk::decompress(rest, n as usize),
                    }
                })
                .map_err(|e| format!("zstd: {e}"))?;
            if out.len() as u64 != n {
                return Err("zstd frame length mismatch".into());
            }
            Ok(Cow::Owned(out))
        }
        Some((&CODEC_DELTA, _)) if version == 4 => Err("delta entry needs its pack to resolve".into()),
        Some((c, _)) => Err(format!("unknown codec {c}")),
    }
}
/// Parse a delta entry payload into `(distance to base entry, delta bytes)`.
fn parse_delta(payload: &[u8], max_delta: u64) -> Result<(u64, Vec<u8>), String> {
    let rest = payload.strip_prefix(&[CODEC_DELTA]).ok_or("not a delta entry")?;
    let (mut dist, mut sh, mut i) = (0u64, 0u32, 0usize);
    loop {
        let b = *rest.get(i).ok_or("truncated delta entry")?;
        i += 1;
        if sh > 56 {
            return Err("delta distance too long".into());
        }
        dist |= ((b & 0x7f) as u64) << sh;
        if b & 0x80 == 0 {
            break;
        }
        sh += 7;
    }
    if dist == 0 {
        return Err("delta entry names itself as base".into());
    }
    let z = &rest[i..];
    let n = zstd::zstd_safe::get_frame_content_size(z)
        .map_err(|_| "bad zstd frame header".to_string())?
        .ok_or_else(|| "zstd frame without content size".to_string())?;
    if n > max_delta {
        return Err("delta claims an oversized body".into());
    }
    let d = zstd::bulk::decompress(z, n as usize).map_err(|e| format!("zstd: {e}"))?;
    Ok((dist, d))
}

/// Resolve the entry at `off` (payload `first`) of a v4 pack. `fetch(off)` returns the payload of
/// the entry that starts at `off`. Bases always precede their delta (distance > 0), so a chain
/// cannot loop; its length is capped by [`MAX_READ_DEPTH`] and every intermediate size by `max`.
fn resolve_v4(mut off: u64, first: Vec<u8>, max: u64, fetch: &mut dyn FnMut(u64) -> Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
    let mut deltas: Vec<Vec<u8>> = Vec::new();
    let mut payload = first;
    while payload.first() == Some(&CODEC_DELTA) {
        if deltas.len() >= MAX_READ_DEPTH {
            return Err("delta chain too deep".into());
        }
        let (dist, d) = parse_delta(&payload, max.saturating_mul(2).saturating_add(1 << 16))?;
        off = off
            .checked_sub(dist)
            .filter(|o| *o >= HEADER + 4)
            .ok_or("delta base outside pack")?;
        deltas.push(d);
        payload = fetch(off)?;
    }
    let mut cur = decode_payload_max(4, &payload, max)?.into_owned();
    for d in deltas.iter().rev() {
        cur = crate::delta::apply(&cur, d, max as usize).map_err(|e| format!("delta: {e}"))?;
    }
    Ok(cur)
}

/// A parsed pack index (also usable for a pack that lives on a remote). The raw bytes are kept
/// and entries are read in place, so opening costs no per-entry work. [`PackIndex::parse`]
/// verifies everything; [`PackIndex::from_vec_lazy`] (local open) checks only structure and leaves
/// the BLAKE3 trailer, ordering and fanout consistency to [`PackIndex::verify_full`] (fsck).
pub struct PackIndex {
    src: Src,
    version: u8,
    count: usize,
    base: usize,
    pub pack_len: u64,
    checksum: [u8; 32],
    fanout: Option<Box<[u32; 256]>>,
}

/// Where the index bytes live: in memory, or in the file, read on demand (a local open of a big
/// pack must not read tens of megabytes before the first lookup).
enum Src {
    Mem(Vec<u8>),
    File {
        file: fs::File,
        len: u64,
        /// Entries of one first-byte bucket, read the first time a lookup lands in it.
        buckets: std::sync::Mutex<Vec<Option<std::sync::Arc<Vec<u8>>>>>,
    },
}

#[cfg(unix)]
fn pread_exact(f: &fs::File, off: u64, n: usize) -> std::io::Result<Vec<u8>> {
    use std::os::unix::fs::FileExt;
    let mut v = vec![0u8; n];
    f.read_exact_at(&mut v, off)?;
    Ok(v)
}

fn entry_at(b: &[u8], base: usize, i: usize) -> (ObjectId, u64, u32) {
    let c = &b[base + i * IDX_ENTRY..base + (i + 1) * IDX_ENTRY];
    (
        ObjectId::from_bytes(c[..32].try_into().unwrap()),
        u64::from_le_bytes(c[32..40].try_into().unwrap()),
        u32::from_le_bytes(c[40..44].try_into().unwrap()),
    )
}

impl PackIndex {
    /// Parse and fully verify `.idx` bytes; `label` is only used in error messages.
    pub fn parse(b: &[u8], label: &Path) -> Result<PackIndex, StoreError> {
        let idx = Self::from_vec_lazy(b.to_vec(), label)?;
        idx.verify_full().map_err(|e| bad(label, e))?;
        Ok(idx)
    }
    /// Structure-only parse (header, length, fanout shape). v1 indexes are always fully verified
    /// because their binary search has no other guard.
    pub fn from_vec_lazy(bytes: Vec<u8>, label: &Path) -> Result<PackIndex, StoreError> {
        let (version, count, pack_len, checksum, fanout, base) = Self::parse_header(&bytes, bytes.len() as u64, label)?;
        let idx = PackIndex {
            src: Src::Mem(bytes),
            version,
            count,
            base,
            pack_len,
            checksum,
            fanout,
        };
        if version == 1 {
            idx.verify_full().map_err(|e| bad(label, e))?;
        }
        Ok(idx)
    }

    /// Like [`PackIndex::from_vec_lazy`] for an index file: only the header and fanout table are read
    /// now; entries are read per first-byte bucket on first use. Same structure checks (the file length
    /// must match the entry count); v1 indexes and non-unix platforms read the whole file (v1 is
    /// verified in full at open).
    pub fn open_lazy(path: &Path) -> Result<PackIndex, StoreError> {
        #[cfg(unix)]
        {
            let f = fs::File::open(path).map_err(|source| StoreError::Io { path: path.to_owned(), source })?;
            let len = f.metadata().map_err(|source| StoreError::Io { path: path.to_owned(), source })?.len();
            let head_n = ((8 + 4 + 8 + 32 + FANOUT_BYTES) as u64).min(len);
            // v1 has no fanout and is always fully verified at open (see `from_vec_lazy`): read whole
            let v2_plus = head_n >= 8 && pread_exact(&f, 0, 8).is_ok_and(|m| m != IDX_MAGIC[..]);
            if v2_plus && head_n >= 8 + 4 + 8 + 32 {
                let head = pread_exact(&f, 0, head_n as usize).map_err(|source| StoreError::Io { path: path.to_owned(), source })?;
                let (version, count, pack_len, checksum, fanout, base) = Self::parse_header(&head, len, path)?;
                return Ok(PackIndex {
                    src: Src::File {
                        file: f,
                        len,
                        buckets: std::sync::Mutex::new(vec![None; 256]),
                    },
                    version,
                    count,
                    base,
                    pack_len,
                    checksum,
                    fanout,
                });
            }
        }
        let b = fs::read(path).map_err(|source| StoreError::Io { path: path.to_owned(), source })?;
        Self::from_vec_lazy(b, path)
    }

    /// Header fields from the first bytes of an index whose whole length is `total`.
    #[allow(clippy::type_complexity)]
    fn parse_header(b: &[u8], total: u64, label: &Path) -> Result<(u8, usize, u64, [u8; 32], Option<Box<[u32; 256]>>, usize), StoreError> {
        let version = match b.get(..8) {
            Some(m) if m == IDX_MAGIC => 1u8,
            Some(m) if m == IDX_MAGIC_V2 => 2u8,
            Some(m) if m == IDX_MAGIC_V3 => 3u8,
            Some(m) if m == IDX_MAGIC_V4 => 4u8,
            _ => return Err(bad(label, "bad index header")),
        };
        let fan = if version >= 2 { FANOUT_BYTES } else { 0 };
        let base = 8 + 4 + 8 + 32 + fan;
        if (total as usize) < base + 32 || b.len() < base {
            return Err(bad(label, "bad index header"));
        }
        let count = u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize;
        let pack_len = u64::from_le_bytes(b[12..20].try_into().unwrap());
        let checksum: [u8; 32] = b[20..52].try_into().unwrap();
        if count.checked_mul(IDX_ENTRY).map(|n| n as u64) != Some(total - base as u64 - 32) {
            return Err(bad(label, "index length does not match entry count"));
        }
        let fanout = if version >= 2 {
            let mut f = Box::new([0u32; 256]);
            for (i, v) in f.iter_mut().enumerate() {
                *v = u32::from_le_bytes(b[52 + i * 4..56 + i * 4].try_into().unwrap());
            }
            if f.windows(2).any(|w| w[0] > w[1]) || f[255] as usize != count {
                return Err(bad(label, "index fanout table is inconsistent"));
            }
            Some(f)
        } else {
            None
        };
        Ok((version, count, pack_len, checksum, fanout, base))
    }

    /// The whole index file (memory, or read from disk).
    fn all_bytes(&self) -> Result<std::borrow::Cow<'_, [u8]>, String> {
        match &self.src {
            Src::Mem(b) => Ok(std::borrow::Cow::Borrowed(b)),
            #[cfg(unix)]
            Src::File { file, len, .. } => pread_exact(file, 0, *len as usize).map(std::borrow::Cow::Owned).map_err(|e| format!("index read: {e}")),
            #[cfg(not(unix))]
            Src::File { .. } => unreachable!("file-backed indexes exist on unix only"),
        }
    }

    /// Everything the lazy open skips: BLAKE3 trailer, strict ordering, every entry inside the
    /// pack, fanout agreeing with the ids.
    pub fn verify_full(&self) -> Result<(), String> {
        let all = self.all_bytes()?;
        let (body, trailer) = all.split_at(all.len() - 32);
        if blake3::hash(body).as_bytes() != trailer {
            return Err("index checksum mismatch".into());
        }
        let mut counts = [0u32; 256];
        let mut prev: Option<ObjectId> = None;
        for i in 0..self.count {
            let (id, off, len) = entry_at(&all, self.base, i);
            self.check_entry(off, len).map_err(|_| "index entry outside pack".to_string())?;
            if prev.is_some_and(|p| p >= id) {
                return Err("index not strictly sorted".into());
            }
            prev = Some(id);
            counts[id.as_bytes()[0] as usize] += 1;
        }
        if let Some(f) = &self.fanout {
            let mut run = 0u32;
            for (i, c) in counts.iter().enumerate() {
                run += c;
                if f[i] != run {
                    return Err("index fanout table does not match its ids".into());
                }
            }
        }
        Ok(())
    }
    fn check_entry(&self, off: u64, len: u32) -> Result<(), ()> {
        // checked: `off` is attacker-controlled and `off + len` could wrap
        let end_ok = off
            .checked_add(len as u64)
            .is_some_and(|end| end <= self.pack_len.saturating_sub(32));
        if off < HEADER + 4 || !end_ok || (self.version >= 2 && len == 0) {
            return Err(());
        }
        Ok(())
    }
    /// 1, 2 or 3: which pack format this index belongs to.
    pub fn version(&self) -> u8 {
        self.version
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    /// All entries as `(id, offset, len)`, sorted by id. `len` covers the whole entry payload
    /// (for v2 including the codec byte).
    pub fn iter(&self) -> impl Iterator<Item = (ObjectId, u64, u32)> + '_ {
        let all = self.all_bytes().map(|c| c.into_owned()).unwrap_or_default();
        let (base, count) = (self.base, if all.is_empty() { 0 } else { self.count });
        (0..count).map(move |i| entry_at(&all, base, i))
    }
    /// The pack's whole-file checksum, which is also the pack's last 32 bytes (its trailer).
    pub fn checksum(&self) -> &[u8; 32] {
        &self.checksum
    }
    /// The largest end offset (`offset + len`) among those of `ids` that this pack holds;
    /// `None` if it holds none of them.
    pub fn known_end(&self, ids: &[ObjectId]) -> Option<u64> {
        ids.iter().filter_map(|id| self.find(id)).map(|(o, l)| o + l as u64).max()
    }
    /// Bounds-checked lookup: one fanout read, then a binary search inside the id's bucket.
    pub fn lookup(&self, id: &ObjectId) -> Result<Option<(u64, u32)>, String> {
        let (lo, hi) = match &self.fanout {
            Some(f) => {
                let first = id.as_bytes()[0] as usize;
                (if first == 0 { 0 } else { f[first - 1] as usize }, f[first] as usize)
            }
            None => (0, self.count),
        };
        match &self.src {
            Src::Mem(b) => self.search(&b[self.base + lo * IDX_ENTRY..self.base + hi * IDX_ENTRY], id),
            #[cfg(unix)]
            Src::File { file, buckets, .. } => {
                let first = id.as_bytes()[0] as usize;
                let cached = buckets.lock().unwrap()[first].clone();
                let ents = match cached {
                    Some(e) => e,
                    None => {
                        let e = std::sync::Arc::new(
                            pread_exact(file, (self.base + lo * IDX_ENTRY) as u64, (hi - lo) * IDX_ENTRY).map_err(|e| format!("index read: {e}"))?,
                        );
                        buckets.lock().unwrap()[first] = Some(e.clone());
                        e
                    }
                };
                self.search(&ents, id)
            }
            #[cfg(not(unix))]
            Src::File { .. } => unreachable!("file-backed indexes exist on unix only"),
        }
    }
    /// Binary search of `id` in sorted entries `ents` (a whole bucket or table).
    fn search(&self, ents: &[u8], id: &ObjectId) -> Result<Option<(u64, u32)>, String> {
        let (mut lo, mut hi) = (0, ents.len() / IDX_ENTRY);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (eid, off, len) = entry_at(ents, 0, mid);
            match eid.cmp(id) {
                std::cmp::Ordering::Equal => {
                    self.check_entry(off, len)
                        .map_err(|_| format!("index entry for {id} outside pack"))?;
                    return Ok(Some((off, len)));
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        Ok(None)
    }
    /// `(offset, len)` of the object's entry payload within the pack file (an invalid entry
    /// counts as absent; use [`PackIndex::lookup`] to see the error).
    pub fn find(&self, id: &ObjectId) -> Option<(u64, u32)> {
        self.lookup(id).ok().flatten()
    }
}

/// A decoded v3 block: the objects it holds, by id.
pub struct Block {
    data: Vec<u8>,
    ents: std::collections::HashMap<ObjectId, (usize, usize)>,
    order: Vec<ObjectId>,
}

impl Block {
    /// The encoded bytes of `id`, if this block holds it.
    pub fn get(&self, id: &ObjectId) -> Option<&[u8]> {
        self.ents.get(id).map(|&(o, l)| &self.data[o..o + l])
    }
    /// Every `(table id, encoded bytes)` in storage order. The table id is the pack's claim;
    /// callers that trust it must hash the bytes.
    pub fn iter(&self) -> impl Iterator<Item = (ObjectId, &[u8])> + '_ {
        self.order.iter().map(|id| {
            let (o, l) = self.ents[id];
            (*id, &self.data[o..o + l])
        })
    }
    pub fn len(&self) -> usize {
        self.order.len()
    }
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

/// True if a v3 block payload holds only commits and trees (read from its codec byte).
pub fn block_is_metadata(payload: &[u8]) -> bool {
    matches!(payload.first(), Some(&CODEC_BLOCK_ZSTD_META) | Some(&CODEC_BLOCK_RAW_META))
}

/// Decode a v3 block payload (`[codec][body]`, without the u32 frame length).
pub fn decode_block(payload: &[u8]) -> Result<Block, String> {
    let Some((&codec, body)) = payload.split_first() else {
        return Err("empty block".into());
    };
    let data: Vec<u8> = match codec {
        CODEC_BLOCK_ZSTD_META | CODEC_BLOCK_ZSTD_DATA => {
            let n = zstd::zstd_safe::get_frame_content_size(body)
                .map_err(|_| "bad zstd frame header".to_string())?
                .ok_or_else(|| "zstd frame without content size".to_string())?;
            if n > MAX_BLOCK {
                return Err("zstd frame claims an oversized block".into());
            }
            let out = zstd::bulk::decompress(body, n as usize).map_err(|e| format!("zstd: {e}"))?;
            if out.len() as u64 != n {
                return Err("zstd frame length mismatch".into());
            }
            out
        }
        CODEC_BLOCK_RAW_META | CODEC_BLOCK_RAW_DATA => body.to_vec(),
        c => return Err(format!("unknown block codec {c}")),
    };
    let count = data
        .get(..4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
        .ok_or("block too short")?;
    let table_end = count
        .checked_mul(36)
        .and_then(|t| t.checked_add(4))
        .filter(|e| *e <= data.len())
        .ok_or("block table does not fit")?;
    let mut ents = std::collections::HashMap::with_capacity(count);
    let mut order = Vec::with_capacity(count);
    let mut off = table_end;
    for i in 0..count {
        let t = 4 + i * 36;
        let id = ObjectId::from_bytes(data[t..t + 32].try_into().unwrap());
        let len = u32::from_le_bytes(data[t + 32..t + 36].try_into().unwrap()) as usize;
        let end = off
            .checked_add(len)
            .filter(|e| *e <= data.len())
            .ok_or("block entry outside block")?;
        if ents.insert(id, (off, len)).is_some() {
            return Err("duplicate id in block".into());
        }
        order.push(id);
        off = end;
    }
    if off != data.len() {
        return Err("block has trailing bytes".into());
    }
    Ok(Block { data, ents, order })
}

/// One-block memory for readers that decode many objects of the same block in a row.
#[derive(Default)]
pub struct BlockCache(Option<(u64, Block)>);

fn is_meta_kind(enc: &[u8]) -> bool {
    matches!(enc.first(), Some(2 | 3))
}

/// Encode one v3 block from `objs` (all metadata, or all not).
fn encode_block(objs: &[&(ObjectId, Vec<u8>)]) -> Vec<u8> {
    let meta = objs.first().is_some_and(|o| is_meta_kind(&o.1));
    let total: usize = objs.iter().map(|o| o.1.len()).sum();
    let mut raw = Vec::with_capacity(4 + objs.len() * 36 + total);
    raw.extend_from_slice(&(objs.len() as u32).to_le_bytes());
    for o in objs {
        raw.extend_from_slice(o.0.as_bytes());
        raw.extend_from_slice(&(o.1.len() as u32).to_le_bytes());
    }
    for o in objs {
        raw.extend_from_slice(&o.1);
    }
    if let Ok(c) = zstd::bulk::compress(&raw, ZSTD_LEVEL) {
        if c.len() < raw.len() {
            let mut out = Vec::with_capacity(c.len() + 1);
            out.push(if meta { CODEC_BLOCK_ZSTD_META } else { CODEC_BLOCK_ZSTD_DATA });
            out.extend_from_slice(&c);
            return out;
        }
    }
    let mut out = Vec::with_capacity(raw.len() + 1);
    out.push(if meta { CODEC_BLOCK_RAW_META } else { CODEC_BLOCK_RAW_DATA });
    out.extend_from_slice(&raw);
    out
}

/// Split `objects` (already in layout order) into blocks: a block never mixes metadata with file
/// content, and is closed once adding the next object would pass `target` bytes.
fn split_blocks(objects: &[(ObjectId, Vec<u8>)], target: usize) -> Vec<Vec<&(ObjectId, Vec<u8>)>> {
    let mut blocks: Vec<Vec<&(ObjectId, Vec<u8>)>> = Vec::new();
    let (mut cur, mut size, mut meta) = (Vec::new(), 0usize, true);
    for o in objects {
        let m = is_meta_kind(&o.1);
        if !cur.is_empty() && (m != meta || size + o.1.len() > target) {
            blocks.push(std::mem::take(&mut cur));
            size = 0;
        }
        meta = m;
        size += o.1.len();
        cur.push(o);
    }
    if !cur.is_empty() {
        blocks.push(cur);
    }
    blocks
}

impl PackIndex {
    /// Recover the encoded object `id` from the byte range [`PackIndex::find`] returned for it.
    /// v1/v2: the entry itself. v3: the whole block is decompressed and the object cut out.
    pub fn decode_entry<'a>(&self, id: &ObjectId, payload: &'a [u8]) -> Result<std::borrow::Cow<'a, [u8]>, String> {
        if self.version != 3 {
            return decode_payload(self.version, payload);
        }
        let blk = decode_block(payload)?;
        blk.get(id)
            .map(|b| std::borrow::Cow::Owned(b.to_vec()))
            .ok_or_else(|| format!("{id} is not in its block"))
    }
    /// [`PackIndex::decode_entry`] remembering the last block decoded (`off` is the block's pack
    /// offset), for loops over objects sorted by offset.
    pub fn decode_entry_cached<'a>(
        &self,
        id: &ObjectId,
        off: u64,
        payload: &'a [u8],
        cache: &mut BlockCache,
    ) -> Result<std::borrow::Cow<'a, [u8]>, String> {
        if self.version != 3 {
            return decode_payload(self.version, payload);
        }
        if cache.0.as_ref().map(|c| c.0) != Some(off) {
            cache.0 = Some((off, decode_block(payload)?));
        }
        let blk = &cache.0.as_ref().expect("just set").1;
        blk.get(id)
            .map(|b| std::borrow::Cow::Owned(b.to_vec()))
            .ok_or_else(|| format!("{id} is not in its block"))
    }
}

/// One opened pack: its index lives in memory, object bytes are read by range.
pub struct Pack {
    pub pack_path: PathBuf,
    idx: PackIndex,
    /// v3: the block read last, so reading neighbours of one file does not decompress it again.
    block: std::sync::Mutex<Option<(u64, std::sync::Arc<Block>)>>,
    /// Opened on first read and kept: a read is then one `pread`, not open + seek + read + close.
    /// A pack replaced by GC after this open stays readable through the old inode, whose entries
    /// are exactly what `idx` describes.
    #[cfg(unix)]
    file: std::sync::OnceLock<fs::File>,
}

impl Pack {
    /// Open `<name>.idx` (and check the sibling `.pack` exists with the recorded length).
    /// Cheap: hashes neither the pack nor (v2) the index; [`Pack::verify`] does both.
    pub fn open(idx_path: &Path) -> Result<Pack, StoreError> {
        let idx = PackIndex::open_lazy(idx_path)?;
        let pack_path = idx_path.with_extension("pack");
        let actual = io(&pack_path, fs::metadata(&pack_path))?.len();
        if actual != idx.pack_len {
            return Err(bad(
                &pack_path,
                format!("pack is {actual} bytes, index says {} (truncated?)", idx.pack_len),
            ));
        }
        Ok(Pack {
            pack_path,
            idx,
            block: std::sync::Mutex::new(None),
            #[cfg(unix)]
            file: std::sync::OnceLock::new(),
        })
    }

    pub fn version(&self) -> u8 {
        self.idx.version
    }
    pub fn ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.idx.iter().map(|e| e.0)
    }
    pub fn contains(&self, id: &ObjectId) -> bool {
        matches!(self.idx.lookup(id), Ok(Some(_)))
    }
    /// `len` bytes at `off` of the pack file; `None` when the file is shorter (truncated).
    /// `len` comes from the index, so it is never allocated before it is known to lie inside the
    /// pack (a lying index on a short pack cannot demand gigabytes).
    fn read_range(&self, off: u64, len: u64) -> Result<Option<Vec<u8>>, StoreError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            if off.checked_add(len).is_none_or(|end| end > self.idx.pack_len) {
                return Ok(None);
            }
            let f = match self.file.get() {
                Some(f) => f,
                None => {
                    let f = io(&self.pack_path, fs::File::open(&self.pack_path))?;
                    self.file.get_or_init(|| f)
                }
            };
            let mut buf = vec![0u8; len as usize];
            Ok(f.read_exact_at(&mut buf, off).ok().map(|()| buf))
        }
        #[cfg(not(unix))]
        {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = io(&self.pack_path, fs::File::open(&self.pack_path))?;
            io(&self.pack_path, f.seek(SeekFrom::Start(off)))?;
            let mut buf = Vec::new();
            Ok(match f.take(len).read_to_end(&mut buf) {
                Ok(n) if n as u64 == len => Some(buf),
                _ => None,
            })
        }
    }
    /// Read and decode one object's encoded bytes with a single ranged read (caller verifies the hash).
    pub fn read(&self, id: &ObjectId) -> Result<Option<Vec<u8>>, StoreError> {
        let corrupt = |reason: String| StoreError::Corrupt { id: *id, reason };
        let Some((off, len)) = self.idx.lookup(id).map_err(corrupt)? else {
            return Ok(None);
        };
        if self.idx.version == 3 {
            // a block holds many objects: when the one read last is this block, do not read its
            // (possibly megabytes of) compressed bytes again for every object in it
            let hit = self.block.lock().unwrap().as_ref().filter(|c| c.0 == off).map(|c| c.1.clone());
            if let Some(blk) = hit {
                return blk.get(id).map(|b| Some(b.to_vec())).ok_or_else(|| StoreError::Corrupt {
                    id: *id,
                    reason: "not in its block".into(),
                });
            }
        }
        let buf = self.read_range(off, len as u64)?.ok_or_else(|| corrupt("pack truncated".into()))?;
        if self.idx.version == 4 && buf.first() == Some(&CODEC_DELTA) {
            let limit = self.idx.pack_len.saturating_sub(32);
            let mut fetch = |o: u64| -> Result<Vec<u8>, String> {
                let len4 = self
                    .read_range(o.wrapping_sub(4), 4)
                    .ok()
                    .flatten()
                    .ok_or_else(|| "base frame: short read".to_string())?;
                let l = u32::from_le_bytes([len4[0], len4[1], len4[2], len4[3]]) as u64;
                let Some(end) = o.checked_add(l) else {
                    return Err("base entry outside pack".into());
                };
                if l == 0 || end > limit {
                    return Err("base entry outside pack".into());
                }
                match self.read_range(o, l) {
                    Ok(Some(b)) => Ok(b),
                    _ => Err("pack truncated".into()),
                }
            };
            return resolve_v4(off, buf, MAX_DECODED, &mut fetch).map(Some).map_err(corrupt);
        }
        if self.idx.version == 3 {
            let mut cached = self.block.lock().unwrap();
            if cached.as_ref().map(|c| c.0) != Some(off) {
                *cached = Some((off, std::sync::Arc::new(decode_block(&buf).map_err(corrupt)?)));
            }
            let blk = cached.as_ref().expect("just set").1.clone();
            return blk.get(id).map(|b| Some(b.to_vec())).ok_or_else(|| StoreError::Corrupt {
                id: *id,
                reason: "not in its block".into(),
            });
        }
        decode_payload(self.idx.version, &buf)
            .map(|c| Some(c.into_owned()))
            .map_err(corrupt)
    }
    pub fn modified(&self) -> Option<std::time::SystemTime> {
        fs::metadata(&self.pack_path).ok()?.modified().ok()
    }

    /// Full check: index (checksum, order, fanout), whole-pack checksum, then every entry's
    /// framing, codec and object hash. This is where a corrupted index that opened lazily is caught.
    pub fn verify(&self) -> Vec<String> {
        let mut out = Vec::new();
        let name = self.pack_path.display().to_string();
        let name = name.as_str();
        if let Err(e) = self.idx.verify_full() {
            out.push(format!("{}: {e}", self.pack_path.with_extension("idx").display()));
        }
        let bytes = match fs::read(&self.pack_path) {
            Ok(b) => b,
            Err(e) => {
                out.push(format!("{name}: {e}"));
                return out;
            }
        };
        let magic = match self.idx.version {
            4 => PACK_MAGIC_V4,
            3 => PACK_MAGIC_V3,
            2 => PACK_MAGIC_V2,
            _ => PACK_MAGIC,
        };
        if bytes.len() as u64 != self.idx.pack_len || bytes.len() < 44 || &bytes[..8] != magic {
            out.push(format!("{name}: bad length or header"));
            return out;
        }
        let (body, trailer) = bytes.split_at(bytes.len() - 32);
        let mut entries: Vec<_> = self.idx.iter().collect();
        let version = self.idx.version;
        if version == 3 {
            // many ids share a block: check each block once, and every id against its block
            entries.sort_by_key(|e| e.1);
        }
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(8);
        let per = entries.len().div_ceil(threads).max(1);
        // The whole-pack checksum and the per-object hashes are independent: run them side by side.
        std::thread::scope(|sc| {
            let sum = sc.spawn(|| blake3::hash(body));
            let parts: Vec<_> = entries
                .chunks(per)
                .map(|part| {
                    sc.spawn(move || {
                        let mut out = Vec::new();
                        let mut blk: Option<(u64, Result<Block, String>)> = None;
                        for (id, off, len) in part {
                            let (o, l) = (*off as usize, *len as usize);
                            if o < 4 || o + l > body.len() || u32::from_le_bytes(body[o - 4..o].try_into().unwrap()) as usize != l {
                                out.push(format!("{name}: bad framing for {id}"));
                                continue;
                            }
                            if version == 3 {
                                if blk.as_ref().map(|b| b.0) != Some(*off) {
                                    blk = Some((*off, decode_block(&body[o..o + l])));
                                }
                                match &blk.as_ref().expect("just set").1 {
                                    Err(e) => out.push(format!("{name}: block at {off} undecodable: {e}")),
                                    Ok(b) => match b.get(id) {
                                        None => out.push(format!("{name}: {id} is not in its block")),
                                        Some(enc) if vcs_objects::id_of_encoded(enc) != *id => {
                                            out.push(format!("{name}: object {id} hash mismatch"))
                                        }
                                        Some(_) => {}
                                    },
                                }
                                continue;
                            }
                            let decoded = if version == 4 && body[o..o + l].first() == Some(&CODEC_DELTA) {
                                let mut fetch = |bo: u64| -> Result<Vec<u8>, String> {
                                    let bo = bo as usize;
                                    if bo < 4 || bo > body.len() {
                                        return Err("base entry outside pack".into());
                                    }
                                    let bl = u32::from_le_bytes(body[bo - 4..bo].try_into().unwrap()) as usize;
                                    body.get(bo..bo.saturating_add(bl))
                                        .map(|s| s.to_vec())
                                        .ok_or_else(|| "base entry outside pack".to_string())
                                };
                                resolve_v4(o as u64, body[o..o + l].to_vec(), MAX_DECODED, &mut fetch).map(std::borrow::Cow::Owned)
                            } else {
                                decode_payload(version, &body[o..o + l])
                            };
                            match decoded {
                                Err(e) => out.push(format!("{name}: object {id} undecodable: {e}")),
                                Ok(enc) if vcs_objects::id_of_encoded(&enc) != *id => {
                                    out.push(format!("{name}: object {id} hash mismatch"))
                                }
                                Ok(_) => {}
                            }
                        }
                        out
                    })
                })
                .collect();
            let h = sum.join().unwrap();
            if h.as_bytes() != trailer || trailer != self.idx.checksum {
                out.push(format!("{name}: pack checksum mismatch"));
            }
            for p in parts {
                out.extend(p.join().unwrap());
            }
        });
        out
    }
}

/// A pack and its index, built in memory.
pub struct BuiltPack {
    /// `pack-<hex of the pack checksum>`: content-addressed, so uploading it twice is harmless.
    pub name: String,
    pub pack: Vec<u8>,
    pub idx: Vec<u8>,
}

/// Build pack and index bytes for `objects` (id, encoded bytes) in the default write format
/// ([`PackFormat::from_env`]: v2, never v4). See [`build_pack_fmt`].
pub fn build_pack(objects: Vec<(ObjectId, Vec<u8>)>) -> BuiltPack {
    // Packs built here go to the network, where entries are fetched by range and delta bases cannot be resolved: never v4.
    let fmt = match PackFormat::from_env() {
        PackFormat::V4 => PackFormat::V2,
        f => f,
    };
    build_pack_fmt(objects, fmt)
}

/// Build pack and index bytes in an explicit format. Entries are laid out in the order given
/// (duplicate ids keep their first occurrence), so callers may group related objects; the index
/// is always sorted by id.
pub fn build_pack_fmt(objects: Vec<(ObjectId, Vec<u8>)>, fmt: PackFormat) -> BuiltPack {
    build_pack_blocks(objects, fmt, BLOCK_TARGET)
}

/// [`build_pack_fmt`] with an explicit block size for v3 (ignored for v1/v2).
pub fn build_pack_blocks(objects: Vec<(ObjectId, Vec<u8>)>, fmt: PackFormat, block_target: usize) -> BuiltPack {
    let mut seen = std::collections::HashSet::new();
    let objects: Vec<_> = objects.into_iter().filter(|o| seen.insert(o.0)).collect();
    let (order, planned): (Vec<usize>, Vec<Planned>) = if fmt == PackFormat::V4 {
        plan_v4(&objects)
    } else {
        (Vec::new(), Vec::new())
    };
    let mut pack = Vec::new();
    pack.extend_from_slice(match fmt {
        PackFormat::V4 => PACK_MAGIC_V4,
        PackFormat::V3 => PACK_MAGIC_V3,
        PackFormat::V2 => PACK_MAGIC_V2,
        PackFormat::V1 => PACK_MAGIC,
    });
    let mut ents = Vec::new();
    if fmt == PackFormat::V3 {
        let blocks = split_blocks(&objects, block_target.max(1));
        let payloads = encode_blocks(&blocks);
        pack.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
        for (blk, pl) in blocks.iter().zip(&payloads) {
            pack.extend_from_slice(&(pl.len() as u32).to_le_bytes());
            let off = pack.len() as u64;
            for o in blk {
                ents.push((o.0, off, pl.len() as u32));
            }
            pack.extend_from_slice(pl);
        }
    } else if fmt == PackFormat::V4 {
        pack.extend_from_slice(&(objects.len() as u32).to_le_bytes());
        let mut starts: Vec<u64> = Vec::with_capacity(order.len());
        for (k, &oi) in order.iter().enumerate() {
            let pl: std::borrow::Cow<[u8]> = match &planned[k] {
                Planned::Whole(w) => std::borrow::Cow::Borrowed(w),
                Planned::Delta { base, z } => {
                    let here = pack.len() as u64 + 4;
                    let mut v = vec![CODEC_DELTA];
                    let mut dist = here - starts[*base];
                    loop {
                        let b = (dist & 0x7f) as u8;
                        dist >>= 7;
                        if dist == 0 {
                            v.push(b);
                            break;
                        }
                        v.push(b | 0x80);
                    }
                    v.extend_from_slice(z);
                    std::borrow::Cow::Owned(v)
                }
            };
            pack.extend_from_slice(&(pl.len() as u32).to_le_bytes());
            starts.push(pack.len() as u64);
            ents.push((objects[oi].0, pack.len() as u64, pl.len() as u32));
            pack.extend_from_slice(&pl);
        }
    } else {
        let payloads = encode_all(&objects, fmt);
        pack.extend_from_slice(&(objects.len() as u32).to_le_bytes());
        for ((id, _), pl) in objects.iter().zip(&payloads) {
            pack.extend_from_slice(&(pl.len() as u32).to_le_bytes());
            ents.push((*id, pack.len() as u64, pl.len() as u32));
            pack.extend_from_slice(pl);
        }
    }
    let sum = *blake3::hash(&pack).as_bytes();
    pack.extend_from_slice(&sum);
    ents.sort_by_key(|e| e.0);
    let mut idx = Vec::new();
    idx.extend_from_slice(match fmt {
        PackFormat::V4 => IDX_MAGIC_V4,
        PackFormat::V3 => IDX_MAGIC_V3,
        PackFormat::V2 => IDX_MAGIC_V2,
        PackFormat::V1 => IDX_MAGIC,
    });
    idx.extend_from_slice(&(ents.len() as u32).to_le_bytes());
    idx.extend_from_slice(&(pack.len() as u64).to_le_bytes());
    idx.extend_from_slice(&sum);
    if fmt != PackFormat::V1 {
        let mut counts = [0u32; 256];
        for e in &ents {
            counts[e.0.as_bytes()[0] as usize] += 1;
        }
        let mut run = 0u32;
        for c in counts {
            run += c;
            idx.extend_from_slice(&run.to_le_bytes());
        }
    }
    for (id, off, len) in &ents {
        idx.extend_from_slice(id.as_bytes());
        idx.extend_from_slice(&off.to_le_bytes());
        idx.extend_from_slice(&len.to_le_bytes());
    }
    idx.extend_from_slice(blake3::hash(&idx).as_bytes());
    BuiltPack {
        name: format!("pack-{}", ObjectId::from_bytes(sum).to_hex()),
        pack,
        idx,
    }
}

/// One planned pack entry: stored whole (finished payload) or as a delta against the entry at
/// layout position `base` (always earlier than itself).
enum Planned {
    Whole(Vec<u8>),
    Delta { base: usize, z: Vec<u8> },
}

/// Kinds worth deltifying (the gate measured blobs and trees; commits and the rest gain < 1.15x).
fn deltable(enc: &[u8]) -> bool {
    enc.len() >= 64 && enc.len() <= crate::delta::MAX_BASE && matches!(enc[0], 1 | 2)
}

/// Choose layout order and delta bases. Eligible objects are sorted by (kind, content sketch,
/// size desc) so versions of one file or directory are adjacent without knowing any path; each
/// is then tried as a delta against up to `DELTA_WINDOW` predecessors whose chain is shorter than
/// [`MAX_WRITE_DEPTH`], and kept only if it beats the whole v2 payload. Deterministic.
fn plan_v4(objects: &[(ObjectId, Vec<u8>)]) -> (Vec<usize>, Vec<Planned>) {
    const CHUNK: usize = 4096;
    let eligible: Vec<usize> = (0..objects.len()).filter(|&i| deltable(&objects[i].1)).collect();
    let keys = par_map_idx(&eligible, |&i| crate::delta::sketch(&objects[i].1));
    let mut sorted: Vec<(u8, u64, u64, usize, ObjectId, usize)> = eligible
        .iter()
        .zip(keys)
        .map(|(&i, (mn, mx))| (objects[i].1[0], mn, mx, usize::MAX - objects[i].1.len(), objects[i].0, i))
        .collect();
    sorted.sort();
    let mut order: Vec<usize> = sorted.iter().map(|s| s.5).collect();
    let ne = order.len();
    let rest: Vec<usize> = (0..objects.len()).filter(|&i| !deltable(&objects[i].1)).collect();
    order.extend(rest.iter().copied());
    let starts: Vec<usize> = (0..ne).step_by(CHUNK).collect();
    let chunks: Vec<Vec<Planned>> = par_map_idx(&starts, |&s| {
        let e = (s + CHUNK).min(ne);
        let mut out: Vec<Planned> = Vec::with_capacity(e - s);
        let mut win: Vec<(usize, u32, crate::delta::Index)> = Vec::new();
        for pos in s..e {
            let target = &objects[order[pos]].1;
            let whole = encode_payload(PackFormat::V2, target);
            let mut best: Option<(usize, u32, Vec<u8>)> = None;
            let mut best_len = whole.len();
            for (bpos, bd, idx) in win.iter().rev().take(DELTA_WINDOW) {
                if *bd >= MAX_WRITE_DEPTH || objects[order[*bpos]].1[0] != target[0] {
                    continue;
                }
                if let Some(d) = crate::delta::create(idx, target, target.len()) {
                    if let Ok(z) = zstd::bulk::compress(&d, ZSTD_LEVEL) {
                        let l = 1 + 3 + z.len();
                        if l < best_len {
                            best_len = l;
                            best = Some((*bpos, *bd + 1, z));
                        }
                    }
                }
            }
            let depth = match best {
                Some((base, d, z)) => {
                    out.push(Planned::Delta { base, z });
                    d
                }
                None => {
                    out.push(Planned::Whole(whole));
                    0
                }
            };
            win.push((pos, depth, crate::delta::Index::new(target)));
            if win.len() > DELTA_WINDOW * 2 {
                win.drain(..DELTA_WINDOW);
            }
        }
        out
    });
    let mut planned: Vec<Planned> = chunks.into_iter().flatten().collect();
    planned.extend(rest.iter().map(|&i| Planned::Whole(encode_payload(PackFormat::V2, &objects[i].1))));
    (order, planned)
}

/// Run `f` over `items` on several threads (inline when small); results keep the input order.
fn par_map_idx<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(16);
    if threads <= 1 || items.len() < 2 {
        return items.iter().map(f).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<R>>> = items.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|sc| {
        for _ in 0..threads.min(items.len()) {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(it) = items.get(i) else { break };
                *slots[i].lock().unwrap() = Some(f(it));
            });
        }
    });
    slots.into_iter().map(|m| m.into_inner().unwrap().unwrap()).collect()
}

/// Block payloads, compressed on several threads when there is enough to do.
fn encode_blocks(blocks: &[Vec<&(ObjectId, Vec<u8>)>]) -> Vec<Vec<u8>> {
    if blocks.len() < 2 {
        return blocks.iter().map(|b| encode_block(b)).collect();
    }
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(16);
    let per = blocks.len().div_ceil(threads).max(1);
    std::thread::scope(|sc| {
        let parts: Vec<_> = blocks
            .chunks(per)
            .map(|c| sc.spawn(move || c.iter().map(|b| encode_block(b)).collect::<Vec<_>>()))
            .collect();
        parts.into_iter().flat_map(|p| p.join().unwrap()).collect()
    })
}

/// Entry payloads for all objects; v2 compression runs on several threads for large inputs.
fn encode_all(objects: &[(ObjectId, Vec<u8>)], fmt: PackFormat) -> Vec<Vec<u8>> {
    let total: usize = objects.iter().map(|o| o.1.len()).sum();
    if fmt == PackFormat::V1 || total < 256 * 1024 || objects.len() < 2 {
        return objects.iter().map(|o| encode_payload(fmt, &o.1)).collect();
    }
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(16);
    let per = objects.len().div_ceil(threads).max(1);
    std::thread::scope(|sc| {
        let parts: Vec<_> = objects
            .chunks(per)
            .map(|c| sc.spawn(move || c.iter().map(|o| encode_payload(fmt, &o.1)).collect::<Vec<_>>()))
            .collect();
        parts.into_iter().flat_map(|p| p.join().unwrap()).collect()
    })
}

/// Write a pack + index for `objects` (id, encoded bytes) into `dir`; returns the opened pack.
/// Pack is renamed into place first, index last: a pack without an index is ignored.
pub fn write_pack(dir: &Path, tmp_dir: &Path, objects: Vec<(ObjectId, Vec<u8>)>) -> Result<Pack, StoreError> {
    write_pack_fmt(dir, tmp_dir, objects, PackFormat::from_env())
}

/// [`write_pack`] in an explicit format.
pub fn write_pack_fmt(dir: &Path, tmp_dir: &Path, mut objects: Vec<(ObjectId, Vec<u8>)>, fmt: PackFormat) -> Result<Pack, StoreError> {
    objects.sort_by_key(|o| o.0);
    write_pack_ordered(dir, tmp_dir, objects, fmt)
}

/// Like [`write_pack_fmt`] but keeps the order given (the caller chose a layout, see
/// [`crate::layout::locality_order`]); the pack name is the checksum, so the same set in another
/// order is another pack.
pub fn write_pack_ordered(dir: &Path, tmp_dir: &Path, objects: Vec<(ObjectId, Vec<u8>)>, fmt: PackFormat) -> Result<Pack, StoreError> {
    io(dir, fs::create_dir_all(dir))?;
    let built = build_pack_fmt(objects, fmt);
    let name = built.name.strip_prefix("pack-").unwrap_or(&built.name).to_owned();
    let (pp, ip) = (dir.join(format!("pack-{name}.pack")), dir.join(format!("pack-{name}.idx")));
    for (final_path, data) in [(&pp, &built.pack), (&ip, &built.idx)] {
        let tmp = tmp_dir.join(format!("{}.{}.packtmp", name, final_path.extension().unwrap().to_string_lossy()));
        {
            let mut f = io(&tmp, fs::File::create(&tmp))?;
            io(&tmp, f.write_all(data))?;
            io(&tmp, f.sync_all())?;
        }
        io(final_path, fs::rename(&tmp, final_path))?;
    }
    // Directory fsync is a unix notion; Windows cannot open a directory as a plain file.
    #[cfg(unix)]
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Pack::open(&ip)
}

struct HashWriter<W: Write> {
    inner: W,
    hasher: blake3::Hasher,
    len: u64,
}

impl<W: Write> HashWriter<W> {
    fn put(&mut self, b: &[u8]) -> std::io::Result<()> {
        self.hasher.update(b);
        self.len += b.len() as u64;
        self.inner.write_all(b)
    }
}

/// Bytes of pack data between write-back kicks while streaming (Linux).
#[cfg(target_os = "linux")]
const WRITEBACK_STEP: u64 = 8 << 20;

/// Stream `count` objects (id, encoded bytes) into one new pack + index under `<store>/pack`,
/// without holding the whole pack in memory. Same format and crash order as [`write_pack`]:
/// data fsynced, pack renamed, index renamed last, directories fsynced.
pub fn write_pack_streaming<'a>(
    store_dir: &Path,
    count: usize,
    objects: impl Iterator<Item = Result<(ObjectId, std::borrow::Cow<'a, [u8]>), StoreError>>,
) -> Result<Pack, StoreError> {
    let (dir, tmp_dir) = (store_dir.join("pack"), store_dir.join("tmp"));
    io(&dir, fs::create_dir_all(&dir))?;
    let tmp = tmp_dir.join(format!("stream.{}.{:p}.packtmp", std::process::id(), &count));
    let mut ents: Vec<(ObjectId, u64, u32)> = Vec::with_capacity(count);
    let sum = {
        let f = io(&tmp, fs::File::create(&tmp))?;
        let mut w = HashWriter {
            inner: std::io::BufWriter::with_capacity(1 << 20, f),
            hasher: blake3::Hasher::new(),
            len: 0,
        };
        io(&tmp, w.put(PACK_MAGIC_V2))?;
        io(&tmp, w.put(&(count as u32).to_le_bytes()))?;
        // Linux: start write-back of finished stretches while the rest is still being produced,
        // so the final fsync waits for little instead of flushing the whole pack at once.
        #[cfg(target_os = "linux")]
        let mut kicked = 0u64;
        for o in objects {
            let (id, enc) = o?;
            // v2 entry: codec byte (raw: these packs are written for speed) then the encoded object
            io(&tmp, w.put(&(enc.len() as u32 + 1).to_le_bytes()))?;
            ents.push((id, w.len, enc.len() as u32 + 1));
            io(&tmp, w.put(&[CODEC_RAW]))?;
            io(&tmp, w.put(&enc))?;
            #[cfg(target_os = "linux")]
            if w.len >= kicked + WRITEBACK_STEP + (1 << 20) {
                use std::os::unix::io::AsRawFd;
                let done = w.len - (1 << 20); // the BufWriter may still hold the last MiB
                // SAFETY: valid fd; the call only schedules write-back and has no memory effects.
                unsafe {
                    libc::sync_file_range(
                        w.inner.get_ref().as_raw_fd(),
                        kicked as i64,
                        (done - kicked) as i64,
                        libc::SYNC_FILE_RANGE_WRITE,
                    )
                };
                kicked = done;
            }
        }
        let sum = *w.hasher.finalize().as_bytes();
        io(&tmp, w.inner.write_all(&sum))?;
        w.len += 32;
        let f = io(&tmp, w.inner.into_inner().map_err(|e| e.into_error()))?;
        io(&tmp, f.sync_all())?;
        (sum, w.len)
    };
    let (sum, pack_len) = sum;
    ents.sort_by_key(|e| e.0);
    let mut idx = Vec::with_capacity(8 + 4 + 8 + 32 + ents.len() * IDX_ENTRY + 32);
    idx.extend_from_slice(IDX_MAGIC_V2);
    idx.extend_from_slice(&(ents.len() as u32).to_le_bytes());
    idx.extend_from_slice(&pack_len.to_le_bytes());
    idx.extend_from_slice(&sum);
    let mut counts = [0u32; 256];
    for e in &ents {
        counts[e.0.as_bytes()[0] as usize] += 1;
    }
    let mut run = 0u32;
    for c in counts {
        run += c;
        idx.extend_from_slice(&run.to_le_bytes());
    }
    for (id, off, len) in &ents {
        idx.extend_from_slice(id.as_bytes());
        idx.extend_from_slice(&off.to_le_bytes());
        idx.extend_from_slice(&len.to_le_bytes());
    }
    idx.extend_from_slice(blake3::hash(&idx).as_bytes());
    let name = ObjectId::from_bytes(sum).to_hex();
    let (pp, ip) = (dir.join(format!("pack-{name}.pack")), dir.join(format!("pack-{name}.idx")));
    let itmp = tmp_dir.join(format!("stream.{}.{:p}.idxtmp", std::process::id(), &count));
    {
        let mut f = io(&itmp, fs::File::create(&itmp))?;
        io(&itmp, f.write_all(&idx))?;
        io(&itmp, f.sync_all())?;
    }
    io(&pp, fs::rename(&tmp, &pp))?;
    io(&ip, fs::rename(&itmp, &ip))?;
    #[cfg(unix)]
    for d in [&dir, store_dir, &tmp_dir] {
        let f = io(d, fs::File::open(d))?;
        io(d, f.sync_all())?;
    }
    Pack::open(&ip)
}
