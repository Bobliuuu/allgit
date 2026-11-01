//! Read one object out of a remote pack with a ranged GET, instead of downloading the pack.

use crate::backend::{Backend, BackendError};
use std::collections::HashMap;
use std::path::Path;
use vcs_objects::{id_of_encoded, ObjectId};
use vcs_store::pack::PackIndex;

/// Check the remote pack really ends where its index says: the last 32 bytes (the trailer) must
/// equal the index's pack checksum and nothing may follow. One 33-byte ranged GET. Ranged object
/// reads never touch the trailer, so without this a pack cut short by a half-finished upload,
/// or one with extra bytes, would be accepted.
fn check_trailer(b: &dyn Backend, stem: &str, idx: &PackIndex) -> Result<(), BackendError> {
    let bad = |why: &str| BackendError::Other(format!("pack {stem}: {why}"));
    let start = idx.pack_len.checked_sub(32).ok_or_else(|| bad("index pack length too small"))?;
    match b.get_range(&format!("{stem}.pack"), start, 33)? {
        None => Err(bad("pack file missing")),
        Some(t) if t.len() == 32 && t[..] == idx.checksum()[..] => Ok(()),
        Some(t) if t.len() < 32 => Err(bad("pack is shorter than its index says (truncated trailer?)")),
        Some(t) if t.len() > 32 => Err(bad("pack is longer than its index says")),
        Some(_) => Err(bad("pack trailer does not match its index")),
    }
}

/// Fetch `id` from the pack stored at `<stem>.pack` / `<stem>.idx`. Downloads the index
/// (small, 44 bytes per object) and then exactly the object's byte range. The bytes are
/// verified against `id`. `Ok(None)` if the pack, index or object is absent.
pub fn fetch_from_pack(b: &dyn Backend, stem: &str, id: &ObjectId) -> Result<Option<Vec<u8>>, BackendError> {
    let Some(idx) = b.get(&format!("{stem}.idx"))? else {
        return Ok(None);
    };
    let idx = PackIndex::parse(&idx.data, Path::new(stem)).map_err(|e| BackendError::Other(e.to_string()))?;
    let Some((off, len)) = idx.find(id) else { return Ok(None) };
    check_trailer(b, stem, &idx)?;
    let Some(bytes) = b.get_range(&format!("{stem}.pack"), off, len as u64)? else {
        return Ok(None);
    };
    if bytes.len() != len as usize || id_of_encoded(&bytes) != *id {
        return Err(BackendError::Other(format!("pack {stem}: object {id} fails hash verification")));
    }
    Ok(Some(bytes))
}

/// Key prefix under which a remote keeps packs: `packs/<name>.pack` and `packs/<name>.idx`.
pub const PACK_PREFIX: &str = "packs/";

/// Look for `id` in every pack under [`PACK_PREFIX`]. `Ok(None)` if no pack has it.
/// Cost: one `list`, then per pack one index GET (until found) and one ranged GET.
pub fn fetch_from_any_pack(b: &dyn Backend, id: &ObjectId) -> Result<Option<Vec<u8>>, BackendError> {
    for key in b.list(PACK_PREFIX)? {
        if let Some(stem) = key.strip_suffix(".idx") {
            if let Some(bytes) = fetch_from_pack(b, stem, id)? {
                return Ok(Some(bytes));
            }
        }
    }
    Ok(None)
}

/// Maximum unwanted bytes read between two wanted ranges to keep them in one request.
const COALESCE_GAP: u64 = 64 * 1024;

/// Fetch several objects at once. Packs are listed once and each index read once; wanted
/// ranges that are close together in a pack are merged into one ranged GET; ids not in any
/// pack are read loose, one GET each. Returned bytes are verified against their ids. Errors if
/// any id is absent everywhere. Compare with calling [`fetch_from_any_pack`] per id, which
/// repeats the list and index reads every time.
pub fn fetch_many(b: &dyn Backend, ids: &[ObjectId]) -> Result<Vec<(ObjectId, Vec<u8>)>, BackendError> {
    let mut want: Vec<ObjectId> = ids.to_vec();
    want.sort();
    want.dedup();
    let mut out = Vec::with_capacity(want.len());
    let mut remaining: Vec<ObjectId> = want;
    if !remaining.is_empty() {
        for key in b.list(PACK_PREFIX)? {
            let Some(stem) = key.strip_suffix(".idx") else { continue };
            if remaining.is_empty() {
                break;
            }
            let Some(idx) = b.get(&key)? else { continue };
            let idx = PackIndex::parse(&idx.data, Path::new(stem)).map_err(|e| BackendError::Other(e.to_string()))?;
            let mut hits: Vec<(u64, u32, ObjectId)> = Vec::new();
            remaining.retain(|id| match idx.find(id) {
                Some((off, len)) => {
                    hits.push((off, len, *id));
                    false
                }
                None => true,
            });
            hits.sort_by_key(|h| h.0);
            if !hits.is_empty() {
                check_trailer(b, stem, &idx)?;
            }
            let pack_key = format!("{stem}.pack");
            let mut i = 0;
            while i < hits.len() {
                let start = hits[i].0;
                let mut end = hits[i].0 + hits[i].1 as u64;
                let mut j = i + 1;
                while j < hits.len() && hits[j].0 <= end + COALESCE_GAP {
                    end = end.max(hits[j].0 + hits[j].1 as u64);
                    j += 1;
                }
                let Some(buf) = b.get_range(&pack_key, start, end - start)? else {
                    return Err(BackendError::Other(format!("pack {stem}: pack file missing")));
                };
                for (off, len, id) in &hits[i..j] {
                    let s = (off - start) as usize;
                    let e = s + *len as usize;
                    let Some(bytes) = buf.get(s..e) else {
                        return Err(BackendError::Other(format!("pack {stem}: short read for {id}")));
                    };
                    if id_of_encoded(bytes) != *id {
                        return Err(BackendError::Other(format!("pack {stem}: object {id} fails hash verification")));
                    }
                    out.push((*id, bytes.to_vec()));
                }
                i = j;
            }
        }
    }
    for id in remaining {
        let h = id.to_hex();
        let key = format!("objects/{}/{}", &h[..2], &h[2..]);
        match b.get(&key)? {
            Some(v) => out.push((id, v.data)),
            None => return Err(BackendError::Other(format!("object {id} missing on remote"))),
        }
    }
    Ok(out)
}

