//! `AGI5`: the memory-mappable index base file.
//!
//! Layout (all integers little endian; speed over space, so records are fixed width and a lookup
//! is a binary search straight in the mapped file, no decoding of anything else):
//!
//! ```text
//! 0    "AGI5", version u8 (=1), 3 pad
//! 8    generation u64      random per written file; ties an `index.log` to exactly this base
//! 16   written_ns u64
//! 24   n u64               record count
//! 32   heap_len u64        bytes of path heap
//! 40   tree_len u64        bytes of the cache-tree section
//! 48   nchunks u64         number of checksum chunks
//! 56   header_sum u64      sum64 of bytes 0..56
//! 64   fanout[257] u32     fanout[b] = records whose path starts with a byte < b (fanout[256] = n)
//! 1152 records[n] x 64     id[32] size u64 mtime u64 ctime u64 path_off u32 path_len u16 mode u8 pad u8
//!      heap                path bytes, no separators; records are sorted by path bytes
//!      tree section        count varint, fingerprint id, (dir str, id)*   (count 0: absent)
//!      chunk sums          nchunks x u64: sum64 of each 64 KiB chunk of everything above
//!      sums_sum u64        sum64 of the chunk sums, last 8 bytes of the file
//! ```
//!
//! A reader checks the header, then each 64 KiB chunk the first time it touches it, so a lookup
//! verifies a few chunks, not the file; a full decode verifies all of them. A base file is never
//! modified after it is renamed into place (new generation = new file), so readers holding a map
//! of an older generation stay valid.

use crate::error::{Error, Result};
use crate::index::IndexEntry;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, Ordering};
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::{EntryMode, ObjectId};

pub(crate) const MAGIC: &[u8; 4] = b"AGI5";
pub(crate) const FORMAT: u8 = 1;
pub(crate) const REC: usize = 64;
pub(crate) const CHUNK: usize = 64 * 1024;
const HDR: usize = 64;
const FAN: usize = HDR;
const RECS_OFF: usize = 1152;
/// Records per "block" for parallel scans.
pub(crate) const BLOCK5: usize = 2048;

pub(crate) type Trees = (ObjectId, BTreeMap<String, ObjectId>);

fn corrupt(e: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("index: {e}"))
}

/// Fast 64-bit checksum (four multiply/xorshift lanes, ~10 GB/s). Detects torn and bit-flipped
/// data; not cryptographic.
pub(crate) fn sum64(b: &[u8]) -> u64 {
    const P: [u64; 4] = [
        0x9E37_79B1_85EB_CA87,
        0xC2B2_AE3D_27D4_EB4F,
        0x1656_67B1_9E37_79F9,
        0x85EB_CA77_C2B2_AE63,
    ];
    let mut l: [u64; 4] = [
        0x243F_6A88_85A3_08D3,
        0x1319_8A2E_0370_7344,
        0xA409_3822_299F_31D0,
        0x082E_FA98_EC4E_6C89,
    ];
    let mut rest = b;
    while rest.len() >= 32 {
        let (c, r) = rest.split_at(32);
        rest = r;
        for i in 0..4 {
            let w = u64::from_le_bytes(c[i * 8..i * 8 + 8].try_into().expect("8 bytes"));
            l[i] = (l[i] ^ w).wrapping_mul(P[i]);
            l[i] ^= l[i] >> 29;
        }
    }
    let mut tail = [0u8; 32];
    tail[..rest.len()].copy_from_slice(rest);
    for i in 0..4 {
        let w = u64::from_le_bytes(tail[i * 8..i * 8 + 8].try_into().expect("8 bytes"));
        l[i] = (l[i] ^ w).wrapping_mul(P[i]);
        l[i] ^= l[i] >> 29;
    }
    let mut h = (b.len() as u64).wrapping_mul(P[0]);
    for (i, x) in l.iter().enumerate() {
        h = (h ^ x.rotate_left(13 * i as u32 + 1)).wrapping_mul(P[(i + 1) % 4]);
        h ^= h >> 32;
    }
    h
}

/// File contents: a read-only map for big files, a vector otherwise.
pub(crate) enum Bytes {
    Vec(Vec<u8>),
    #[cfg(unix)]
    Map {
        ptr: *mut libc::c_void,
        len: usize,
    },
}

