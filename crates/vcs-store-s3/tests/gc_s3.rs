//! #232: remote garbage collection against a real S3-compatible server (gated on
//! `VCS_TEST_S3_ENDPOINT`): `conformance_gc` (touch / age / clock), then the real sweeper and the real
//! publisher on S3. Ages are the server's `LastModified` (1 s resolution), so the scenarios sleep a few
//! seconds and use `allow_short_grace` (tests only).

use std::collections::HashSet;
use vcs_objects::*;
use vcs_remote::backend::conformance_gc;
use vcs_remote::gc::{remote_gc, RemoteGcOptions};
use vcs_remote::protocol::object_key;
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore};
use vcs_store_s3::{S3Backend, S3Config};

fn s3(test: &str) -> Option<S3Backend> {
    let Ok(ep) = std::env::var("VCS_TEST_S3_ENDPOINT") else {
        eprintln!("skipping {test}: VCS_TEST_S3_ENDPOINT not set");
        return None;
    };
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
    let mut c = S3Config::new(
        &ep,
        &var("VCS_TEST_S3_BUCKET", "vcs-test"),
        &var("VCS_TEST_S3_ACCESS_KEY", "minioadmin"),
        &var("VCS_TEST_S3_SECRET_KEY", "minioadmin"),
    );
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    c.prefix = format!("{test}-{t}/");
    let b = S3Backend::new_unchecked(c.clone()).unwrap();
    b.ensure_bucket().unwrap();
    Some(S3Backend::connect(c).unwrap())
}

fn sig() -> Signature {
    Signature { name: "t".into(), email: "t@x".into(), timestamp: 1 }
}
fn tree_of(s: &MemoryStore, content: &[u8]) -> ObjectId {
    let blob = s.put_blob(content).unwrap();
    s.put(&Object::Tree(Tree::new(vec![TreeEntry { name: "f".into(), mode: EntryMode::File, id: blob }]).unwrap())).unwrap()
}
fn commit(s: &MemoryStore, root: ObjectId, parents: Vec<ObjectId>, tag: u8) -> ObjectId {
    s.put(&Object::Commit(Commit {
        zone: "z".into(),
        parents,
        root,
        change_id: ChangeId::from_bytes([tag; 16]),
        author: sig(),
        message: format!("c{tag}"),
    }))
    .unwrap()
}
fn deps_of(o: &Object) -> Vec<ObjectId> {
    match o {
        Object::Commit(c) => c.parents.iter().copied().chain([c.root]).collect(),
        Object::Tree(t) => t.entries().iter().map(|e| e.id).collect(),
        _ => vec![],
    }
}
fn upload(b: &dyn Backend, s: &MemoryStore, id: ObjectId, seen: &mut HashSet<ObjectId>) {
    if !seen.insert(id) {
        return;
    }
    let o = s.get(&id).unwrap();
    for d in deps_of(&o) {
        upload(b, s, d, seen);
    }
    b.put_if_absent(&object_key(&id), &o.encode()).unwrap();
}
fn tx(old: Option<ObjectId>, new: ObjectId) -> Transaction {
    Transaction {
        updates: vec![RefUpdate { zone: "z".into(), branch: "main".into(), old, new }],
        author: sig(),
        message: "m".into(),
        lock_ops: vec![],
    }
}
fn opts() -> RemoteGcOptions {
    RemoteGcOptions {
        grace_ms: 3_000,
        hold_ms: 1_000,
        dry_run: false,
        lock_ttl_ms: 60_000,
        owner: "test".into(),
        allow_short_grace: true,
    }
}

#[test]
fn s3_conformance_gc() {
    let Some(b) = s3("conformance-gc") else { return };
    assert!(b.supports_gc());
    // the first reading creates the clock object; later ones are anchored (no request)
    let n0 = b.requests();
    let t1 = b.now_ms().unwrap();
    let n1 = b.requests();
    let t2 = b.now_ms().unwrap();
    assert_eq!(b.requests(), n1, "an anchored clock reading must not hit the network ({n0} -> {n1})");
    assert!(t2 >= t1);
    // the anchored clock agrees with this machine to within a minute (both are NTP clocks)
    let local = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    assert!(t1.abs_diff(local) < 60_000, "server {t1} local {local}");
    conformance_gc(&b);
}

#[test]
fn s3_sweeper_deletes_old_garbage_keeps_the_head_and_touch_protects() {
    let Some(b) = s3("sweep") else { return };
    let store = MemoryStore::new();
    let head = commit(&store, tree_of(&store, b"head content"), vec![], 2);
    let orphan = commit(&store, tree_of(&store, b"orphan content"), vec![], 1);
    Publisher::new(&b, &store, tx(None, head)).run().unwrap();
    upload(&b, &store, orphan, &mut HashSet::new());
    // too young: kept
    let rep = remote_gc(&b, &opts()).unwrap();
    assert!(rep.deleted.is_empty(), "{rep:?}");
    std::thread::sleep(std::time::Duration::from_millis(4_200));
    // a touch (what a publisher adopting the object by dedup does) makes it young again
    assert!(b.touch(&object_key(&orphan)).unwrap());
    let rep = remote_gc(&b, &opts()).unwrap();
    assert!(!rep.deleted.contains(&orphan), "a touched object must survive: {rep:?}");
    assert!(b.exists(&object_key(&orphan)).unwrap());
    // untouched for longer than the grace: gone, with its tree and blob; the head and manifest are not
    std::thread::sleep(std::time::Duration::from_millis(4_200));
    let rep = remote_gc(&b, &opts()).unwrap();
    assert!(rep.deleted.contains(&orphan), "{rep:?}");
    assert_eq!(rep.deleted.len(), 3, "commit, tree, blob: {rep:?}");
    assert!(!b.exists(&object_key(&orphan)).unwrap());
    assert!(fetch_object(&b, &head).is_ok(), "the head must survive");
    assert!(b.exists("manifest").unwrap());
    assert!(b.list("tomb/").unwrap().is_empty(), "no tombstones left behind");
    assert!(b.list("gc/").unwrap().iter().all(|k| k == "gc/clock"), "lock released: {:?}", b.list("gc/"));
    // idempotent
    assert!(remote_gc(&b, &opts()).unwrap().deleted.is_empty());
}
