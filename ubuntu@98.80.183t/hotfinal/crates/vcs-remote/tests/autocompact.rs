//! Automatic compaction after publish: many tiny packs become few, every object stays
//! readable, and the pass is a no-op where it must be (few packs, no delete support).

use vcs_objects::*;
use vcs_remote::autocompact::maybe_compact;
use vcs_remote::compact::count_packs;
use vcs_remote::packfetch::{ReadAhead, RemoteSource};
use vcs_remote::*;
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

#[test]
fn many_small_packs_are_merged_and_stay_readable() {
    let (b, _store, _commits, all) = build(40);
    let before = count_packs(&b).unwrap();
    assert!(before > 24, "one pack per publish: {before}");
    let rep = maybe_compact(&b, "test").unwrap().expect("compacts above the threshold");
    let after = count_packs(&b).unwrap();
    assert_eq!(rep.packs_before, before);
    assert_eq!(rep.packs_after, after);
    assert!(after < before / 2, "{before} -> {after}");
    // every object is still there, byte for byte, through the ordinary reader
    let mut src = RemoteSource::new(&b, ReadAhead::None);
    for (id, enc) in &all {
        assert_eq!(src.fetch(id).unwrap().as_deref(), Some(&enc[..]), "{id}");
    }
}

#[test]
fn few_packs_or_no_gc_support_do_nothing() {
    let (b, ..) = build(5);
    let n = count_packs(&b).unwrap();
    assert!(maybe_compact(&b, "test").unwrap().is_none(), "below the threshold");
    assert_eq!(count_packs(&b).unwrap(), n);
    // a backend without delete or a clock is left alone even with many packs
    let (plain, ..) = build(30);
    let no_gc = MemBackend::new();
    for k in plain.list("").unwrap() {
        if let Some(v) = plain.get(&k).unwrap() {
            no_gc.put_if_absent(&k, &v.data).unwrap();
        }
    }
    assert!(count_packs(&no_gc).unwrap() > 24);
    assert!(maybe_compact(&no_gc, "test").unwrap().is_none());
}