// SAFETY: the map is read-only and never remapped; the bytes behind it are immutable by the
// format's contract (a base file is replaced by rename, never written in place).
unsafe impl Send for Bytes {}
unsafe impl Sync for Bytes {}

impl Bytes {
    pub(crate) fn as_slice(&self) -> &[u8] {
        match self {
            Bytes::Vec(v) => v,
            #[cfg(unix)]
            // SAFETY: ptr/len describe a live PROT_READ mapping owned by self.
            Bytes::Map { ptr, len } => unsafe { std::slice::from_raw_parts(*ptr as *const u8, *len) },
        }
    }

    /// Map (or, for small files and on failure, read) the file at `path`.
    #[allow(dead_code)]
    pub(crate) fn open(path: &std::path::Path) -> std::io::Result<Bytes> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let f = std::fs::File::open(path)?;
            let len = f.metadata()?.len() as usize;
            if len >= 4 * CHUNK {
                // SAFETY: plain read-only private mapping of an open file; checked for MAP_FAILED.
                let p = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, f.as_raw_fd(), 0) };
                if p != libc::MAP_FAILED {
                    return Ok(Bytes::Map { ptr: p, len });
                }
            }
        }
        std::fs::read(path).map(Bytes::Vec)
    }
}

#[cfg(unix)]
impl Drop for Bytes {
    fn drop(&mut self) {
        if let Bytes::Map { ptr, len } = self {
            // SAFETY: unmapping the mapping created in `open`, once.
            unsafe {
                libc::munmap(*ptr, *len);
            }
        }
    }
}

fn u32at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().expect("4 bytes"))
}
fn u64at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().expect("8 bytes"))
}

/// A parsed, header-verified `AGI5` file.
pub(crate) struct Base5 {
    bytes: Bytes,
    pub gen: u64,
    pub written_ns: u64,
    pub n: usize,
    heap_off: usize,
    heap_len: usize,
    tree_off: usize,
    tree_len: usize,
    sums_off: usize,
    nchunks: usize,
    /// 0 = chunk not verified yet, 1 = verified.
    ok: Vec<AtomicU8>,
    /// Every chunk has been verified: slices need no further checks.
    full: std::sync::atomic::AtomicBool,
}

impl Base5 {
    pub(crate) fn is_agi5(b: &[u8]) -> bool {
        b.starts_with(MAGIC)
    }

    pub(crate) fn parse(bytes: Bytes) -> Result<Base5> {
        let b = bytes.as_slice();
        if b.len() < RECS_OFF + 8 || &b[..4] != MAGIC {
            return Err(corrupt("bad magic"));
        }
        if b[4] != FORMAT {
            return Err(corrupt(format!("unsupported AGI5 version {}", b[4])));
        }
        if sum64(&b[..56]) != u64at(b, 56) {
            return Err(corrupt("header checksum mismatch"));
        }
        let (gen, written_ns) = (u64at(b, 8), u64at(b, 16));
        let (n, heap_len, tree_len, nchunks) = (u64at(b, 24), u64at(b, 32), u64at(b, 40), u64at(b, 48));
        let heap_off = (n as u128) * REC as u128 + RECS_OFF as u128;
        let tree_off = heap_off + heap_len as u128;
        let sums_off = tree_off + tree_len as u128;
        let total = sums_off + nchunks as u128 * 8 + 8;
        if total != b.len() as u128 || n > u32::MAX as u64 || heap_len > u32::MAX as u64 {
            return Err(corrupt("length does not match header (torn or truncated file)"));
        }
        let (heap_off, tree_off, sums_off) = (heap_off as usize, tree_off as usize, sums_off as usize);
        if nchunks as usize != sums_off.div_ceil(CHUNK) {
            return Err(corrupt("bad chunk count"));
        }
        let nchunks = nchunks as usize;
        if sum64(&b[sums_off..sums_off + nchunks * 8]) != u64at(b, b.len() - 8) {
            return Err(corrupt("checksum table mismatch"));
        }
        let ok = (0..nchunks).map(|_| AtomicU8::new(0)).collect();
        Ok(Base5 {
            bytes,
            gen,
            written_ns,
            n: n as usize,
            heap_off,
            heap_len: heap_len as usize,
            tree_off,
            tree_len: tree_len as usize,
            sums_off,
            nchunks,
            ok,
            full: std::sync::atomic::AtomicBool::new(false),
        })
    }