/// First ranged GET when streaming the metadata prefix of a pack, then x4 per request up to the
/// cap. Small first so a pack that is mostly file data costs little over-read.
const PREFIX_FIRST: u64 = 4 * 1024;
const PREFIX_MAX: u64 = 4 * 1024 * 1024;
/// Header of a pack: magic + u32 count; each entry is a u32 length then the encoded object.
const PACK_HEADER: u64 = 12;

/// How much of a pack to read ahead the first time a walk needs something from it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadAhead {
    /// Only the object asked for (one ranged GET each).
    None,
    /// The leading commits and trees (packs written by [`crate::Publisher`] put them first),
    /// in a few ranged GETs that grow geometrically and stop at the first file blob.
    Metadata,
    /// The whole pack in one GET.
    Whole,
}

/// Reads objects from a remote that may hold packs, loose objects, or both. The pack list and
/// indexes are fetched once (`1 + packs` requests) and then answer "which pack has it" from
/// memory, so a walk over thousands of objects costs a few large ranged GETs, not LISTs plus
/// one GET per object. A pack the walk never touches is never read. Everything returned is
/// checked against the id that was asked for.
pub struct RemoteSource<'a> {
    b: &'a dyn Backend,
    ahead: ReadAhead,
    packs: Option<Vec<(String, PackIndex)>>,
    read_ahead_done: Vec<bool>,
    /// Pre-read objects keyed by the hash of their bytes, so a lying pack cannot plant data.
    cache: HashMap<ObjectId, Vec<u8>>,
}

impl<'a> RemoteSource<'a> {
    pub fn new(b: &'a dyn Backend, ahead: ReadAhead) -> Self {
        Self {
            b,
            ahead,
            packs: None,
            read_ahead_done: vec![],
            cache: HashMap::new(),
        }
    }

    fn packs(&mut self) -> Result<&[(String, PackIndex)], BackendError> {
        if self.packs.is_none() {
            let mut v = Vec::new();
            for key in self.b.list(PACK_PREFIX)? {
                let Some(stem) = key.strip_suffix(".idx") else { continue };
                let Some(idx) = self.b.get(&key)? else { continue };
                let idx = PackIndex::parse(&idx.data, Path::new(stem)).map_err(|e| BackendError::Other(e.to_string()))?;
                v.push((stem.to_owned(), idx));
            }
            self.read_ahead_done = vec![false; v.len()];
            self.packs = Some(v);
        }
        Ok(self.packs.as_deref().unwrap_or(&[]))
    }

    fn read_ahead(&mut self, i: usize) -> Result<(), BackendError> {
        let (stem, pack_len) = {
            let (s, idx) = &self.packs.as_ref().expect("packs loaded")[i];
            (s.clone(), idx.pack_len)
        };
        let key = format!("{stem}.pack");
        let end = pack_len.saturating_sub(32);
        if self.ahead == ReadAhead::Whole {
            if let Some(v) = self.b.get(&key)? {
                let to = (end as usize).min(v.data.len());
                absorb(&mut self.cache, &v.data, PACK_HEADER as usize, to, false);
            }
            return Ok(());
        }
        let (mut pos, mut want) = (PACK_HEADER, PREFIX_FIRST);
        while pos < end {
            let Some(buf) = self.b.get_range(&key, pos, want.min(end - pos))? else {
                break;
            };
            let (used, stop) = absorb(&mut self.cache, &buf, 0, buf.len(), true);
            pos += used as u64;
            // stop: first non-metadata entry; used == 0: one entry larger than the request
            if stop || used == 0 {
                break;
            }
            want = (want * 4).min(PREFIX_MAX);
        }
        Ok(())
    }

