//! Pack/index format v2 (codec byte, zstd 3 with raw fallback, fanout index) and v1 compatibility.
//! The v1 fixtures in tests/fixtures/v1 were produced by the pre-v2 `build_pack`.
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use vcs_objects::{id_of_encoded, Object, ObjectId};
use vcs_store::pack::*;
use vcs_store::{LocalStore, ObjectStore};

fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// Same generator the v1 fixture was made with.
fn fixture_objects() -> Vec<(ObjectId, Vec<u8>)> {
    let mut v = Vec::new();
    for i in 0..40u32 {
        v.push(Object::Blob(format!("fixture object {i}\n").repeat(1 + i as usize * 3).into_bytes()).encode());
    }
    for i in 0..3u64 {
        v.push(Object::Blob(noise(i + 1, 300)).encode());
    }
    v.into_iter().map(|e| (id_of_encoded(&e), e)).collect()
}

fn fixture_dir(t: &Path) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v1");
    for e in ["pack", "idx"] {
        fs::copy(src.join(format!("fixture.{e}")), t.join(format!("pack-fixture.{e}"))).unwrap();
    }
    t.join("pack-fixture.idx")
}

fn write(dir: &Path, objs: Vec<(ObjectId, Vec<u8>)>, fmt: PackFormat) -> Pack {
    write_pack_fmt(&dir.join("pack"), dir, objs, fmt).unwrap()
}

#[test]
fn committed_v1_fixture_is_byte_identical_to_v1_writer_and_readable() {
    let built = build_pack_fmt(fixture_objects(), PackFormat::V1);
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v1");
    assert_eq!(built.pack, fs::read(src.join("fixture.pack")).unwrap());
    assert_eq!(built.idx, fs::read(src.join("fixture.idx")).unwrap());

    let d = tempfile::tempdir().unwrap();
    let p = Pack::open(&fixture_dir(d.path())).unwrap();
    assert_eq!(p.version(), 1);
    assert!(p.verify().is_empty());
    for (id, enc) in fixture_objects() {
        assert_eq!(p.read(&id).unwrap().unwrap(), enc);
    }
}

#[test]
fn v2_roundtrip_ids_unchanged_and_verify_clean() {
    let d = tempfile::tempdir().unwrap();
    let objs = fixture_objects();
    let p = write(d.path(), objs.clone(), PackFormat::V2);
    assert_eq!(p.version(), 2);
    for (id, enc) in &objs {
        let got = p.read(id).unwrap().unwrap();
        assert_eq!(&got, enc);
        assert_eq!(id_of_encoded(&got), *id, "ids hash the uncompressed encoding");
    }
    assert_eq!(p.read(&ObjectId::from_bytes([7; 32])).unwrap(), None);
    assert!(p.verify().is_empty(), "{:?}", p.verify());
    // compressible data really shrinks the pack
    let v1 = build_pack_fmt(objs.clone(), PackFormat::V1);
    let v2 = build_pack_fmt(objs, PackFormat::V2);
    assert!(v2.pack.len() < v1.pack.len() / 2, "v2 {} vs v1 {}", v2.pack.len(), v1.pack.len());
}

#[test]
fn incompressible_objects_are_stored_raw_and_never_expand_beyond_the_codec_byte() {
    for (i, n) in [(1u64, 0usize), (2, 1), (3, 40), (4, 5000)] {
        let enc = Object::Blob(noise(i, n)).encode();
        let pl = encode_payload(PackFormat::V2, &enc);
        assert_eq!(pl[0], CODEC_RAW, "n={n}");
        assert_eq!(pl.len(), enc.len() + 1);
        assert_eq!(&decode_payload(2, &pl).unwrap()[..], &enc[..]);
    }
    let enc = Object::Blob(vec![b'a'; 5000]).encode();
    let pl = encode_payload(PackFormat::V2, &enc);
    assert_eq!(pl[0], CODEC_ZSTD);
    assert!(pl.len() < enc.len());
    assert_eq!(&decode_payload(2, &pl).unwrap()[..], &enc[..]);
}