    #[allow(dead_code)]
    pub(crate) fn open(path: &std::path::Path) -> Result<Base5> {
        let b = Bytes::open(path).map_err(|e| Error::Io {
            path: path.to_owned(),
            source: e,
        })?;
        Base5::parse(b)
    }

    pub(crate) fn raw(&self) -> &[u8] {
        self.bytes.as_slice()
    }
    pub(crate) fn len_bytes(&self) -> usize {
        self.raw().len()
    }

    /// Identity of the whole content: a hash of the header and the chunk checksums (which cover
    /// every other byte). Cheap (a few KiB) and changes whenever any byte does.
    pub(crate) fn digest(&self) -> [u8; 32] {
        let b = self.raw();
        let mut h = blake3::Hasher::new();
        h.update(&b[..HDR]);
        h.update(&b[self.sums_off..]);
        *h.finalize().as_bytes()
    }

    /// The checksum-table checksum (last 8 bytes): binds a log to this exact file.
    pub(crate) fn sums_sum(&self) -> u64 {
        u64at(self.raw(), self.len_bytes() - 8)
    }

    fn verify_chunk(&self, c: usize) -> Result<()> {
        if self.ok[c].load(Ordering::Relaxed) == 1 {
            return Ok(());
        }
        let b = self.raw();
        let (lo, hi) = (c * CHUNK, ((c + 1) * CHUNK).min(self.sums_off));
        if sum64(&b[lo..hi]) != u64at(b, self.sums_off + c * 8) {
            return Err(corrupt(format!("checksum mismatch in chunk {c} (bytes {lo}..{hi})")));
        }
        self.ok[c].store(1, Ordering::Relaxed);
        Ok(())
    }

    /// Bytes `[off, off+len)` of the body, after verifying the chunks they lie in.
    fn slice(&self, off: usize, len: usize) -> Result<&[u8]> {
        let end = off
            .checked_add(len)
            .filter(|e| *e <= self.sums_off)
            .ok_or_else(|| corrupt("offset out of range"))?;
        if len > 0 && !self.full.load(Ordering::Relaxed) {
            for c in off / CHUNK..=(end - 1) / CHUNK {
                self.verify_chunk(c)?;
            }
        }
        Ok(&self.raw()[off..end])
    }

