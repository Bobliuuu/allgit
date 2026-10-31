//! Read one object out of a remote pack with a ranged GET, instead of downloading the pack.

use crate::backend::{Backend, BackendError};
use std::path::Path;
use vcs_objects::{id_of_encoded, ObjectId};
use vcs_store::pack::PackIndex;

/// Fetch `id` from the pack stored at `<stem>.pack` / `<stem>.idx`. Downloads the index
/// (small, 44 bytes per object) and then exactly the object's byte range. The bytes are
/// verified against `id`. `Ok(None)` if the pack, index or object is absent.
pub fn fetch_from_pack(b: &dyn Backend, stem: &str, id: &ObjectId) -> Result<Option<Vec<u8>>, BackendError> {
    let Some(idx) = b.get(&format!("{stem}.idx"))? else {
        return Ok(None);
    };
    let idx = PackIndex::parse(&idx.data, Path::new(stem)).map_err(|e| BackendError::Other(e.to_string()))?;
    let Some((off, len)) = idx.find(id) else { return Ok(None) };
    let Some(bytes) = b.get_range(&format!("{stem}.pack"), off, len as u64)? else {
        return Ok(None);
    };
    if bytes.len() != len as usize || id_of_encoded(&bytes) != *id {
        return Err(BackendError::Other(format!("pack {stem}: object {id} failed verification")));
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
                        return Err(BackendError::Other(format!("pack {stem}: object {id} failed verification")));
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