    /// The verified encoded bytes of `id`: from read-ahead, else one ranged GET from the pack
    /// that indexes it, else one GET of the loose object. `Ok(None)` if the remote lacks it.
    pub fn fetch(&mut self, id: &ObjectId) -> Result<Option<Vec<u8>>, BackendError> {
        if let Some(v) = self.cache.remove(id) {
            return Ok(Some(v));
        }
        let hit = self
            .packs()?
            .iter()
            .enumerate()
            .find_map(|(i, (stem, idx))| idx.find(id).map(|(o, l)| (i, format!("{stem}.pack"), o, l)));
        if let Some((i, key, off, len)) = hit {
            if self.ahead != ReadAhead::None && !self.read_ahead_done[i] {
                self.read_ahead_done[i] = true;
                self.read_ahead(i)?;
                if let Some(v) = self.cache.remove(id) {
                    return Ok(Some(v));
                }
            }
            let bytes = self
                .b
                .get_range(&key, off, len as u64)?
                .ok_or_else(|| BackendError::Other(format!("pack {key}: pack file missing")))?;
            if bytes.len() != len as usize || id_of_encoded(&bytes) != *id {
                return Err(BackendError::Other(format!("object {id} fails hash verification")));
            }
            return Ok(Some(bytes));
        }
        let h = id.to_hex();
        Ok(self.b.get(&format!("objects/{}/{}", &h[..2], &h[2..]))?.map(|v| v.data))
    }
}

/// Parse `[u32 len][object]` frames from `buf[from..to]` into `cache`. With `meta_only`, stop at
/// the first object that is not a tree or commit. Returns (bytes consumed from `from`, stopped).
fn absorb(cache: &mut HashMap<ObjectId, Vec<u8>>, buf: &[u8], from: usize, to: usize, meta_only: bool) -> (usize, bool) {
    let mut p = from;
    while p + 4 <= to {
        let len = u32::from_le_bytes(buf[p..p + 4].try_into().unwrap()) as usize;
        let Some(obj) = p.checked_add(4 + len).filter(|e| *e <= to).map(|e| &buf[p + 4..e]) else {
            break;
        };
        // kinds: 2 = tree, 3 = commit (vcs_objects::Kind)
        if meta_only && !matches!(obj.first(), Some(2 | 3)) {
            return (p - from, true);
        }
        cache.insert(id_of_encoded(obj), obj.to_vec());
        p += 4 + len;
    }
    (p - from, false)
}

/// Upload a local pack (and its index) as `packs/<name>`. The pack goes first, so a reader
/// never sees an index without its pack. Create-only: an existing name is left unchanged.
pub fn upload_pack(b: &dyn Backend, name: &str, pack_path: &Path) -> Result<(), BackendError> {
    let other = |e: std::io::Error| BackendError::Other(e.to_string());
    let pack = std::fs::read(pack_path).map_err(other)?;
    let idx = std::fs::read(pack_path.with_extension("idx")).map_err(other)?;
    b.put_if_absent(&format!("{PACK_PREFIX}{name}.pack"), &pack)?;
    b.put_if_absent(&format!("{PACK_PREFIX}{name}.idx"), &idx)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemBackend;
    use vcs_objects::Object;
    use vcs_store::pack::write_pack;

    #[test]
    fn reads_one_object_by_range_not_whole_pack() {
        let d = tempfile::tempdir().unwrap();
        let objs: Vec<_> = (0..50)
            .map(|i| {
                let enc = Object::Blob(vec![i as u8; 10_000]).encode();
                (id_of_encoded(&enc), enc)
            })
            .collect();
        let want = objs[17].clone();
        let pack = write_pack(&d.path().join("pack"), d.path(), objs).unwrap();
        let idx_path = pack.pack_path.with_extension("idx");
        let b = MemBackend::new();
        let pack_bytes = std::fs::read(&pack.pack_path).unwrap();
        b.put_if_absent("p/x.pack", &pack_bytes).unwrap();
        b.put_if_absent("p/x.idx", &std::fs::read(&idx_path).unwrap()).unwrap();
        let before = b.bytes_read();
        let got = fetch_from_pack(&b, "p/x", &want.0).unwrap().unwrap();
        assert_eq!(got, want.1);
        let used = b.bytes_read() - before;
        assert!(
            used < pack_bytes.len() as u64 / 10,
            "read {used} of {} pack bytes",
            pack_bytes.len()
        );
        assert_eq!(fetch_from_pack(&b, "p/x", &ObjectId::from_bytes([9; 32])).unwrap(), None);
        assert_eq!(fetch_from_pack(&b, "p/none", &want.0).unwrap(), None);
        // corrupt the pack on the remote: the ranged bytes must fail verification
        let mut bad = pack_bytes.clone();
        for x in bad.iter_mut().skip(12) {
            *x ^= 0xFF;
        }
        b.tamper("p/x.pack", bad);
        assert!(fetch_from_pack(&b, "p/x", &want.0).is_err());
    }
}
