//! Remote packs: publish bundles a transaction into one pack, fetch reads indexes plus ranges,
//! legacy loose remotes keep working, and request/byte counts are measured with
//! `CountingBackend`. Run with `--nocapture` to see the numbers quoted in docs/benchmarks.md.

use vcs_objects::*;
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore};

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

fn file(name: String, id: ObjectId) -> TreeEntry {
    TreeEntry {
        name,
        mode: EntryMode::File,
        id,
    }
}

fn commit(s: &MemoryStore, zone: &str, parents: Vec<ObjectId>, root: ObjectId, n: u32) -> ObjectId {
    s.put(&Object::Commit(Commit {
        zone: zone.into(),
        parents,
        root,
        change_id: ChangeId::from_bytes([n as u8; 16]),
        author: sig(),
        message: format!("c{n}"),
    }))
    .unwrap()
}

fn tx(zone: &str, old: Option<ObjectId>, new: ObjectId) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: zone.into(),
            branch: "main".into(),
            old,
            new,
        }],
        author: sig(),
        message: "m".into(),
        lock_ops: vec![],
    }
}

fn publish(b: &dyn Backend, s: &MemoryStore, zone: &str, old: Option<ObjectId>, new: ObjectId, packed: bool) {
    let p = Publisher::new(b, s, tx(zone, old, new));
    let p = if packed { p } else { p.with_loose_objects() };
    p.run().unwrap();
}

/// A flat directory of `n` files, then one commit that edits file 0. Returns (c1, c2).
fn big_dir(s: &MemoryStore, n: usize) -> (ObjectId, ObjectId) {
    let mut es: Vec<TreeEntry> = (0..n)
        .map(|i| file(format!("f{i:05}"), s.put_blob(format!("content {i}").as_bytes()).unwrap()))
        .collect();
    let c1 = commit(s, "z", vec![], s.put(&Object::Tree(Tree::new(es.clone()).unwrap())).unwrap(), 1);
    es[0] = file("f00000".into(), s.put_blob(b"edited").unwrap());
    let c2 = commit(s, "z", vec![c1], s.put(&Object::Tree(Tree::new(es).unwrap())).unwrap(), 2);
    (c1, c2)
}

/// `n` commits over `files` files, each commit rewriting one file. Returns the head.
fn history(s: &MemoryStore, n: u32, files: usize) -> ObjectId {
    let mut es: Vec<TreeEntry> = (0..files)
        .map(|i| file(format!("f{i:03}"), s.put_blob(format!("v0 {i}").as_bytes()).unwrap()))
        .collect();
    let mut parent = vec![];
    let mut head = None;
    for c in 0..n {
        let i = c as usize % files;
        es[i] = file(format!("f{i:03}"), s.put_blob(format!("v{c} {i}").as_bytes()).unwrap());
        let root = s.put(&Object::Tree(Tree::new(es.clone()).unwrap())).unwrap();
        let id = commit(s, "z", parent.clone(), root, c);
        parent = vec![id];
        head = Some(id);
    }
    head.unwrap()
}

/// Every object reachable from `id` is in `s` (full closure).
fn assert_closure(s: &MemoryStore, id: ObjectId) {
    let mut stack = vec![id];
    let mut seen = std::collections::HashSet::new();
    while let Some(i) = stack.pop() {
        if !seen.insert(i) {
            continue;
        }
        match s.get(&i).unwrap_or_else(|_| panic!("object {i} missing locally")) {
            Object::Commit(c) => {
                stack.extend(c.parents);
                stack.push(c.root);
            }
            Object::Tree(t) => stack.extend(t.entries().iter().map(|e| e.id)),
            _ => {}
        }
    }
}

#[test]
fn one_file_publish_in_a_big_directory_is_constant_cost() {
    let mut rows = vec![];
    for packed in [false, true] {
        let src = MemoryStore::new();
        let (c1, c2) = big_dir(&src, 5000);
        let mem = MemBackend::new();
        publish(&mem, &src, "z", None, c1, packed);
        let cb = CountingBackend::new(&mem);
        publish(&cb, &src, "z", Some(c1), c2, packed);
        let st = cb.stats();
        println!("1-file publish into a 5000-entry directory, packed={packed}: {st}");
        rows.push(st);
        let dst = MemoryStore::new();
        fetch_heads(&mem, &dst, None).unwrap();
        assert_closure(&dst, c2);
    }
    assert!(rows[0].requests > 5000, "loose baseline should show the HEAD walk: {}", rows[0]);
    assert!(
        rows[1].requests <= 8,
        "packed publish must not scale with the directory: {}",
        rows[1]
    );
}

