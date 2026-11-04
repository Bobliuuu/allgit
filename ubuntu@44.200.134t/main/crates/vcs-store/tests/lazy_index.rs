//! A pack opened from disk reads its index on demand; answers equal the fully parsed index, and
//! damage is still caught (at open for structure, on use or by `verify` for content).
use std::path::Path;
use vcs_objects::{Object, ObjectId};
use vcs_store::pack::{build_pack_fmt, PackFormat, PackIndex};

fn objects(n: usize) -> Vec<(ObjectId, Vec<u8>)> {
    (0..n)
        .map(|i| {
            let o = Object::Blob(format!("object number {i} {}", "x".repeat(i % 50)).into_bytes());
            let enc = o.encode();
            (vcs_objects::id_of_encoded(&enc), enc)
        })
        .collect()
}

#[test]
fn file_backed_index_answers_like_the_parsed_one() {
    for fmt in [PackFormat::V2, PackFormat::V1] {
        let objs = objects(700);
        let built = build_pack_fmt(objs.clone(), fmt);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.idx");
        std::fs::write(&p, &built.idx).unwrap();
        let lazy = PackIndex::open_lazy(&p).unwrap();
        let full = PackIndex::parse(&built.idx, Path::new("t")).unwrap();
        assert_eq!(lazy.len(), full.len());
        assert_eq!(lazy.checksum(), full.checksum());
        assert_eq!(lazy.iter().collect::<Vec<_>>(), full.iter().collect::<Vec<_>>());
        for (id, _) in &objs {
            assert_eq!(lazy.lookup(id).unwrap(), full.lookup(id).unwrap());
            assert!(lazy.find(id).is_some());
        }
        for b in 0..=255u8 {
            let mut x = [b; 32];
            x[31] = 0xAB;
            let id = ObjectId::from_bytes(x);
            assert_eq!(lazy.find(&id), full.find(&id));
        }
        lazy.verify_full().unwrap();
    }
}

#[test]
fn file_backed_index_still_rejects_damage() {
    let built = build_pack_fmt(objects(300), PackFormat::V2);
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.idx");
    // truncated or padded: refused when opened
    for bad in [
        built.idx[..built.idx.len() - 1].to_vec(),
        [built.idx.clone(), vec![0]].concat(),
        built.idx[..40].to_vec(),
        Vec::new(),
    ] {
        std::fs::write(&p, &bad).unwrap();
        assert!(PackIndex::open_lazy(&p).is_err());
    }
    // a flipped byte in the entry area passes the structural open but fails the full check
    let mut b = built.idx.clone();
    let at = 52 + 1024 + 10;
    b[at] ^= 0xff;
    std::fs::write(&p, &b).unwrap();
    let lazy = PackIndex::open_lazy(&p).unwrap();
    assert!(lazy.verify_full().is_err());
}
