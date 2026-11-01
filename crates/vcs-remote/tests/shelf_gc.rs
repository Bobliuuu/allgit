//! Shelves (docs/shelves.md) against the REAL remote GC sweeper. A shelf must never lose the
//! objects it depends on to a sweep, however the sweep and the `shelve` interleave, and only
//! while it exists: deleting or expiring a shelf releases them. `GcQuirks::sweeper_ignores_shelves`
//! switches the shelf roots off so the mutation tests can show each check is load-bearing.

use std::path::Path;
use vcs_objects::*;
use vcs_remote::gc::{remote_gc, remote_gc_with, GcPoint, RemoteGcOptions};
use vcs_remote::protocol::{object_key, GcQuirks};
use vcs_remote::shelf::{self, ShelfError, ShelfMeta, ShelfPack, ShelfZone};
use vcs_remote::*;
use vcs_store::pack::{build_pack_fmt, PackFormat, PackIndex};
use vcs_store::{MemoryStore, ObjectStore};

const SEC: u64 = 1000;
const HOUR: u64 = 3600 * SEC;
const DAY: u64 = 24 * HOUR;
const T0: u64 = 1_000_000;

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

fn commit(s: &MemoryStore, content: &[u8], parents: Vec<ObjectId>, tag: u8) -> ObjectId {
    let blob = s.put_blob(content).unwrap();
    let root = s
        .put(&Object::Tree(
            Tree::new(vec![TreeEntry {
                name: "f".into(),
                mode: EntryMode::File,
                id: blob,
            }])
            .unwrap(),
        ))
        .unwrap();
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

fn upload_loose(b: &dyn Backend, s: &MemoryStore, id: ObjectId) {
    let o = s.get(&id).unwrap();
    for d in deps_of(&o) {
        upload_loose(b, s, d);
    }
    b.put_if_absent(&object_key(&id), &o.encode()).unwrap();
}

fn closure(s: &MemoryStore, id: ObjectId, out: &mut Vec<ObjectId>) {
    if out.contains(&id) {
        return;
    }
    out.push(id);
    for d in deps_of(&s.get(&id).unwrap()) {
        closure(s, d, out);
    }
}

fn opts() -> RemoteGcOptions {
    RemoteGcOptions {
        grace_ms: DAY,
        hold_ms: HOUR,
        dry_run: false,
        lock_ttl_ms: 6 * HOUR,
        owner: "sweeper".into(),
        allow_short_grace: false,
    }
}

struct World {
    /// An old commit that only a shelf still needs (the branch moved on).
    base: ObjectId,
    base_closure: Vec<ObjectId>,
    /// The ephemeral shelf commit on top of `base` and the new objects it needs.
    shelved: ObjectId,
    shelf_objs: Vec<(ObjectId, Vec<u8>)>,
}

fn world() -> (World, MemBackend) {
    let store = MemoryStore::new();
    let head = commit(&store, b"current head", vec![], 1);
    let base = commit(&store, b"old base", vec![], 2);
    let shelved = commit(&store, b"shelved work", vec![base], 3);
    let mut base_closure = vec![];
    closure(&store, base, &mut base_closure);
    let mut all = vec![];
    closure(&store, shelved, &mut all);
    let shelf_objs: Vec<_> = all
        .into_iter()
        .filter(|i| !base_closure.contains(i))
        .map(|i| (i, store.get(&i).unwrap().encode()))
        .collect();
    let b = MemBackend::new().with_gc();
    b.set_now_ms(T0);
    // publish `head`; `base` is an old object that is no longer reachable from any branch
    Publisher::new(
        &b,
        &store,
        Transaction {
            updates: vec![RefUpdate {
                zone: "z".into(),
                branch: "main".into(),
                old: None,
                new: head,
            }],
            author: sig(),
            message: "m".into(),
            lock_ops: vec![],
        },
    )
    .run()
    .unwrap();
    upload_loose(&b, &store, base);
    b.set_now_ms(T0 + 10 * DAY);
    (
        World {
            base,
            base_closure,
            shelved,
            shelf_objs,
        },
        b,
    )
}

fn meta_and_pack(w: &World, name: &str, expires_ms: Option<u64>) -> (ShelfMeta, ShelfPack) {
    let built = build_pack_fmt(w.shelf_objs.clone(), PackFormat::V2);
    let idx = PackIndex::parse(&built.idx, Path::new("t")).unwrap();
    (
        ShelfMeta {
            principal: "alice".into(),
            name: name.into(),
            message: "wip".into(),
            author: "t <t@x>".into(),
            created_ms: T0 + 10 * DAY,
            expires_ms,
            zones: vec![ShelfZone {
                zone: "z".into(),
                branch: "main".into(),
                base: Some(w.base),
                commit: w.shelved,
            }],
            bases: vec![w.base],
            objects: idx.len() as u32,
            pack_bytes: built.pack.len() as u64,
            pack_checksum: shelf::checksum_hex(&idx),
        },
        ShelfPack {
            pack: built.pack,
            idx: built.idx,
        },
    )
}

fn base_present(w: &World, b: &MemBackend) -> bool {
    w.base_closure.iter().all(|i| b.exists(&object_key(i)).unwrap())
}

#[test]
fn a_shelf_keeps_its_base_through_a_sweep_and_releases_it_when_deleted() {
    let (w, b) = world();
    let (m, p) = meta_and_pack(&w, "w1", None);
    shelf::upload(&b, &m, &p).unwrap();
    b.set_now_ms(T0 + 30 * DAY); // the touch from the upload is now old too
    let r = remote_gc(&b, &opts()).unwrap();
    assert!(base_present(&w, &b), "the sweep deleted a shelf's base: {:?}", r.deleted);
    // the shelf downloads and verifies after the sweep
    let objs = shelf::download(&b, &shelf::load_meta(&b, "alice", "w1").unwrap()).unwrap();
    assert_eq!(objs.len(), w.shelf_objs.len());
    // only while it exists
    shelf::delete(&b, "alice", "w1").unwrap();
    remote_gc(&b, &opts()).unwrap();
    assert!(!base_present(&w, &b), "a deleted shelf must stop pinning its base");
}

#[test]
fn mutation_sweeper_ignoring_shelf_roots_loses_the_base() {
    let (w, b) = world();
    let (m, p) = meta_and_pack(&w, "w1", None);
    shelf::upload(&b, &m, &p).unwrap();
    b.set_now_ms(T0 + 30 * DAY);
    let q = GcQuirks {
        sweeper_ignores_shelves: true,
        ..Default::default()
    };
    remote_gc_with(&b, &opts(), q, &mut |_| {}).unwrap();
    assert!(
        !base_present(&w, &b),
        "with shelf roots switched off the base is swept: the shelf roots are what protect it"
    );
}

#[test]
fn expired_shelves_are_removed_by_gc_and_release_their_base() {
    let (w, b) = world();
    let (m, p) = meta_and_pack(&w, "old", Some(T0 + 10 * DAY + HOUR));
    shelf::upload(&b, &m, &p).unwrap();
    let (m2, p2) = meta_and_pack(&w, "forever", None);
    shelf::upload(&b, &m2, &p2).unwrap();
    b.set_now_ms(T0 + 30 * DAY);
    let mut dry = opts();
    dry.dry_run = true;
    let r = remote_gc(&b, &dry).unwrap();
    assert_eq!(r.expired_shelves, vec!["alice/old".to_string()]);
    assert!(b.exists(&shelf::meta_key("alice", "old")).unwrap(), "dry run deletes nothing");
    let r = remote_gc(&b, &opts()).unwrap();
    assert_eq!(r.expired_shelves, vec!["alice/old".to_string()]);
    for k in ["meta", "pack", "idx"] {
        assert!(
            !b.exists(&format!("shelves/alice/old/{k}")).unwrap(),
            "{k} of an expired shelf remains"
        );
        assert!(b.exists(&format!("shelves/alice/forever/{k}")).unwrap());
    }
    assert!(base_present(&w, &b), "the other shelf still needs the base");
    shelf::delete(&b, "alice", "forever").unwrap();
    remote_gc(&b, &opts()).unwrap();
    assert!(!base_present(&w, &b));
}

#[test]
fn a_sweep_between_candidates_and_remark_sees_a_shelf_created_meanwhile() {
    for ignore in [false, true] {
        let (w, b) = world();
        let (m, p) = meta_and_pack(&w, "racy", None);
        b.set_now_ms(T0 + 30 * DAY);
        let q = GcQuirks {
            sweeper_ignores_shelves: ignore,
            ..Default::default()
        };
        let mut done = false;
        remote_gc_with(&b, &opts(), q, &mut |pt| {
            // the base was already chosen as a candidate; now the shelf appears
            if pt == GcPoint::Candidates && !done {
                done = true;
                shelf::upload(&b, &m, &p).unwrap();
            }
        })
        .unwrap();
        assert!(done);
        // The upload touched the base, so the racing sweep must have kept it (shelf roots or not).
        assert!(
            base_present(&w, &b),
            "a shelf created during a sweep lost its base (ignore={ignore})"
        );
        // Once the touch is old, only the shelf root protects it.
        b.set_now_ms(T0 + 60 * DAY);
        remote_gc_with(&b, &opts(), q, &mut |_| {}).unwrap();
        assert_eq!(base_present(&w, &b), !ignore, "ignore={ignore}: shelf roots decide the base's fate");
    }
}

#[test]
fn a_shelve_that_meets_a_running_sweep_aborts_cleanly_and_never_leaves_a_dangling_shelf() {
    // tombstoned base: the sweeper is about to delete it
    let (w, b) = world();
    let (m, p) = meta_and_pack(&w, "t", None);
    b.set_now_ms(T0 + 30 * DAY);
    let mut result = None;
    remote_gc_with(&b, &opts(), GcQuirks::default(), &mut |pt| {
        if pt == GcPoint::Tombed && result.is_none() {
            result = Some(shelf::upload(&b, &m, &p));
        }
    })
    .unwrap();
    // The upload either failed (tombstone seen) or the sweeper kept the base because of the touch.
    match result.expect("hook ran") {
        Err(ShelfError::Gc(_)) => assert!(!b.exists(&shelf::meta_key("alice", "t")).unwrap()),
        Ok(()) => assert!(base_present(&w, &b), "shelf exists but its base is gone"),
        Err(e) => panic!("{e}"),
    }
    // an already swept base: the shelve refuses instead of writing a dangling shelf
    let (w, b) = world();
    let (m, p) = meta_and_pack(&w, "s", None);
    b.set_now_ms(T0 + 30 * DAY);
    remote_gc(&b, &opts()).unwrap();
    assert!(!base_present(&w, &b));
    let e = shelf::upload(&b, &m, &p).unwrap_err();
    assert!(matches!(e, ShelfError::Gc(_)), "{e}");
    assert!(shelf::list(&b, None).unwrap().is_empty());
    assert!(
        b.list("shelves/").unwrap().is_empty(),
        "debris of the failed shelve must be removed"
    );
}

#[test]
fn shelf_objects_are_hash_verified_on_download() {
    let (w, b) = world();
    let (m, p) = meta_and_pack(&w, "v", None);
    shelf::upload(&b, &m, &p).unwrap();
    let meta = shelf::load_meta(&b, "alice", "v").unwrap();
    assert_eq!(shelf::download(&b, &meta).unwrap().len(), w.shelf_objs.len());
    // flip a byte in the stored pack: refused
    let key = shelf::pack_key("alice", "v");
    let mut bytes = b.get(&key).unwrap().unwrap().data;
    let n = bytes.len() / 2;
    bytes[n] ^= 1;
    b.delete(&key).unwrap();
    b.put_if_absent(&key, &bytes).unwrap();
    assert!(shelf::download(&b, &meta).is_err());
    // a pack whose internal checksums are all valid but whose entry does not hash to its id
    let mut forged = w.shelf_objs.clone();
    forged[0].0 = w.shelf_objs[1].0;
    forged.truncate(1);
    let built = build_pack_fmt(forged, PackFormat::V2);
    let idx = PackIndex::parse(&built.idx, Path::new("t")).unwrap();
    let mut m2 = m.clone();
    m2.name = "forged".into();
    m2.objects = 1;
    m2.pack_bytes = built.pack.len() as u64;
    m2.pack_checksum = shelf::checksum_hex(&idx);
    shelf::upload(
        &b,
        &m2,
        &ShelfPack {
            pack: built.pack,
            idx: built.idx,
        },
    )
    .unwrap();
    let e = shelf::download(&b, &m2).unwrap_err();
    assert!(e.to_string().contains("hash verification"), "{e}");
}

#[test]
fn create_only_and_unparsable_meta() {
    let (w, b) = world();
    let (m, p) = meta_and_pack(&w, "once", None);
    shelf::upload(&b, &m, &p).unwrap();
    assert!(matches!(shelf::upload(&b, &m, &p), Err(ShelfError::Exists(_))));
    // a damaged meta makes GC refuse to run rather than guess
    b.put_if_absent("shelves/mallory/x/meta", b"garbage").unwrap();
    b.set_now_ms(T0 + 30 * DAY);
    assert!(remote_gc(&b, &opts()).is_err());
    assert!(base_present(&w, &b));
}

#[test]
fn threaded_sweep_racing_a_shelve_never_leaves_a_shelf_without_its_base() {
    let (mut ok, mut aborted) = (0, 0);
    for i in 0..300 {
        let (w, b) = world();
        let (m, p) = meta_and_pack(&w, &format!("r{i}"), None);
        let mut o = opts();
        o.dry_run = false;
        std::thread::scope(|s| {
            let sweeper = s.spawn(|| remote_gc(&b, &o));
            let res = shelf::upload(&b, &m, &p);
            // the sweep may be locked out or lose a race with itself; it must never error as damaged
            let swept = sweeper.join().unwrap();
            assert!(swept.is_ok() || matches!(swept, Err(vcs_remote::GcError::Locked(_))), "{swept:?}");
            match res {
                Ok(()) => {
                    ok += 1;
                    assert!(base_present(&w, &b), "iteration {i}: shelf stored but its base was swept");
                    assert!(shelf::download(&b, &shelf::load_meta(&b, "alice", &format!("r{i}")).unwrap()).is_ok());
                }
                Err(ShelfError::Gc(_)) => {
                    aborted += 1;
                    assert!(b.list("shelves/").unwrap().is_empty(), "iteration {i}: debris left");
                }
                Err(e) => panic!("{e}"),
            }
        });
    }
    eprintln!("threaded race: {ok} shelved, {aborted} aborted by the sweep");
    assert!(ok + aborted == 300);
}
