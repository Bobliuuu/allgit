//! Pack files: many encoded objects in one file plus a sorted index. Format in docs/object-model.md.

use crate::{io, StoreError};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use vcs_objects::ObjectId;

pub const PACK_MAGIC: &[u8; 8] = b"VCSPACK1";
pub const IDX_MAGIC: &[u8; 8] = b"VCSPIDX1";
const HEADER: u64 = 12; // magic + u32 count
const IDX_ENTRY: usize = 32 + 8 + 4;

fn bad(path: &Path, why: impl Into<String>) -> StoreError {
    StoreError::Io {
        path: path.to_owned(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, why.into()),
    }
}

/// A parsed, checksum-verified pack index (also usable for a pack that lives on a remote).
pub struct PackIndex {
    /// sorted by id: (id, offset of entry payload, payload length)
    entries: Vec<(ObjectId, u64, u32)>,
    pub pack_len: u64,
    checksum: [u8; 32],
}

impl PackIndex {
    /// Parse `.idx` bytes; `label` is only used in error messages.
    pub fn parse(b: &[u8], label: &Path) -> Result<PackIndex, StoreError> {
        let idx_path = label;
        if b.len() < 8 + 4 + 8 + 32 + 32 || &b[..8] != IDX_MAGIC {
            return Err(bad(idx_path, "bad index header"));
        }
        let (body, trailer) = b.split_at(b.len() - 32);
        if blake3::hash(body).as_bytes() != trailer {
            return Err(bad(idx_path, "index checksum mismatch"));
        }
        let count = u32::from_le_bytes(body[8..12].try_into().unwrap()) as usize;
        let pack_len = u64::from_le_bytes(body[12..20].try_into().unwrap());
        let checksum: [u8; 32] = body[20..52].try_into().unwrap();
        let rest = &body[52..];
        if count.checked_mul(IDX_ENTRY) != Some(rest.len()) {
            return Err(bad(idx_path, "index length does not match entry count"));
        }
        let mut entries = Vec::with_capacity(count);
        for c in rest.as_chunks::<IDX_ENTRY>().0 {
            let id = ObjectId::from_bytes(c[..32].try_into().unwrap());
            let off = u64::from_le_bytes(c[32..40].try_into().unwrap());
            let len = u32::from_le_bytes(c[40..44].try_into().unwrap());
            // checked: `off` is attacker-controlled and `off + len` could wrap
            let end_ok = off.checked_add(len as u64).is_some_and(|end| end <= pack_len.saturating_sub(32));
            if off < HEADER + 4 || !end_ok {
                return Err(bad(idx_path, "index entry outside pack"));
            }
            entries.push((id, off, len));
        }
        if entries.windows(2).any(|w| w[0].0 >= w[1].0) {
            return Err(bad(idx_path, "index not strictly sorted"));
        }
        Ok(PackIndex {
            entries,
            pack_len,
            checksum,
        })
    }
    /// All entries as `(id, offset, len)`, sorted by id.
    pub fn entries(&self) -> &[(ObjectId, u64, u32)] {
        &self.entries
    }
    /// `(offset, len)` of the object's encoded bytes within the pack file.
    pub fn find(&self, id: &ObjectId) -> Option<(u64, u32)> {
        self.entries
            .binary_search_by(|e| e.0.cmp(id))
            .ok()
            .map(|i| (self.entries[i].1, self.entries[i].2))
    }
}

/// One opened pack: its index lives in memory, object bytes are read by range.
pub struct Pack {
    pub pack_path: PathBuf,
    idx: PackIndex,
}

