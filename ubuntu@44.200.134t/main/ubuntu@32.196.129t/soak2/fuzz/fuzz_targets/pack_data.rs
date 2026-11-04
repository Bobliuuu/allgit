#![no_main]
use libfuzzer_sys::fuzz_target;
use vcs_objects::Object;
use vcs_store::pack::{write_pack, Pack};

// A valid pack built from fuzzer-chosen blobs, then damaged by fuzzer-chosen byte edits in
// the pack file and (optionally resealed) index, then opened, read and verified.
fuzz_target!(|input: (Vec<Vec<u8>>, Vec<(u16, u8)>, Vec<(u16, u8)>, bool)| {
    let (blobs, pack_edits, idx_edits, reseal) = input;
    if blobs.is_empty() || blobs.len() > 32 {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let objs = blobs
        .into_iter()
        .map(|b| {
            let o = Object::Blob(b);
            (o.id(), o.encode())
        })
        .collect();
    let p = write_pack(&dir.path().join("p"), &tmp, objs).unwrap();
    let idx_path = p.pack_path.with_extension("idx");
    let mut pack = std::fs::read(&p.pack_path).unwrap();
    let mut idx = std::fs::read(&idx_path).unwrap();
    for (i, x) in pack_edits {
        let n = pack.len();
        pack[i as usize % n] ^= x;
    }
    for (i, x) in idx_edits {
        let n = idx.len();
        idx[i as usize % n] ^= x;
    }
    if reseal {
        idx.truncate(idx.len() - 32);
        let sum = *blake3::hash(&idx).as_bytes();
        idx.extend_from_slice(&sum);
    }
    std::fs::write(&p.pack_path, &pack).unwrap();
    std::fs::write(&idx_path, &idx).unwrap();
    if let Ok(q) = Pack::open(&idx_path) {
        let ids: Vec<_> = q.ids().collect();
        for id in ids {
            let _ = q.read(&id);
        }
        let _ = q.verify();
    }
});
