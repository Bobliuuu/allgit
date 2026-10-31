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
