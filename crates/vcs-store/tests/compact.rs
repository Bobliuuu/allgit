//! Local pack compaction (`LocalStore::compact_packs`).

use vcs_store::pack::PackFormat;
use vcs_store::{LocalStore, ObjectStore};

fn many_packs(n: usize) -> (tempfile::TempDir, std::path::PathBuf, Vec<vcs_objects::ObjectId>) {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("objects");
    let s = LocalStore::open(&dir).unwrap();
    let mut ids = vec![];
    for p in 0..n {
        for i in 0..5 {
            ids.push(s.put_blob(format!("pack {p} blob {i}").as_bytes()).unwrap());
        }
        assert_eq!(s.repack().unwrap(), 5);
    }
    assert_eq!(s.pack_count(), n);
    (d, dir, ids)
}

#[test]
fn small_packs_merge_and_everything_stays_readable_across_reopen() {
    let (_d, dir, ids) = many_packs(9);
    let s = LocalStore::open(&dir).unwrap();
    let c = s.compact_packs(PackFormat::V2).unwrap();
    assert!(c.merges >= 1 && c.packs_after < 9, "{c:?}");
    assert_eq!(c.packs_after, s.pack_count());
    assert!(c.packs_after <= 3, "{c:?}");
    assert!(s.verify_packs().is_empty());
    for reopen in [false, true] {
        let s2;
        let s = if reopen {
            s2 = LocalStore::open(&dir).unwrap();
            &s2
        } else {
            &s
        };
        for (n, id) in ids.iter().enumerate() {
            assert_eq!(s.get_blob(id).unwrap(), format!("pack {} blob {}", n / 5, n % 5).as_bytes());
        }
        assert_eq!(s.pack_count(), c.packs_after);
    }
    // no stray files: exactly one .pack and one .idx per pack
    let names: Vec<_> = std::fs::read_dir(dir.join("pack"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.iter().filter(|n| n.ends_with(".idx")).count(), c.packs_after);
    assert_eq!(names.iter().filter(|n| n.ends_with(".pack")).count(), c.packs_after);
    // a second pass has nothing to do
    assert_eq!(s.compact_packs(PackFormat::V2).unwrap().merges, 0);
}

#[test]
fn a_reader_opened_before_the_merge_still_finds_objects() {
    let (_d, dir, ids) = many_packs(8);
    let old_reader = LocalStore::open(&dir).unwrap();
    let writer = LocalStore::open(&dir).unwrap();
    assert!(writer.compact_packs(PackFormat::V2).unwrap().merges >= 1);
    for id in &ids {
        assert!(old_reader.get_blob(id).is_ok(), "reader that predates the merge lost {id}");
    }
}

#[test]
fn rollback_format_v1_is_honoured() {
    let (_d, dir, ids) = many_packs(6);
    let s = LocalStore::open(&dir).unwrap();
    s.compact_packs(PackFormat::V1).unwrap();
    assert!(s.verify_packs().is_empty());
    for id in &ids {
        s.get_blob(id).unwrap();
    }
}

// ---------------------------------------------------------------- pack format v3 (blocks)

use vcs_objects::{id_of_encoded, Object};
use vcs_store::pack::{build_pack_blocks, decode_block, write_pack_fmt, Pack, PackIndex};

fn objs(n: usize, size: usize) -> Vec<(vcs_objects::ObjectId, Vec<u8>)> {
    (0..n)
        .map(|i| {
            let mut body = format!("object {i} ").into_bytes();
            body.resize(size, b'x');
            let enc = Object::Blob(body).encode();
            (id_of_encoded(&enc), enc)
        })
        .collect()
}

#[test]
fn v3_pack_roundtrips_across_blocks_and_verifies() {
    let d = tempfile::tempdir().unwrap();
    let list = objs(300, 3000); // ~900 KB: several blocks at the default size? force small blocks below
    let b = build_pack_blocks(list.clone(), PackFormat::V3, 64 * 1024);
    let idx = PackIndex::parse(&b.idx, std::path::Path::new("x")).unwrap();
    assert_eq!(idx.version(), 3);
    assert_eq!(idx.len(), 300);
    let blocks: std::collections::BTreeSet<u64> = idx.iter().map(|e| e.1).collect();
    assert!(blocks.len() > 5 && blocks.len() < 300, "{} blocks", blocks.len());
    // write to disk and read through Pack
    std::fs::write(d.path().join("p.pack"), &b.pack).unwrap();
    std::fs::write(d.path().join("p.idx"), &b.idx).unwrap();
    let p = Pack::open(&d.path().join("p.idx")).unwrap();
    assert_eq!(p.version(), 3);
    for (id, enc) in &list {
        assert_eq!(&p.read(id).unwrap().unwrap(), enc);
    }
    assert!(p.verify().is_empty(), "{:?}", p.verify());
    // a flipped byte anywhere in the body is caught by verify
    let mut bad = b.pack.clone();
    bad[200] ^= 0x01;
    std::fs::write(d.path().join("p.pack"), &bad).unwrap();
    let p = Pack::open(&d.path().join("p.idx")).unwrap();
    assert!(!p.verify().is_empty());
}

#[test]
fn v3_blocks_separate_metadata_from_content_and_keep_the_given_order() {
    use vcs_objects::*;
    let blob = Object::Blob(b"file".to_vec()).encode();
    let bid = id_of_encoded(&blob);
    let tree = Object::Tree(
        Tree::new(vec![TreeEntry {
            name: "f".into(),
            mode: EntryMode::File,
            id: bid,
        }])
        .unwrap(),
    )
    .encode();
    let tid = id_of_encoded(&tree);
    let b = build_pack_blocks(vec![(tid, tree), (bid, blob)], PackFormat::V3, 1 << 20);
    let idx = PackIndex::parse(&b.idx, std::path::Path::new("x")).unwrap();
    let (t, f) = (idx.find(&tid).unwrap(), idx.find(&bid).unwrap());
    assert!(t.0 < f.0, "the tree block comes first and is its own block");
    let tb = &b.pack[t.0 as usize..t.0 as usize + t.1 as usize];
    assert!(vcs_store::pack::block_is_metadata(tb));
    assert!(!vcs_store::pack::block_is_metadata(
        &b.pack[f.0 as usize..f.0 as usize + f.1 as usize]
    ));
}

#[test]
fn hostile_blocks_are_rejected_not_trusted() {
    assert!(decode_block(&[]).is_err());
    assert!(decode_block(&[9, 1, 2, 3]).is_err(), "unknown codec");
    // raw block claiming 1000 entries in 8 bytes
    let mut b = vec![vcs_store::pack::CODEC_BLOCK_RAW_DATA];
    b.extend_from_slice(&1000u32.to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    assert!(decode_block(&b).is_err());
    // zstd frame that claims a huge output
    let big = zstd::bulk::compress(&vec![0u8; 1 << 20], 3).unwrap();
    let mut z = vec![vcs_store::pack::CODEC_BLOCK_ZSTD_DATA];
    z.extend_from_slice(&big);
    assert!(decode_block(&z).is_err(), "decodes to a zero table with trailing bytes");
}

#[test]
fn v3_compaction_into_a_local_store_and_back_to_v2_with_the_rollback_flag() {
    let (_d, dir, ids) = many_packs(7);
    let s = LocalStore::open(&dir).unwrap();
    s.compact_packs(PackFormat::V3).unwrap();
    for id in &ids {
        s.get_blob(id).unwrap();
    }
    assert!(s.verify_packs().is_empty());
    let (_, converted) = s.repack_as(PackFormat::V2, true).unwrap();
    assert!(converted >= 1, "v3 packs are rewritten as v2");
    for id in &ids {
        s.get_blob(id).unwrap();
    }
    assert!(s.verify_packs().is_empty());
    let _ = write_pack_fmt;
}