#[test]
fn clone_of_1000_commits_reads_indexes_and_ranges() {
    let mut rows = vec![];
    for packed in [false, true] {
        let src = MemoryStore::new();
        let head = history(&src, 1000, 100);
        let mem = MemBackend::new();
        publish(&mem, &src, "z", None, head, packed);
        let cb = CountingBackend::new(&mem);
        let dst = MemoryStore::new();
        fetch_heads_lazy(&cb, &dst, None).unwrap();
        let st = cb.stats();
        println!("lazy clone of 1000 commits x 100 files, packed={packed}: {st}");
        rows.push(st);
        // lazy: commits and trees only
        let Object::Commit(c) = dst.get(&head).unwrap() else { panic!() };
        assert!(dst.contains(&c.root).unwrap());
        // a full fetch into a second store also completes
        let full = MemoryStore::new();
        let cf = CountingBackend::new(&mem);
        fetch_heads(&cf, &full, None).unwrap();
        println!("full fetch of 1000 commits x 100 files, packed={packed}: {}", cf.stats());
        assert_closure(&full, head);
    }
    assert!(rows[0].requests > 2000, "loose baseline: {}", rows[0]);
    assert!(rows[1].requests <= 12, "packed clone: {}", rows[1]);
}

#[test]
fn mixed_loose_and_packed_remote() {
    let src = MemoryStore::new();
    let h1 = history(&src, 5, 10);
    let mem = MemBackend::new();
    publish(&mem, &src, "z", None, h1, false); // legacy loose
                                               // extend: packed on top of loose, then loose on top of packed
    let step = |parent: ObjectId, n: u32| {
        let blob = src.put_blob(format!("n{n}").as_bytes()).unwrap();
        let root = src.put(&Object::Tree(Tree::new(vec![file("g".into(), blob)]).unwrap())).unwrap();
        commit(&src, "z", vec![parent], root, 100 + n)
    };
    let h2 = step(h1, 2);
    publish(&mem, &src, "z", Some(h1), h2, true);
    let h3 = step(h2, 3);
    publish(&mem, &src, "z", Some(h2), h3, false);
    let h4 = step(h3, 4);
    publish(&mem, &src, "z", Some(h3), h4, true);
    assert!(mem.keys().iter().any(|k| k.starts_with("packs/")));
    assert!(mem.keys().iter().any(|k| k.starts_with("objects/")));
    for lazy in [false, true] {
        let dst = MemoryStore::new();
        if lazy {
            fetch_heads_lazy(&mem, &dst, None).unwrap();
        } else {
            fetch_heads(&mem, &dst, None).unwrap();
            assert_closure(&dst, h4);
        }
        assert!(dst.contains(&h4).unwrap() && dst.contains(&h1).unwrap());
    }
    // single-object reads work against both layouts
    for id in [h1, h2, h3, h4] {
        fetch_object(&mem, &id).unwrap();
    }
}

#[test]
fn new_branch_off_an_old_commit_does_not_reupload_history() {
    let src = MemoryStore::new();
    let head = history(&src, 50, 20);
    let mem = MemBackend::new();
    publish(&mem, &src, "z", None, head, true);
    // branch from the commit 10 below the head: not a manifest head, but an ancestor of one
    let Object::Commit(mut c) = src.get(&head).unwrap() else { panic!() };
    for _ in 0..10 {
        let Object::Commit(p) = src.get(&c.parents[0]).unwrap() else {
            panic!()
        };
        c = p;
    }
    let base = Object::Commit(c.clone()).id();
    let blob = src.put_blob(b"side").unwrap();
    let root = src.put(&Object::Tree(Tree::new(vec![file("s".into(), blob)]).unwrap())).unwrap();
    let side = commit(&src, "z", vec![base], root, 999);
    let before = mem.keys().len();
    let mut t = tx("z", None, side);
    t.updates[0].branch = "side".into();
    let cb = CountingBackend::new(&mem);
    Publisher::new(&cb, &src, t).run().unwrap();
    let st = cb.stats();
    println!("new branch off an ancestor commit: {st}");
    assert!(st.bytes_up < 20_000, "history must not be re-sent: {st}");
    assert!(mem.keys().len() > before);
    let dst = MemoryStore::new();
    fetch_heads(&mem, &dst, None).unwrap();
    assert_closure(&dst, side);
}

