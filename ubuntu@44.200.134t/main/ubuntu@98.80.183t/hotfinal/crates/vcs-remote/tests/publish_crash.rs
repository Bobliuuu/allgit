//! A publisher that dies at any step before the manifest CAS leaves nothing visible: the manifest
//! is byte-for-byte what it was, every branch head still has its whole closure, and a fresh
//! publisher (same transaction) then finishes and converges. Also pins the request count and
//! sequential depth of a one-file push. The step machine is the real `Publisher`; the backend is
//! in memory (EMULATED: counts and depth, not wall time).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use vcs_objects::*;
use vcs_remote::timeline::TimelineBackend;
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore};

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

fn commit(s: &MemoryStore, parent: Option<ObjectId>, files: usize, n: u8) -> ObjectId {
    let ents: Vec<TreeEntry> = (0..files)
        .map(|i| TreeEntry {
            name: format!("f{i:03}"),
            mode: EntryMode::File,
            id: s.put_blob(&if i == 0 { vec![n] } else { vec![i as u8, 9] }).unwrap(),
        })
        .collect();
    let root = s.put(&Object::Tree(Tree::new(ents).unwrap())).unwrap();
    s.put(&Object::Commit(Commit {
        zone: "z".into(),
        parents: parent.into_iter().collect(),
        root,
        change_id: ChangeId::from_bytes([n; 16]),
        author: sig(),
        message: format!("c{n}"),
    }))
    .unwrap()
}

fn tx(old: Option<ObjectId>, new: ObjectId) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: "z".into(),
            branch: "main".into(),
            old,
            new,
        }],
        author: sig(),
        message: "m".into(),
        lock_ops: vec![],
    }
}

fn deps_of(o: &Object) -> Vec<ObjectId> {
    match o {
        Object::Commit(c) => c.parents.iter().copied().chain([c.root]).collect(),
        Object::Tree(t) => t.entries().iter().map(|e| e.id).collect(),
        _ => vec![],
    }
}

/// Every object in the closure of every manifest head is readable from the remote.
fn closure_complete(b: &dyn Backend) {
    let (m, _) = load_manifest(b).unwrap();
    let mut seen = HashSet::new();
    let mut stack: Vec<ObjectId> = m.branches.values().copied().collect();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let o = fetch_object(b, &id).unwrap_or_else(|e| panic!("head closure lost {}: {e}", id.short()));
        stack.extend(deps_of(&o));
    }
}

fn base() -> (MemBackend, MemoryStore, ObjectId, ObjectId) {
    let b = MemBackend::new().with_gc();
    let s = MemoryStore::new();
    let c1 = commit(&s, None, 30, 1);
    Publisher::new(&b, &s, tx(None, c1)).run().unwrap();
    let c2 = commit(&s, Some(c1), 30, 2);
    (b, s, c1, c2)
}

/// Crash after `k` steps, for every k up to the CAS: nothing visible changed, a retry converges.
#[test]
fn crash_before_cas_leaves_no_visible_state() {
    let (b, s, c1, c2) = base();
    let before = b.get("manifest").unwrap().unwrap();
    let mut k = 0;
    loop {
        let mut p = Publisher::new(&b, &s, tx(Some(c1), c2));
        for _ in 0..k {
            assert_eq!(p.step().unwrap(), Step::Continue);
        }
        let at_cas = p.next_is_cas();
        drop(p); // the client dies here
        let now = b.get("manifest").unwrap().unwrap();
        assert_eq!(
            (now.data, now.tag),
            (before.data.clone(), before.tag.clone()),
            "manifest changed after {k} steps"
        );
        assert_eq!(
            load_manifest(&b).unwrap().0.head("z", "main"),
            Some(c1),
            "head moved after {k} steps"
        );
        closure_complete(&b);
        // readers still get exactly the old tip
        let local = MemoryStore::new();
        let got = fetch_heads(&b, &local, None).unwrap();
        assert_eq!(got.iter().map(|g| g.2).collect::<Vec<_>>(), vec![c1]);
        if at_cas {
            break;
        }
        k += 1;
        assert!(k < 20, "never reached the CAS");
    }
    assert!(k >= 3, "pack, index and tx are separate steps before the CAS (k={k})");
    // a fresh publisher of the same transaction (leftover pack, index, tx are deduplicated) finishes
    assert!(matches!(
        Publisher::new(&b, &s, tx(Some(c1), c2)).run().unwrap(),
        Outcome::Applied { .. }
    ));
    assert_eq!(load_manifest(&b).unwrap().0.head("z", "main"), Some(c2));
    closure_complete(&b);
    let local = MemoryStore::new();
    assert_eq!(fetch_heads(&b, &local, None).unwrap()[0].2, c2);
}

/// The index is the commit point of a pack: a crash after the pack but before the index must not
/// make any pack visible to readers (no `.idx` without its `.pack`, ever).
#[test]
fn index_never_lands_before_its_pack() {
    let (b, s, c1, c2) = base();
    let mut p = Publisher::new(&b, &s, tx(Some(c1), c2));
    loop {
        let keys = b.list("packs/").unwrap();
        for k in &keys {
            if let Some(stem) = k.strip_suffix(".idx") {
                assert!(keys.contains(&format!("{stem}.pack")), "{stem}.idx is visible without its pack");
            }
        }
        if p.next_is_cas() {
            break;
        }
        p.step().unwrap();
    }
}

/// A lost CAS (another writer moved the branch) leaves the loser's pack and tx as unreferenced
/// garbage and changes nothing visible.
#[test]
fn lost_cas_changes_nothing_visible() {
    let (b, s, c1, c2) = base();
    let c3 = commit(&s, Some(c1), 30, 3);
    Publisher::new(&b, &s, tx(Some(c1), c3)).run().unwrap(); // the winner
    let after_winner = b.get("manifest").unwrap().unwrap();
    let e = Publisher::new(&b, &s, tx(Some(c1), c2)).run().unwrap_err();
    assert!(matches!(e, PublishError::Diverged { .. }), "{e}");
    let now = b.get("manifest").unwrap().unwrap();
    assert_eq!((now.data, now.tag), (after_winner.data, after_winner.tag));
    closure_complete(&b);
}

/// One-file push: no existence probes, no second manifest read, pack then (index and tx together),
/// then the CAS. Counts and sequential depth, with a hinted manifest like `Repo::publish`.
#[test]
fn one_file_push_requests_and_depth() {
    let (b, s, c1, c2) = base();
    let mem = Arc::new(b);
    let tl = TimelineBackend::new(mem.clone(), Duration::from_millis(2));
    let (m, tag) = load_manifest(mem.as_ref()).unwrap();
    Publisher::new(&tl, &s, tx(Some(c1), c2)).with_manifest_hint(m, tag).run().unwrap();
    for e in tl.events() {
        println!("{:>8} {}", e.op, e.key);
    }
    let ops: Vec<&str> = tl.events().iter().map(|e| e.op).collect();
    assert!(!ops.contains(&"HEAD"), "no existence probe for a new commit: {ops:?}");
    assert_eq!(ops.iter().filter(|o| **o == "GET").count(), 0, "manifest hint: no GET: {ops:?}");
    // pack, idx, tx, cas (+ at most the backend clock reads, which the in-memory GC clock does not count as requests)
    assert!(tl.requests() <= 4, "requests {}: {ops:?}", tl.requests());
    assert!(tl.depth() <= 3, "depth {}: {ops:?}", tl.depth());
}