impl Pack {
    /// Open `<name>.idx` (and check the sibling `.pack` exists with the recorded length).
    /// Cheap: does not hash the pack; [`Pack::verify`] does.
    pub fn open(idx_path: &Path) -> Result<Pack, StoreError> {
        let b = io(idx_path, fs::read(idx_path))?;
        let idx = PackIndex::parse(&b, idx_path)?;
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

    pub fn ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.idx.entries.iter().map(|e| e.0)
    }
    fn find(&self, id: &ObjectId) -> Option<(u64, u32)> {
        self.idx.find(id)
    }
    pub fn contains(&self, id: &ObjectId) -> bool {
        self.find(id).is_some()
    }
    /// Read the raw encoded bytes of one object with a single ranged read (caller verifies the hash).
    pub fn read(&self, id: &ObjectId) -> Result<Option<Vec<u8>>, StoreError> {
        let Some((off, len)) = self.find(id) else { return Ok(None) };
        let mut f = io(&self.pack_path, fs::File::open(&self.pack_path))?;
        io(&self.pack_path, f.seek(SeekFrom::Start(off)))?;
        // `len` comes from the index; never allocate it up front, so a lying index on a short
        // pack cannot demand gigabytes (the buffer only grows with bytes actually read).
        let mut buf = Vec::new();
        match f.take(len as u64).read_to_end(&mut buf) {
            Ok(n) if n == len as usize => Ok(Some(buf)),
            _ => Err(StoreError::Corrupt {
                id: *id,
                reason: "pack truncated".into(),
            }),
        }
    }
    pub fn modified(&self) -> Option<std::time::SystemTime> {
        fs::metadata(&self.pack_path).ok()?.modified().ok()
    }

    /// Full check: whole-pack checksum, then every entry's framing and object hash.
    pub fn verify(&self) -> Vec<String> {
        let mut out = Vec::new();
        let name = self.pack_path.display().to_string();
        let name = name.as_str();
        let bytes = match fs::read(&self.pack_path) {
            Ok(b) => b,
            Err(e) => return vec![format!("{name}: {e}")],
        };
        if bytes.len() as u64 != self.idx.pack_len || bytes.len() < 44 || &bytes[..8] != PACK_MAGIC {
            return vec![format!("{name}: bad length or header")];
        }
        let (body, trailer) = bytes.split_at(bytes.len() - 32);
        let entries = &self.idx.entries;
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
                            } else if vcs_objects::id_of_encoded(&body[o..o + l]) != *id {
                                out.push(format!("{name}: object {id} hash mismatch"));
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

/// Build pack and index bytes for `objects` (id, encoded bytes). Entries are laid out in the
/// order given (duplicate ids keep their first occurrence), so callers may group related
/// objects; the index is always sorted by id.
pub fn build_pack(objects: Vec<(ObjectId, Vec<u8>)>) -> BuiltPack {
    let mut seen = std::collections::HashSet::new();
    let objects: Vec<_> = objects.into_iter().filter(|o| seen.insert(o.0)).collect();
    let mut pack = Vec::new();
    pack.extend_from_slice(PACK_MAGIC);
    pack.extend_from_slice(&(objects.len() as u32).to_le_bytes());
    let mut ents = Vec::new();
    for (id, enc) in &objects {
        pack.extend_from_slice(&(enc.len() as u32).to_le_bytes());
        ents.push((*id, pack.len() as u64, enc.len() as u32));
        pack.extend_from_slice(enc);
    }
    let sum = *blake3::hash(&pack).as_bytes();
    pack.extend_from_slice(&sum);
    ents.sort_by_key(|e| e.0);
    let mut idx = Vec::new();
    idx.extend_from_slice(IDX_MAGIC);
    idx.extend_from_slice(&(ents.len() as u32).to_le_bytes());
    idx.extend_from_slice(&(pack.len() as u64).to_le_bytes());
    idx.extend_from_slice(&sum);
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

/// Write a pack + index for `objects` (id, encoded bytes) into `dir`; returns the opened pack.
/// Pack is renamed into place first, index last: a pack without an index is ignored.
pub fn write_pack(dir: &Path, tmp_dir: &Path, mut objects: Vec<(ObjectId, Vec<u8>)>) -> Result<Pack, StoreError> {
    objects.sort_by_key(|o| o.0);
    io(dir, fs::create_dir_all(dir))?;
    let built = build_pack(objects);
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