#[test]
fn moved_and_renamed_content_still_closes() {
    // renaming a directory moves a whole subtree: nothing in the old parent's same-named
    // entry matches, so it is re-sent (wasteful but sound); the remote must stay closed.
    let src = MemoryStore::new();
    let blob = src.put_blob(b"shared").unwrap();
    let sub = src.put(&Object::Tree(Tree::new(vec![file("a".into(), blob)]).unwrap())).unwrap();
    let dir = |name: &str, id| TreeEntry {
        name: name.into(),
        mode: EntryMode::Directory,
        id,
    };
    let r1 = src.put(&Object::Tree(Tree::new(vec![dir("old", sub)]).unwrap())).unwrap();
    let r2 = src.put(&Object::Tree(Tree::new(vec![dir("new", sub)]).unwrap())).unwrap();
    let c1 = commit(&src, "z", vec![], r1, 1);
    let c2 = commit(&src, "z", vec![c1], r2, 2);
    let mem = MemBackend::new();
    publish(&mem, &src, "z", None, c1, true);
    publish(&mem, &src, "z", Some(c1), c2, true);
    let dst = MemoryStore::new();
    fetch_heads(&mem, &dst, None).unwrap();
    assert_closure(&dst, c2);
}

#[test]
fn crash_between_pack_and_index_leaves_an_ignorable_orphan() {
    let src = MemoryStore::new();
    let h = history(&src, 5, 10);
    let mem = MemBackend::new();
    let mut p = Publisher::new(&mem, &src, tx("z", None, h));
    // step until the pack body is written, then "crash" before the index
    loop {
        p.step().unwrap();
        if mem.keys().iter().any(|k| k.ends_with(".pack")) {
            break;
        }
    }
    drop(p);
    assert!(!mem.keys().iter().any(|k| k.ends_with(".idx")));
    let dst = MemoryStore::new();
    assert!(fetch_heads(&mem, &dst, None).unwrap().is_empty(), "nothing is visible yet");
    // a full re-run converges and the reader sees everything
    publish(&mem, &src, "z", None, h, true);
    fetch_heads(&mem, &dst, None).unwrap();
    assert_closure(&dst, h);
    assert_eq!(
        mem.keys().iter().filter(|k| k.ends_with(".pack")).count(),
        1,
        "same pack, same name"
    );
}

#[test]
fn corrupt_pack_range_is_rejected() {
    let src = MemoryStore::new();
    let h = history(&src, 5, 10);
    let mem = MemBackend::new();
    publish(&mem, &src, "z", None, h, true);
    let key = mem.keys().into_iter().find(|k| k.ends_with(".pack")).unwrap();
    let mut bytes = mem.get(&key).unwrap().unwrap().data;
    for b in bytes.iter_mut().skip(12) {
        *b ^= 0xFF;
    }
    mem.tamper(&key, bytes);
    for lazy in [false, true] {
        let dst = MemoryStore::new();
        let r = if lazy {
            fetch_heads_lazy(&mem, &dst, None)
        } else {
            fetch_heads(&mem, &dst, None)
        };
        assert!(r.is_err(), "lazy={lazy}: tampered pack must not yield objects");
        assert!(!dst.contains(&h).unwrap());
    }
}

#[test]
fn one_file_publish_reads_the_manifest_once() {
    let src = MemoryStore::new();
    let (c1, c2) = big_dir(&src, 50);
    let mem = MemBackend::new();
    publish(&mem, &src, "z", None, c1, true);
    let lb = LoggingBackend::new(&mem);
    publish(&lb, &src, "z", Some(c1), c2, true);
    lb.dump("1-file packed publish");
    assert_eq!(lb.count("GET manifest"), 1, "one manifest read per publish");
    assert!(lb.calls().len() <= 6, "{:?}", lb.calls());
}

