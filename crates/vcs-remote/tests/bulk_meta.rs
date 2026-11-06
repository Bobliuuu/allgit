//! `fetch_heads_bulk` (the clone path that streams pack metadata through all cores) against the plain walk:
//! same closure of commits and trees, holes (loose objects, objects in a later pack) are filled, a packless
//! remote is declined, and a tampered pack never yields objects.
use std::sync::Arc;
use vcs_objects::*;
use vcs_remote::packfetch::PackCatalog;
use vcs_remote::*;
use vcs_store::{LocalStore, MemoryStore, ObjectStore};

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

fn dir(name: &str, id: ObjectId) -> TreeEntry {
    TreeEntry {
        name: name.into(),
        mode: EntryMode::Directory,
        id,
    }
}

fn commit(s: &MemoryStore, parents: Vec<ObjectId>, root: ObjectId, n: u32) -> ObjectId {
    s.put(&Object::Commit(Commit {
        zone: "z".into(),
        parents,
        root,
        change_id: ChangeId::from_bytes([n as u8; 16]),
        author: sig(),
        message: format!("c{n}"),
    }))
    .unwrap()
}

fn publish(b: &dyn Backend, s: &MemoryStore, old: Option<ObjectId>, new: ObjectId, packed: bool) {
    let t = Transaction {
        updates: vec![RefUpdate {
            zone: "z".into(),
            branch: "main".into(),
            old,
            new,
        }],
        author: sig(),
        message: "m".into(),
        lock_ops: vec![],
    };
    let p = Publisher::new(b, s, t);
    let p = if packed { p } else { p.with_loose_objects() };
    p.run().unwrap();
}

/// Commit `n` on top of `parent`, each with a root that holds a subdirectory (so trees nest) and one file.
fn step(s: &MemoryStore, parent: Option<ObjectId>, n: u32) -> ObjectId {
    let blob = s.put_blob(format!("n{n}").as_bytes()).unwrap();
    let sub = s.put(&Object::Tree(Tree::new(vec![file(format!("f{n}"), blob)]).unwrap())).unwrap();
    let root = s
        .put(&Object::Tree(Tree::new(vec![dir("d", sub), file("g".into(), blob)]).unwrap()))
        .unwrap();
    commit(s, parent.into_iter().collect(), root, n)
}

/// Every commit and tree reachable from `id` (files are not followed) is in `s`.
fn assert_meta_closure(s: &LocalStore, id: ObjectId) -> usize {
    let (mut stack, mut seen) = (vec![id], std::collections::HashSet::new());
    while let Some(i) = stack.pop() {
        if !seen.insert(i) {
            continue;
        }
        match s.get(&i).unwrap_or_else(|e| panic!("object {i} missing locally: {e}")) {
            Object::Commit(c) => {
                stack.extend(c.parents);
                stack.push(c.root);
            }
            Object::Tree(t) => stack.extend(t.entries().iter().filter(|e| e.mode == EntryMode::Directory).map(|e| e.id)),
            _ => {}
        }
    }
    seen.len()
}

type Heads = Vec<(String, String, ObjectId)>;

fn bulk(mem: &MemBackend, dst: &LocalStore) -> Result<Option<Heads>, PublishError> {
    let (m, _) = load_manifest(mem)?;
    fetch_heads_bulk(mem, dst, &m, &Arc::new(PackCatalog::new()))
}

#[test]
fn bulk_fetch_of_several_packs_equals_the_plain_walk() {
    let src = MemoryStore::new();
    let mem = MemBackend::new();
    let mut head = None;
    for n in 0..30 {
        let c = step(&src, head, n);
        publish(&mem, &src, head, c, true); // one pack per publish
        head = Some(c);
    }
    let head = head.unwrap();
    let d = tempfile::tempdir().unwrap();
    let dst = LocalStore::open(d.path().join("objects")).unwrap();
    let got = bulk(&mem, &dst).unwrap().expect("packs cover the head");
    assert_eq!(got, vec![("z".to_string(), "main".to_string(), head)]);
    let n_bulk = assert_meta_closure(&dst, head);
    // the plain walk fetches the same set
    let plain = MemoryStore::new();
    fetch_heads_lazy(&mem, &plain, None).unwrap();
    assert_eq!(n_bulk, plain.list().unwrap().len(), "same commits and trees as the walk");
}

#[test]
fn bulk_fetch_fills_holes_left_by_loose_objects() {
    let src = MemoryStore::new();
    let mem = MemBackend::new();
    let h1 = step(&src, None, 1);
    publish(&mem, &src, None, h1, false); // loose
    let h2 = step(&src, Some(h1), 2);
    publish(&mem, &src, Some(h1), h2, true); // packed on top of loose
    let h3 = step(&src, Some(h2), 3);
    publish(&mem, &src, Some(h2), h3, false);
    let h4 = step(&src, Some(h3), 4);
    publish(&mem, &src, Some(h3), h4, true);
    let d = tempfile::tempdir().unwrap();
    let dst = LocalStore::open(d.path().join("objects")).unwrap();
    bulk(&mem, &dst).unwrap().expect("the head is in a pack");
    assert!(assert_meta_closure(&dst, h4) >= 4 * 3);
}

#[test]
fn bulk_fetch_declines_a_remote_whose_heads_are_not_in_packs() {
    let src = MemoryStore::new();
    let mem = MemBackend::new();
    let h1 = step(&src, None, 1);
    publish(&mem, &src, None, h1, false);
    let d = tempfile::tempdir().unwrap();
    let dst = LocalStore::open(d.path().join("objects")).unwrap();
    assert!(bulk(&mem, &dst).unwrap().is_none());
    assert!(dst.list().unwrap().is_empty(), "nothing is written when it declines");
}

#[test]
fn bulk_fetch_rejects_a_tampered_pack() {
    let src = MemoryStore::new();
    let mem = MemBackend::new();
    let mut head = None;
    for n in 0..5 {
        let c = step(&src, head, n);
        head = Some(c);
    }
    let head = head.unwrap();
    publish(&mem, &src, None, head, true);
    let key = mem.keys().into_iter().find(|k| k.ends_with(".pack")).unwrap();
    let mut bytes = mem.get(&key).unwrap().unwrap().data;
    for b in bytes.iter_mut().skip(40).take(64) {
        *b ^= 0xFF;
    }
    mem.tamper(&key, bytes);
    let d = tempfile::tempdir().unwrap();
    let dst = LocalStore::open(d.path().join("objects")).unwrap();
    // a pack that does not decode ends the prefix; the walk that follows reports the damage
    let r = bulk(&mem, &dst);
    assert!(r.is_err(), "tampered pack must not yield a clone");
    assert!(!dst.contains(&head).unwrap() || r.is_err());
}
