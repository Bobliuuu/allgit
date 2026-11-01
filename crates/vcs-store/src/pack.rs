//! Pack files: many encoded objects in one file plus a sorted index. Format in docs/object-model.md.
//!
//! Two on-disk versions exist. v1 (`VCSPACK1`/`VCSPIDX1`) stores encoded objects as they are; v2
//! (`VCSPACK2`/`VCSPIDX2`) prefixes every entry with a codec byte (raw or zstd level 3) and adds a
//! 256-entry fanout table to the index. Readers accept both forever; writers emit v2 unless told otherwise.

use crate::{io, StoreError};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use vcs_objects::ObjectId;

pub const PACK_MAGIC: &[u8; 8] = b"VCSPACK1";
pub const IDX_MAGIC: &[u8; 8] = b"VCSPIDX1";
pub const PACK_MAGIC_V2: &[u8; 8] = b"VCSPACK2";
pub const IDX_MAGIC_V2: &[u8; 8] = b"VCSPIDX2";
/// v2 entry codecs (first byte of every entry payload).
pub const CODEC_RAW: u8 = 0;
pub const CODEC_ZSTD: u8 = 1;
pub const ZSTD_LEVEL: i32 = 3;
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
}

impl PackFormat {
    /// The format for new packs: v2 unless `VCS_PACK_FORMAT=v1` (rollback switch).
    pub fn from_env() -> PackFormat {
        match std::env::var("VCS_PACK_FORMAT").as_deref() {
            Ok("v1") | Ok("1") => PackFormat::V1,
            _ => PackFormat::V2,
        }
    }
    pub fn version(self) -> u8 {
        match self {
            PackFormat::V1 => 1,
            PackFormat::V2 => 2,
        }
    }
    pub fn parse(s: &str) -> Option<PackFormat> {
        match s {
            "v1" | "1" => Some(PackFormat::V1),
            "v2" | "2" => Some(PackFormat::V2),
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
        PackFormat::V2 => {
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
    use std::borrow::Cow;
    if version == 1 {
        return Ok(Cow::Borrowed(payload));
    }
    match payload.split_first() {
        None => Err("empty entry".into()),
        Some((&CODEC_RAW, rest)) => Ok(Cow::Borrowed(rest)),
        Some((&CODEC_ZSTD, rest)) => {
            let n = zstd::zstd_safe::get_frame_content_size(rest)
                .map_err(|_| "bad zstd frame header".to_string())?
                .ok_or_else(|| "zstd frame without content size".to_string())?;
            if n > MAX_DECODED {
                return Err("zstd frame claims an oversized object".into());
            }
            let out = zstd::bulk::decompress(rest, n as usize).map_err(|e| format!("zstd: {e}"))?;
            if out.len() as u64 != n {
                return Err("zstd frame length mismatch".into());
            }
            Ok(Cow::Owned(out))
        }
        Some((c, _)) => Err(format!("unknown codec {c}")),
    }
}

/// A parsed pack index (also usable for a pack that lives on a remote). The raw bytes are kept
/// and entries are read in place, so opening costs no per-entry work. [`PackIndex::parse`]
/// verifies everything; [`PackIndex::from_vec_lazy`] (local open) checks only structure and leaves
/// the BLAKE3 trailer, ordering and fanout consistency to [`PackIndex::verify_full`] (fsck).
pub struct PackIndex {
    bytes: Vec<u8>,
    version: u8,
    count: usize,
    base: usize,
    pub pack_len: u64,
    checksum: [u8; 32],
    fanout: Option<Box<[u32; 256]>>,
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
        let b = &bytes;
        let version = match b.get(..8) {
            Some(m) if m == IDX_MAGIC => 1u8,
            Some(m) if m == IDX_MAGIC_V2 => 2u8,
            _ => return Err(bad(label, "bad index header")),
        };
        let fan = if version == 2 { FANOUT_BYTES } else { 0 };
        let base = 8 + 4 + 8 + 32 + fan;
        if b.len() < base + 32 {
            return Err(bad(label, "bad index header"));
        }
        let count = u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize;
        let pack_len = u64::from_le_bytes(b[12..20].try_into().unwrap());
        let checksum: [u8; 32] = b[20..52].try_into().unwrap();
        if count.checked_mul(IDX_ENTRY) != Some(b.len() - base - 32) {
            return Err(bad(label, "index length does not match entry count"));
        }
        let fanout = if version == 2 {
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
        let idx = PackIndex {
            bytes,
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
    /// Everything the lazy open skips: BLAKE3 trailer, strict ordering, every entry inside the
    /// pack, fanout agreeing with the ids.
    pub fn verify_full(&self) -> Result<(), String> {
        let (body, trailer) = self.bytes.split_at(self.bytes.len() - 32);
        if blake3::hash(body).as_bytes() != trailer {
            return Err("index checksum mismatch".into());
        }
        let mut counts = [0u32; 256];
        let mut prev: Option<ObjectId> = None;
        for i in 0..self.count {
            let (id, off, len) = entry_at(&self.bytes, self.base, i);
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
        if off < HEADER + 4 || !end_ok || (self.version == 2 && len == 0) {
            return Err(());
        }
        Ok(())
    }
    /// 1 or 2: which pack format this index belongs to.
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
        (0..self.count).map(|i| entry_at(&self.bytes, self.base, i))
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
        let (mut lo, mut hi) = match &self.fanout {
            Some(f) => {
                let first = id.as_bytes()[0] as usize;
                (if first == 0 { 0 } else { f[first - 1] as usize }, f[first] as usize)
            }
            None => (0, self.count),
        };
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (eid, off, len) = entry_at(&self.bytes, self.base, mid);
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

/// One opened pack: its index lives in memory, object bytes are read by range.
pub struct Pack {
    pub pack_path: PathBuf,
    idx: PackIndex,
}

impl Pack {
    /// Open `<name>.idx` (and check the sibling `.pack` exists with the recorded length).
    /// Cheap: hashes neither the pack nor (v2) the index; [`Pack::verify`] does both.
    pub fn open(idx_path: &Path) -> Result<Pack, StoreError> {
        let b = io(idx_path, fs::read(idx_path))?;
        let idx = PackIndex::from_vec_lazy(b, idx_path)?;
        let pack_path = idx_path.with_extension("pack");
        let actual = io(&pack_path, fs::metadata(&pack_path))?.len();
        if actual != idx.pack_len {
            return Err(bad(
                &pack_path,
                format!("pack is {actual} bytes, index says {} (truncated?)", idx.pack_len),
            ));
        }
        Ok(Pack { pack_path, idx })
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
    /// Read and decode one object's encoded bytes with a single ranged read (caller verifies the hash).
    pub fn read(&self, id: &ObjectId) -> Result<Option<Vec<u8>>, StoreError> {
        let corrupt = |reason: String| StoreError::Corrupt { id: *id, reason };
        let Some((off, len)) = self.idx.lookup(id).map_err(corrupt)? else {
            return Ok(None);
        };
        let mut f = io(&self.pack_path, fs::File::open(&self.pack_path))?;
        io(&self.pack_path, f.seek(SeekFrom::Start(off)))?;
        // `len` comes from the index; never allocate it up front, so a lying index on a short
        // pack cannot demand gigabytes (the buffer only grows with bytes actually read).
        let mut buf = Vec::new();
        match f.take(len as u64).read_to_end(&mut buf) {
            Ok(n) if n == len as usize => {}
            _ => return Err(corrupt("pack truncated".into())),
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
        let magic = if self.idx.version == 2 { PACK_MAGIC_V2 } else { PACK_MAGIC };
        if bytes.len() as u64 != self.idx.pack_len || bytes.len() < 44 || &bytes[..8] != magic {
            out.push(format!("{name}: bad length or header"));
            return out;
        }
        let (body, trailer) = bytes.split_at(bytes.len() - 32);
        let entries: Vec<_> = self.idx.iter().collect();
        let version = self.idx.version;
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
                        for (id, off, len) in part {
                            let (o, l) = (*off as usize, *len as usize);
                            if o < 4 || o + l > body.len() || u32::from_le_bytes(body[o - 4..o].try_into().unwrap()) as usize != l {
                                out.push(format!("{name}: bad framing for {id}"));
                                continue;
                            }
                            match decode_payload(version, &body[o..o + l]) {
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
/// ([`PackFormat::from_env`]: v2). See [`build_pack_fmt`].
pub fn build_pack(objects: Vec<(ObjectId, Vec<u8>)>) -> BuiltPack {
    build_pack_fmt(objects, PackFormat::from_env())
}

/// Build pack and index bytes in an explicit format. Entries are laid out in the order given
/// (duplicate ids keep their first occurrence), so callers may group related objects; the index
/// is always sorted by id.
pub fn build_pack_fmt(objects: Vec<(ObjectId, Vec<u8>)>, fmt: PackFormat) -> BuiltPack {
    let mut seen = std::collections::HashSet::new();
    let objects: Vec<_> = objects.into_iter().filter(|o| seen.insert(o.0)).collect();
    let payloads = encode_all(&objects, fmt);
    let mut pack = Vec::new();
    pack.extend_from_slice(if fmt == PackFormat::V2 { PACK_MAGIC_V2 } else { PACK_MAGIC });
    pack.extend_from_slice(&(objects.len() as u32).to_le_bytes());
    let mut ents = Vec::new();
    for ((id, _), pl) in objects.iter().zip(&payloads) {
        pack.extend_from_slice(&(pl.len() as u32).to_le_bytes());
        ents.push((*id, pack.len() as u64, pl.len() as u32));
        pack.extend_from_slice(pl);
    }
    let sum = *blake3::hash(&pack).as_bytes();
    pack.extend_from_slice(&sum);
    ents.sort_by_key(|e| e.0);
    let mut idx = Vec::new();
    idx.extend_from_slice(if fmt == PackFormat::V2 { IDX_MAGIC_V2 } else { IDX_MAGIC });
    idx.extend_from_slice(&(ents.len() as u32).to_le_bytes());
    idx.extend_from_slice(&(pack.len() as u64).to_le_bytes());
    idx.extend_from_slice(&sum);
    if fmt == PackFormat::V2 {
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
        io(&tmp, w.put(PACK_MAGIC))?;
        io(&tmp, w.put(&(count as u32).to_le_bytes()))?;
        for o in objects {
            let (id, enc) = o?;
            io(&tmp, w.put(&(enc.len() as u32).to_le_bytes()))?;
            ents.push((id, w.len, enc.len() as u32));
            io(&tmp, w.put(&enc))?;
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
    idx.extend_from_slice(IDX_MAGIC);
    idx.extend_from_slice(&(ents.len() as u32).to_le_bytes());
    idx.extend_from_slice(&pack_len.to_le_bytes());
    idx.extend_from_slice(&sum);
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