#[test]
fn stale_manifest_hint_cannot_commit_a_stale_validation() {
    let src = MemoryStore::new();
    let (c1, c2) = big_dir(&src, 10);
    let mem = MemBackend::new();
    publish(&mem, &src, "z", None, c1, true);
    let (hint, tag) = load_manifest(&mem).unwrap();
    // another writer advances z/main AFTER the hint was read
    let other = commit(&src, "z", vec![c1], c1_root(&src, c1), 77);
    publish(&mem, &src, "z", Some(c1), other, true);
    // our publish still believes z/main = c1: it must be refused (Diverged), never applied
    let p = Publisher::new(&mem, &src, tx("z", Some(c1), c2)).with_manifest_hint(hint.clone(), tag.clone());
    match p.run() {
        Err(PublishError::Diverged { found, .. }) => assert_eq!(found, Some(other)),
        r => panic!("a stale hint must end in Diverged, got {r:?}"),
    }
    assert_eq!(load_manifest(&mem).unwrap().0.head("z", "main"), Some(other));
    // a disjoint zone's writer between hint and CAS: the CAS fails on the stale tag, the retry
    // re-reads the manifest, and BOTH updates are present afterwards
    let (hint, tag) = load_manifest(&mem).unwrap();
    let zroot = c1_root(&src, c1);
    let y1 = commit(&src, "y", vec![], zroot, 90);
    publish(&mem, &src, "y", None, y1, true);
    let x1 = commit(&src, "x", vec![], zroot, 91);
    Publisher::new(&mem, &src, tx("x", None, x1))
        .with_manifest_hint(hint, tag)
        .run()
        .unwrap();
    let m = load_manifest(&mem).unwrap().0;
    assert_eq!((m.head("y", "main"), m.head("x", "main")), (Some(y1), Some(x1)));
    assert_eq!(m.head("z", "main"), Some(other));
}

fn c1_root(s: &MemoryStore, c: ObjectId) -> ObjectId {
    let Object::Commit(c) = s.get(&c).unwrap() else { panic!() };
    c.root
}

#[test]
fn hint_is_used_once_and_a_lost_race_rereads() {
    let src = MemoryStore::new();
    let (c1, c2) = big_dir(&src, 10);
    let mem = MemBackend::new();
    publish(&mem, &src, "z", None, c1, true);
    let (hint, tag) = load_manifest(&mem).unwrap();
    let lb = LoggingBackend::new(&mem);
    Publisher::new(&lb, &src, tx("z", Some(c1), c2))
        .with_manifest_hint(hint, tag)
        .run()
        .unwrap();
    assert_eq!(lb.count("GET manifest"), 0, "a fresh hint means no manifest GET at all");
    assert_eq!(lb.count("PUT-CAS manifest"), 1);
}

#[test]
fn conditional_get_costs_one_request_and_no_body_when_unchanged() {
    let src = MemoryStore::new();
    let (c1, c2) = big_dir(&src, 10);
    let mem = MemBackend::new();
    publish(&mem, &src, "z", None, c1, true);
    let cb = CountingBackend::new(&mem);
    let (m, tag) = refresh_manifest(&cb, None).unwrap();
    let full = cb.stats();
    assert!(full.bytes_down > 0);
    let before = cb.stats();
    let (m2, tag2) = refresh_manifest(&cb, Some((m.clone(), tag.clone().unwrap()))).unwrap();
    let st = cb.stats();
    println!(
        "manifest refresh, unchanged: {} request(s), {} body bytes (full read: {})",
        st.requests - before.requests,
        st.bytes_down - before.bytes_down,
        full.bytes_down
    );
    assert_eq!((st.requests - before.requests, st.bytes_down - before.bytes_down), (1, 0));
    assert_eq!((m2.head("z", "main"), tag2), (Some(c1), tag));
    // changed: full body, new head
    publish(&mem, &src, "z", Some(c1), c2, true);
    let (m3, _) = refresh_manifest(&mem, Some((m, "stale".into()))).unwrap();
    assert_eq!(m3.head("z", "main"), Some(c2));
}