    /// Verify every chunk (a full decode does this; lookups verify lazily).
    pub(crate) fn verify_all(&self) -> Result<()> {
        for c in 0..self.nchunks {
            self.verify_chunk(c)?;
        }
        self.full.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn fan(&self, b: usize) -> Result<usize> {
        let o = self.slice(FAN + b * 4, 4)?;
        let v = u32at(o, 0) as usize;
        if v > self.n {
            return Err(corrupt("bad fanout"));
        }
        Ok(v)
    }

    /// Path of record `i`.
    pub(crate) fn path(&self, i: usize) -> Result<&str> {
        let r = self.slice(RECS_OFF + i * REC, REC)?;
        self.path_of(r)
    }

    fn path_of(&self, r: &[u8]) -> Result<&str> {
        let (po, pl) = (u32at(r, 56) as usize, u16::from_le_bytes([r[60], r[61]]) as usize);
        if po + pl > self.heap_len {
            return Err(corrupt("path outside heap"));
        }
        let p = self.slice(self.heap_off + po, pl)?;
        std::str::from_utf8(p).map_err(|_| corrupt("utf-8"))
    }

    fn entry_of(&self, r: &[u8]) -> Result<IndexEntry> {
        let mode = match r[62] {
            1 => EntryMode::File,
            2 => EntryMode::Executable,
            4 => EntryMode::Symlink,
            _ => return Err(corrupt("bad mode")),
        };
        let mut id = [0u8; 32];
        id.copy_from_slice(&r[..32]);
        Ok(IndexEntry {
            id: ObjectId::from_bytes(id),
            mode,
            size: u64at(r, 32),
            mtime_ns: u64at(r, 40),
            ctime_ns: u64at(r, 48),
        })
    }

    /// Path and entry of record `i`.
    pub(crate) fn get_at(&self, i: usize) -> Result<(&str, IndexEntry)> {
        let r = self.slice(RECS_OFF + i * REC, REC)?;
        Ok((self.path_of(r)?, self.entry_of(r)?))
    }

    /// Index of the first record whose path is `>= key` (`n` when none).
    pub(crate) fn lower_bound(&self, key: &str) -> Result<usize> {
        let kb = key.as_bytes();
        let (mut lo, mut hi) = match kb.first() {
            Some(&c) => (self.fan(c as usize)?, self.fan(c as usize + 1)?),
            None => return Ok(0),
        };
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.path(mid)?.as_bytes() < kb {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }

    /// The entry for exactly `path`.
    pub(crate) fn find(&self, path: &str) -> Result<Option<IndexEntry>> {
        let i = self.lower_bound(path)?;
        if i < self.n {
            let (p, e) = self.get_at(i)?;
            if p == path {
                return Ok(Some(e));
            }
        }
        Ok(None)
    }

    /// The cache-tree section (verified), if any.
    pub(crate) fn trees(&self) -> Result<Option<Trees>> {
        let s = self.slice(self.tree_off, self.tree_len)?;
        let mut r = Reader::new(s);
        let c = |e: vcs_objects::DecodeError| corrupt(e);
        let n = r.varint().map_err(c)? as usize;
        let mut out = None;
        if n > 0 {
            let fp = r.id().map_err(c)?;
            let mut m = BTreeMap::new();
            for _ in 0..n {
                let d = r.str(4096).map_err(c)?;
                if !d.is_empty() {
                    vcs_objects::names::validate_path(&d)?;
                }
                m.insert(d, r.id().map_err(c)?);
            }
            out = Some((fp, m));
        }
        r.finish().map_err(c)?;
        Ok(out)
    }

    /// Visit records `[a, b)` in order with the path borrowed from the file. The paths are
    /// validated as the old decoder did (`prev` seeds the shared-prefix shortcut).
    pub(crate) fn each(&self, a: usize, b: usize, mut f: impl FnMut(&str, IndexEntry) -> Result<()>) -> Result<()> {
        let mut prev: &str = "";
        for i in a..b {
            let (p, e) = self.get_at(i)?;
            if !prev.is_empty() && p.as_bytes() <= prev.as_bytes() {
                return Err(corrupt("entries out of order"));
            }
            vcs_objects::names::validate_path_after(prev, p)?;
            f(p, e)?;
            prev = p;
        }
        Ok(())
    }

    /// Check the invariants a reader may rely on without having looked at every record:
    /// strict order across block boundaries and fanout consistency are checked by `each`
    /// (order) over all records by the caller; here only the fanout against the records.
    #[cfg(test)]
    pub(crate) fn check_fanout(&self) -> Result<()> {
        let mut at = 0usize;
        for b in 0..256usize {
            let f = self.fan(b + 1)?;
            if f < at {
                return Err(corrupt("fanout not monotonic"));
            }
            for i in at..f {
                if self.path(i)?.as_bytes().first().map(|&c| c as usize) != Some(b) {
                    return Err(corrupt("fanout disagrees with records"));
                }
            }
            at = f;
        }
        if at != self.n || self.fan(0)? != 0 {
            return Err(corrupt("fanout does not cover the records"));
        }
        Ok(())
    }
}

/// Serialise sorted entries (`entries`, exactly `n` of them) plus the cache-tree section as an
/// `AGI5` base file with generation `gen`.
pub(crate) fn encode<'a>(
    n: usize,
    entries: impl Iterator<Item = (&'a str, &'a IndexEntry)>,
    written_ns: u64,
    trees: Option<(&ObjectId, &BTreeMap<String, ObjectId>)>,
    gen: u64,
) -> Vec<u8> {
    let heap_hint = n * 40;
    let mut out: Vec<u8> = Vec::with_capacity(RECS_OFF + n * REC + heap_hint + 4096);
    out.resize(RECS_OFF, 0);
    let mut heap: Vec<u8> = Vec::with_capacity(heap_hint);
    let mut fan = [0u32; 257];
    let mut count = 0usize;
    for (p, e) in entries {
        let pb = p.as_bytes();
        debug_assert!(!pb.is_empty() && pb.len() <= u16::MAX as usize);
        fan[pb[0] as usize + 1] += 1;
        out.extend_from_slice(e.id.as_bytes());
        out.extend_from_slice(&e.size.to_le_bytes());
        out.extend_from_slice(&e.mtime_ns.to_le_bytes());
        out.extend_from_slice(&e.ctime_ns.to_le_bytes());
        out.extend_from_slice(&(heap.len() as u32).to_le_bytes());
        out.extend_from_slice(&(pb.len() as u16).to_le_bytes());
        out.push(e.mode as u8);
        out.push(0);
        heap.extend_from_slice(pb);
        count += 1;
    }
    assert_eq!(count, n, "entry count mismatch");
    for i in 1..257 {
        fan[i] += fan[i - 1];
    }
    for (i, f) in fan.iter().enumerate() {
        out[FAN + i * 4..FAN + i * 4 + 4].copy_from_slice(&f.to_le_bytes());
    }
    out.extend_from_slice(&heap);
    let tree_start = out.len();
    let mut tw = Writer::new();
    match trees {
        Some((fp, t)) if !t.is_empty() => {
            tw.varint(t.len() as u64).id(fp);
            for (d, id) in t {
                tw.str(d).id(id);
            }
        }
        _ => {
            tw.varint(0);
        }
    }
    out.extend_from_slice(&tw.finish());
    let tree_len = out.len() - tree_start;
    let sums_off = out.len();
    let nchunks = sums_off.div_ceil(CHUNK);
    out[0..4].copy_from_slice(MAGIC);
    out[4] = FORMAT;
    out[8..16].copy_from_slice(&gen.to_le_bytes());
    out[16..24].copy_from_slice(&written_ns.to_le_bytes());
    out[24..32].copy_from_slice(&(n as u64).to_le_bytes());
    out[32..40].copy_from_slice(&(heap.len() as u64).to_le_bytes());
    out[40..48].copy_from_slice(&(tree_len as u64).to_le_bytes());
    out[48..56].copy_from_slice(&(nchunks as u64).to_le_bytes());
    let hs = sum64(&out[..56]);
    out[56..64].copy_from_slice(&hs.to_le_bytes());
    // chunk sums (the header is in chunk 0 and final by now)
    let sums: Vec<u64> = (0..nchunks)
        .map(|c| sum64(&out[c * CHUNK..((c + 1) * CHUNK).min(sums_off)]))
        .collect();
    let mut st = Vec::with_capacity(nchunks * 8);
    for s in &sums {
        st.extend_from_slice(&s.to_le_bytes());
    }
    out.extend_from_slice(&st);
    out.extend_from_slice(&sum64(&st).to_le_bytes());
    out
}

/// A fresh generation id: random enough to never repeat between two writers.
pub(crate) fn new_gen() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    let mut h = blake3::Hasher::new();
    h.update(&t.to_le_bytes());
    h.update(&std::process::id().to_le_bytes());
    let addr = &h as *const _ as usize;
    h.update(&addr.to_le_bytes());
    static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    h.update(&CTR.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    u64::from_le_bytes(h.finalize().as_bytes()[..8].try_into().expect("8 bytes"))
}

/// Cheap torn-write check of the file at `path`: reads only the header and compares the length it
/// promises with the file's. `None` when the file is not `AGI5` (the caller judges other layouts).
pub(crate) fn looks_whole(path: &std::path::Path) -> Option<bool> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = [0u8; HDR];
    let got = f.read(&mut h).ok()?;
    if got < 4 || &h[..4] != MAGIC {
        return None;
    }
    if got < HDR || h[4] != FORMAT || sum64(&h[..56]) != u64at(&h, 56) {
        return Some(false);
    }
    let (n, heap, tree, nch) = (
        u64at(&h, 24) as u128,
        u64at(&h, 32) as u128,
        u64at(&h, 40) as u128,
        u64at(&h, 48) as u128,
    );
    let want = RECS_OFF as u128 + n * REC as u128 + heap + tree + nch * 8 + 8;
    Some(f.metadata().ok()?.len() as u128 == want)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ent(i: u64) -> IndexEntry {
        IndexEntry {
            id: ObjectId::from_bytes([i as u8; 32]),
            mode: if i.is_multiple_of(3) {
                EntryMode::Executable
            } else {
                EntryMode::File
            },
            size: i * 7,
            mtime_ns: 1_000 + i,
            ctime_ns: 2_000 + i,
        }
    }