#[test]
fn bad_codec_and_garbage_zstd_are_errors_not_panics() {
    assert!(decode_payload(2, &[]).is_err());
    assert!(decode_payload(2, &[9, 1, 2]).unwrap_err().contains("codec"));
    assert!(decode_payload(2, &[CODEC_ZSTD, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]).is_err());
    // a frame header claiming a huge size is refused before allocating
    let mut p = vec![CODEC_ZSTD];
    p.extend_from_slice(&[0x28, 0xb5, 0x2f, 0xfd, 0x60]); // magic, FCS field flag = 8 bytes
    p.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0x7f]);
    p.extend_from_slice(&[0; 16]);
    assert!(decode_payload(2, &p).is_err());
}

#[test]
fn v2_index_layout_and_fanout() {
    let objs = fixture_objects();
    let built = build_pack_fmt(objs.clone(), PackFormat::V2);
    assert_eq!(&built.idx[..8], IDX_MAGIC_V2);
    assert_eq!(&built.pack[..8], PACK_MAGIC_V2);
    let idx = PackIndex::parse(&built.idx, Path::new("t")).unwrap();
    assert_eq!(idx.len(), objs.len());
    assert_eq!(built.idx.len(), 52 + 1024 + objs.len() * 44 + 32);
    for (id, _) in &objs {
        assert!(idx.find(id).is_some());
    }
    // misses: every first byte, including ones with empty buckets
    for b in 0..=255u8 {
        let mut x = [b; 32];
        x[31] = 0xAB;
        let id = ObjectId::from_bytes(x);
        assert_eq!(idx.find(&id).is_some(), objs.iter().any(|o| o.0 == id));
    }
    let sorted: Vec<_> = idx.iter().map(|e| e.0).collect();
    assert!(sorted.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn corruption_is_detected() {
    let d = tempfile::tempdir().unwrap();
    let p = write(d.path(), fixture_objects(), PackFormat::V2);
    let (pack_path, idx_path) = (p.pack_path.clone(), p.pack_path.with_extension("idx"));
    let (pb, ib) = (fs::read(&pack_path).unwrap(), fs::read(&idx_path).unwrap());
    drop(p);

    // flipped byte in an object payload: fsck reports it, a read of that object errors or fails hash
    let mut bad = pb.clone();
    bad[40] ^= 0x55;
    fs::write(&pack_path, &bad).unwrap();
    let p = Pack::open(&idx_path).unwrap();
    assert!(!p.verify().is_empty());
    fs::write(&pack_path, &pb).unwrap();

    // flipped byte in an index id: opens lazily (no whole-index hash), but fsck and strict parse catch it
    let mut bad = ib.clone();
    bad[52 + 1024 + 5] ^= 0x01;
    fs::write(&idx_path, &bad).unwrap();
    let p = Pack::open(&idx_path).expect("lazy open does not hash the index");
    assert!(p.verify().iter().any(|m| m.contains("index")), "{:?}", p.verify());
    assert!(PackIndex::parse(&bad, Path::new("t")).is_err());

    // flipped offset byte: lookup bounds check or the object hash catches it
    let mut bad = ib.clone();
    bad[52 + 1024 + 32 + 1] ^= 0x40;
    fs::write(&idx_path, &bad).unwrap();
    let p = Pack::open(&idx_path).unwrap();
    assert!(!p.verify().is_empty());

    // damaged fanout: refused at open
    let mut bad = ib.clone();
    bad[52 + 10 * 4] ^= 0xFF;
    fs::write(&idx_path, &bad).unwrap();
    assert!(Pack::open(&idx_path).is_err());

    // truncated pack and truncated index: refused at open
    fs::write(&idx_path, &ib).unwrap();
    fs::write(&pack_path, &pb[..pb.len() - 1]).unwrap();
    assert!(Pack::open(&idx_path).is_err());
    fs::write(&pack_path, &pb).unwrap();
    for cut in [1usize, 32, 100, 1100] {
        fs::write(&idx_path, &ib[..ib.len() - cut]).unwrap();
        assert!(Pack::open(&idx_path).is_err(), "cut {cut}");
    }
    // v1 index corruption is still caught at open
    let d1 = tempfile::tempdir().unwrap();
    let ip = fixture_dir(d1.path());
    let mut b = fs::read(&ip).unwrap();
    b[60] ^= 1;
    fs::write(&ip, b).unwrap();
    assert!(Pack::open(&ip).is_err());
}

fn blob_ids(s: &LocalStore, n: usize, tag: &str) -> Vec<ObjectId> {
    (0..n)
        .map(|i| s.put_blob(format!("{tag} blob {i} {}", "x".repeat(i * 7)).as_bytes()).unwrap())
        .collect()
}

#[test]
fn mixed_v1_and_v2_packs_in_one_store_fsck_repack_and_gc() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("objects");
    {
        // an old binary's v1 pack sitting in the store
        fs::create_dir_all(dir.join("tmp")).unwrap();
        write_pack_fmt(&dir.join("pack"), &dir.join("tmp"), fixture_objects(), PackFormat::V1).unwrap();
    }
    let s = LocalStore::open(&dir).unwrap();
    let new_ids = blob_ids(&s, 30, "new");
    assert_eq!(s.repack_as(PackFormat::V2, false).unwrap(), (30, 0));
    // both generations readable, fsck clean
    for (id, _) in fixture_objects() {
        assert!(s.contains(&id).unwrap());
    }
    for id in &new_ids {
        s.get(id).unwrap();
    }
    assert!(s.verify_packs().is_empty());
    let versions = |dir: &Path| -> BTreeSet<u8> {
        fs::read_dir(dir.join("pack"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "idx"))
            .map(|e| Pack::open(&e.path()).unwrap().version())
            .collect()
    };
    assert_eq!(versions(&dir), BTreeSet::from([1, 2]));

    // GC rewrite of a v2 pack and of a v1 pack: survivors stay readable, packs are valid
    let doomed: BTreeSet<ObjectId> = [new_ids[0], new_ids[1], fixture_objects()[0].0].into_iter().collect();
    let st = s.sweep(&doomed).unwrap();
    assert_eq!(st.packs_rewritten, 2);
    assert!(!s.contains(&new_ids[0]).unwrap());
    s.get(&new_ids[2]).unwrap();
    assert!(s.contains(&fixture_objects()[1].0).unwrap());
    assert!(s.verify_packs().is_empty());
    drop(s);

    // rollback: repack --format v1 rewrites every pack as v1, and a reopen reads them all
    let s = LocalStore::open(&dir).unwrap();
    let (_, converted) = s.repack_as(PackFormat::V1, true).unwrap();
    assert!(converted >= 1);
    assert_eq!(versions(&dir), BTreeSet::from([1]));
    let s2 = LocalStore::open(&dir).unwrap();
    s2.get(&new_ids[5]).unwrap();
    assert!(s2.verify_packs().is_empty());
    // and forward again
    s2.repack_as(PackFormat::V2, true).unwrap();
    assert_eq!(versions(&dir), BTreeSet::from([2]));
    s2.get(&new_ids[5]).unwrap();
    assert!(s2.verify_packs().is_empty());
}

#[test]
fn many_packs_miss_lookup() {
    // 64 small v2 packs: a miss is answered per pack from its fanout bucket
    let d = tempfile::tempdir().unwrap();
    let mut packs = Vec::new();
    for k in 0..64u64 {
        let objs: Vec<_> = (0..50u64)
            .map(|i| {
                let e = Object::Blob(noise(k * 1000 + i, 64)).encode();
                (id_of_encoded(&e), e)
            })
            .collect();
        let built = build_pack_fmt(objs, PackFormat::V2);
        packs.push(PackIndex::parse(&built.idx, Path::new("t")).unwrap());
    }
    let missing = ObjectId::from_bytes([0xEE; 32]);
    assert!(packs.iter().all(|p| p.find(&missing).is_none()));
    let _ = d;
}
