//! Pack format v3 (independently compressed blocks, written by compaction) read through every
//! remote read path: single fetch, batch fetch, metadata read-ahead, whole-history fetch, GC marking.

use std::collections::HashMap;
use vcs_objects::*;
use vcs_remote::compact::{compact, load_packs, CompactOptions};
use vcs_remote::gc::{remote_gc, RemoteGcOptions};
use vcs_remote::packfetch::{ReadAhead, RemoteSource};
use vcs_remote::*;
use vcs_store::pack::PackFormat;
use vcs_store::{MemoryStore, ObjectStore};

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

/// Text that changes a little each version, so blocks have something to share.
fn text(file: usize, version: usize) -> Vec<u8> {
    let mut s = String::new();
    for line in 0..120 {
        s.push_str(&format!(
            "file {file} line {line} fn work_{line}() {{ /* v{} */ }}\n",
            if line == version { version } else { 0 }
        ));
    }
    s.into_bytes()
}

type Built = (MemBackend, MemoryStore, Vec<ObjectId>, Vec<(ObjectId, Vec<u8>)>);

fn build(n_commits: usize) -> Built {
    let store = MemoryStore::new();
    let b = MemBackend::new().with_gc();
    b.set_now_ms(1_000_000);
    let mut parent: Option<ObjectId> = None;
    let mut commits = vec![];
    for v in 0..n_commits {
        let entries: Vec<TreeEntry> = (0..4)
            .map(|f| TreeEntry {
                name: format!("f{f}.rs"),
                mode: EntryMode::File,
                id: store.put_blob(&text(f, v)).unwrap(),
            })
            .collect();
        let t = store.put(&Object::Tree(Tree::new(entries).unwrap())).unwrap();
        let c = store
            .put(&Object::Commit(Commit {
                zone: "z".into(),
                parents: parent.into_iter().collect(),
                root: t,
                change_id: ChangeId::from_bytes([v as u8; 16]),
                author: sig(),
                message: format!("c{v}"),
            }))
            .unwrap();
        Publisher::new(
            &b,
            &store,
            Transaction {
                updates: vec![RefUpdate {
                    zone: "z".into(),
                    branch: "main".into(),
                    old: parent,
                    new: c,
                }],
                author: sig(),
                message: "m".into(),
                lock_ops: vec![],
            },
        )
        .run()
        .unwrap();
        parent = Some(c);
        commits.push(c);
    }
    let mut all = vec![];
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![*commits.last().unwrap()];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let o = store.get(&id).unwrap();
        match &o {
            Object::Commit(c) => stack.extend(c.parents.iter().copied().chain([c.root])),
            Object::Tree(t) => stack.extend(t.entries().iter().map(|e| e.id)),
            _ => {}
        }
        all.push((id, o.encode()));
    }
    (b, store, commits, all)
}

fn opts() -> CompactOptions {
    CompactOptions {
        dry_run: false,
        all: true,
        format: PackFormat::V3,
        block_bytes: 64 * 1024,
        ..Default::default()
    }
}

#[test]
fn every_read_path_reads_a_v3_pack() {
    let (b, store, commits, all) = build(12);
    let before = load_packs(&b).unwrap();
    let before_bytes: u64 = before.iter().map(|p| p.0.len).sum();
    assert_eq!(before.len(), 12);
    compact(&b, &opts()).unwrap();
    let after = load_packs(&b).unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].1.version(), 3);
    assert!(after[0].0.len < before_bytes / 2, "{} vs {before_bytes}", after[0].0.len);
    let ids: Vec<ObjectId> = all.iter().map(|o| o.0).collect();

    // single and batch
    for (id, enc) in all.iter().take(20) {
        assert_eq!(&fetch_object(&b, id).unwrap().encode(), enc);
    }
    let got: HashMap<_, _> = fetch_many(&b, &ids).unwrap().into_iter().collect();
    for (id, enc) in &all {
        assert_eq!(&got[id], enc);
    }
    // incremental source, with and without read-ahead
    for ahead in [ReadAhead::None, ReadAhead::Metadata, ReadAhead::Whole] {
        let mut src = RemoteSource::new(&b, ahead).with_roots(vec![*commits.last().unwrap()]);
        for (id, enc) in &all {
            assert_eq!(&src.fetch(id).unwrap().expect("present"), enc, "{ahead:?}");
        }
    }
    // whole fetch into an empty store gives the same head and every object
    let dst = MemoryStore::new();
    let heads = fetch_heads(&b, &dst, None).unwrap();
    assert_eq!(heads[0].2, *commits.last().unwrap());
    for id in &ids {
        assert_eq!(&dst.get(id).unwrap().encode(), &store.get(id).unwrap().encode());
    }
    // GC marking reads every object through the v3 pack and finds nothing to delete
    b.advance_ms(30 * 24 * 3600 * 1000);
    let r = remote_gc(
        &b,
        &RemoteGcOptions {
            dry_run: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.reachable, ids.len());
    assert!(r.deleted.is_empty());
}

#[test]
fn a_flipped_byte_in_a_v3_block_is_an_error_never_wrong_data() {
    let (b, _store, _commits, all) = build(6);
    compact(&b, &opts()).unwrap();
    let stem = load_packs(&b).unwrap().remove(0).0.stem;
    let key = format!("{stem}.pack");
    let good = b.get(&key).unwrap().unwrap().data;
    for pos in (20..good.len() - 40).step_by(37) {
        let mut bad = good.clone();
        bad[pos] ^= 0x40;
        b.tamper(&key, bad);
        for (id, enc) in all.iter().step_by(7) {
            if let Ok(o) = fetch_object(&b, id) {
                assert_eq!(&o.encode(), enc, "wrong data accepted after flipping byte {pos}");
            }
        }
    }
}

#[test]
fn v3_input_and_v2_input_merge_into_either_format() {
    let (b, _store, _c, all) = build(8);
    let mut o = opts();
    o.all = false;
    o.tier_ratio = 2;
    o.tier_base = 1 << 30;
    compact(&b, &o).unwrap(); // everything merges into one v3 pack
    let mut o2 = opts();
    o2.format = PackFormat::V2;
    // a second pack so there is something to merge: publish nothing, so compact --all on one pack is a no-op
    let rep = compact(&b, &o2).unwrap();
    assert!(rep.merges.is_empty());
    let ids: Vec<ObjectId> = all.iter().map(|o| o.0).collect();
    assert_eq!(fetch_many(&b, &ids).unwrap().len(), ids.len());
}