    fn sample(n: usize) -> Vec<(String, IndexEntry)> {
        let mut v: Vec<(String, IndexEntry)> = (0..n).map(|i| (format!("d{:03}/f{:05}.txt", i % 97, i), ent(i as u64))).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    fn enc(v: &[(String, IndexEntry)]) -> Vec<u8> {
        encode(v.len(), v.iter().map(|(p, e)| (p.as_str(), e)), 77, None, 5)
    }

    #[test]
    fn roundtrip_and_lookup() {
        let v = sample(3000);
        let b = Base5::parse(Bytes::Vec(enc(&v))).unwrap();
        assert_eq!((b.n, b.written_ns, b.gen), (3000, 77, 5));
        b.verify_all().unwrap();
        b.check_fanout().unwrap();
        for (p, e) in &v {
            assert_eq!(b.find(p).unwrap().as_ref(), Some(e));
        }
        assert!(b.find("nope").unwrap().is_none());
        assert!(b.find("d000/f00000.txtx").unwrap().is_none());
        let mut seen = 0;
        b.each(0, b.n, |p, e| {
            assert_eq!(p, v[seen].0);
            assert_eq!(e, v[seen].1);
            seen += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, 3000);
        // lower_bound agrees with partition_point
        for key in ["", "a", "d000/", "d050/f0", "d096/zzz", "zzzz"] {
            let want = v.partition_point(|(k, _)| k.as_str() < key);
            assert_eq!(b.lower_bound(key).unwrap(), want, "key {key:?}");
        }
    }

    #[test]
    fn empty_index_roundtrips() {
        let b = Base5::parse(Bytes::Vec(enc(&[]))).unwrap();
        assert_eq!(b.n, 0);
        assert!(b.find("a").unwrap().is_none());
        assert_eq!(b.lower_bound("a").unwrap(), 0);
        b.verify_all().unwrap();
        b.check_fanout().unwrap();
        assert!(b.trees().unwrap().is_none());
    }

    #[test]
    fn trees_roundtrip() {
        let mut t = BTreeMap::new();
        t.insert(String::new(), ObjectId::from_bytes([9; 32]));
        t.insert("d000".to_owned(), ObjectId::from_bytes([8; 32]));
        let fp = ObjectId::from_bytes([7; 32]);
        let v = sample(10);
        let bytes = encode(10, v.iter().map(|(p, e)| (p.as_str(), e)), 1, Some((&fp, &t)), 1);
        let (gf, gt) = Base5::parse(Bytes::Vec(bytes)).unwrap().trees().unwrap().unwrap();
        assert_eq!((gf, gt), (fp, t));
    }

    #[test]
    fn any_flipped_byte_is_detected() {
        let v = sample(2000);
        let good = enc(&v);
        // sample positions across header, fanout, records, heap, tail
        let step = good.len() / 211 + 1;
        for pos in (0..good.len()).step_by(step) {
            let mut bad = good.clone();
            bad[pos] ^= 0x40;
            let r = Base5::parse(Bytes::Vec(bad)).and_then(|b| {
                b.verify_all()?;
                b.trees()?;
                Ok(())
            });
            assert!(r.is_err(), "flip at {pos} of {} went unnoticed", good.len());
        }
    }

    #[test]
    fn truncation_is_detected() {
        let good = enc(&sample(500));
        for cut in [1, 8, 100, good.len() / 2] {
            assert!(Base5::parse(Bytes::Vec(good[..good.len() - cut].to_vec())).is_err());
        }
        assert!(Base5::parse(Bytes::Vec(Vec::new())).is_err());
    }

    #[test]
    fn newer_version_is_refused_not_misread() {
        let mut b = enc(&sample(5));
        b[4] = 2;
        let e = Base5::parse(Bytes::Vec(b)).err().expect("must fail").to_string();
        assert!(e.contains("unsupported AGI5 version"), "{e}");
    }

    #[test]
    fn lookup_verifies_only_the_chunks_it_touches() {
        let v = sample(20_000);
        let mut bytes = enc(&v);
        // damage the last record's chunk; a lookup near the start must still work, a full check must fail
        let at = RECS_OFF + 10_000 * REC + 3;
        bytes[at] ^= 1;
        let b = Base5::parse(Bytes::Vec(bytes)).unwrap();
        assert!(b.find("a-not-there").unwrap().is_none()); // touches the fanout chunk only
        assert!(b.find(&v[10_000].0).is_err());
        assert!(b.verify_all().is_err());
    }
}
