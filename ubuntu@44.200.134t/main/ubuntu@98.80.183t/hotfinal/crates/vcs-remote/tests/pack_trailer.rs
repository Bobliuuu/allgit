//! A remote pack whose trailer was cut, altered, or followed by extra bytes must be rejected by
//! ranged fetch (single and batch), even though every object's own bytes still verify.
use vcs_objects::{id_of_encoded, Object, ObjectId};
use vcs_remote::{fetch_many, MemBackend};
use vcs_store::pack::write_pack;

use vcs_remote::Backend;

fn remote(pack_edit: &dyn Fn(Vec<u8>) -> Vec<u8>) -> (MemBackend, Vec<ObjectId>) {
    let d = tempfile::tempdir().unwrap();
    let objs: Vec<(ObjectId, Vec<u8>)> = (0..6u8)
        .map(|i| {
            let e = Object::Blob(vec![i; 3000]).encode();
            (id_of_encoded(&e), e)
        })
        .collect();
    let ids = objs.iter().map(|o| o.0).collect();
    let p = write_pack(&d.path().join("pack"), d.path(), objs).unwrap();
    let b = MemBackend::new();
    b.put_if_absent("packs/p1.pack", &pack_edit(std::fs::read(&p.pack_path).unwrap()))
        .unwrap();
    b.put_if_absent("packs/p1.idx", &std::fs::read(p.pack_path.with_extension("idx")).unwrap())
        .unwrap();
    (b, ids)
}

#[test]
fn intact_pack_is_accepted() {
    let (b, ids) = remote(&|p| p);
    assert_eq!(fetch_many(&b, &ids).unwrap().len(), ids.len());
    assert!(vcs_remote::fetch_object(&b, &ids[0]).is_ok());
}

#[test]
fn pack_missing_only_its_trailer_is_rejected() {
    for cut in [1usize, 16, 32] {
        let (b, ids) = remote(&move |mut p| {
            p.truncate(p.len() - cut);
            p
        });
        let e = fetch_many(&b, &ids).unwrap_err().to_string();
        assert!(e.contains("trailer") || e.contains("shorter"), "cut {cut}: {e}");
        assert!(vcs_remote::fetch_object(&b, &ids[0]).is_err(), "cut {cut}: single path accepted it");
    }
}

#[test]
fn altered_or_extended_trailer_is_rejected() {
    let (b, ids) = remote(&|mut p| {
        let n = p.len();
        p[n - 1] ^= 1;
        p
    });
    assert!(fetch_many(&b, &ids).is_err());
    assert!(vcs_remote::fetch_object(&b, &ids[0]).is_err());
    let (b, ids) = remote(&|mut p| {
        p.push(0);
        p
    });
    assert!(fetch_many(&b, &ids).is_err());
    assert!(vcs_remote::fetch_object(&b, &ids[0]).is_err());
}
