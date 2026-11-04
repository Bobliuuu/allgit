//! Lock validation through the real publish protocol (deterministic scenarios).
#![allow(clippy::type_complexity)]
use std::cell::Cell;
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

/// Commit in `zone` whose tree has the given (path, content) files.
fn commit(s: &MemoryStore, zone: &str, parent: Option<ObjectId>, files: &[(&str, &str)]) -> ObjectId {
    fn build(s: &MemoryStore, files: &[(&str, &str)]) -> ObjectId {
        let mut entries = vec![];
        let mut dirs: std::collections::BTreeMap<&str, Vec<(&str, &str)>> = Default::default();
        for (p, c) in files {
            match p.split_once('/') {
                None => entries.push(TreeEntry {
                    name: (*p).into(),
                    mode: EntryMode::File,
                    id: s.put_blob(c.as_bytes()).unwrap(),
                }),
                Some((d, rest)) => dirs.entry(d).or_default().push((rest, c)),
            }
        }
        for (d, fs) in dirs {
            entries.push(TreeEntry {
                name: d.into(),
                mode: EntryMode::Directory,
                id: build(s, &fs),
            });
        }
        s.put(&Object::Tree(Tree::new(entries).unwrap())).unwrap()
    }
    let root = build(s, files);
    s.put(&Object::Commit(Commit {
        zone: zone.into(),
        parents: parent.into_iter().collect(),
        root,
        change_id: ChangeId::from_bytes([files.len() as u8; 16]),
        author: sig(),
        message: format!("{files:?}"),
    }))
    .unwrap()
}

fn tx(updates: Vec<(&str, Option<ObjectId>, ObjectId)>, lock_ops: Vec<LockOp>, msg: &str) -> Transaction {
    let mut u: Vec<RefUpdate> = updates
        .into_iter()
        .map(|(z, old, new)| RefUpdate {
            zone: z.into(),
            branch: "main".into(),
            old,
            new,
        })
        .collect();
    u.sort();
    Transaction {
        updates: u,
        author: sig(),
        message: msg.into(),
        lock_ops,
    }
}

fn acquire(path: &str, lease_ms: u64) -> LockOp {
    LockOp::Acquire {
        zone: "z".into(),
        path: path.into(),
        lease_ms,
        reason: String::new(),
    }
}
fn present(path: &str, token: u64) -> LockOp {
    LockOp::Present {
        zone: "z".into(),
        path: path.into(),
        token,
    }
}

struct Env {
    b: MemBackend,
    now: Cell<u64>,
}
impl Env {
    fn run(&self, s: &MemoryStore, who: &str, t: Transaction) -> Result<Outcome, PublishError> {
        Publisher::new(&self.b, s, t)
            .with_principal(who)
            .with_clock(|| self.now.get())
            .run()
    }
    fn manifest(&self) -> Manifest {
        load_manifest(&self.b).unwrap().0
    }
}
fn env() -> Env {
    Env {
        b: MemBackend::new(),
        now: Cell::new(1000),
    }
}

/// Base commit with two files, published without locks. Returns (store, base).
fn base(e: &Env) -> (MemoryStore, ObjectId) {
    let s = MemoryStore::new();
    let c = commit(&s, "z", None, &[("a.bin", "1"), ("b.txt", "1"), ("d/c.bin", "1")]);
    e.run(&s, "alice", tx(vec![("z", None, c)], vec![], "base")).unwrap();
    (s, c)
}

#[test]
fn lock_free_repos_keep_v1_manifest_bytes() {
    let e = env();
    base(&e);
    let raw = e.b.get("manifest").unwrap().unwrap().data;
    assert_eq!(raw[4], 1);
    assert!(!e.manifest().has_lock_state());
}

#[test]
fn second_user_publish_of_locked_path_is_refused_and_leaves_no_trace() {
    let e = env();
    let (s, c) = base(&e);
    e.run(&s, "alice", tx(vec![], vec![acquire("a.bin", 500)], "lock")).unwrap();
    let m = e.manifest();
    assert_eq!(m.locks[&("z".into(), "a.bin".into())].holder, "alice");
    assert_eq!(m.next_token, 1);

    let s2 = MemoryStore::new();
    let c1 = commit(&s2, "z", None, &[("a.bin", "1"), ("b.txt", "1"), ("d/c.bin", "1")]);
    assert_eq!(c1, c);
    let touches = commit(&s2, "z", Some(c1), &[("a.bin", "BOB"), ("b.txt", "1"), ("d/c.bin", "1")]);
    let gen = e.manifest().generation;
    let err = e.run(&s2, "bob", tx(vec![("z", Some(c1), touches)], vec![], "bob")).unwrap_err();
    assert!(
        matches!(err, PublishError::LockedBy { ref holder, ref path, .. } if holder == "alice" && path == "a.bin"),
        "{err}"
    );
    assert_eq!(e.manifest().generation, gen, "rejected tx writes nothing");

    // untouched paths are fine
    let other = commit(&s2, "z", Some(c1), &[("a.bin", "1"), ("b.txt", "BOB"), ("d/c.bin", "1")]);
    e.run(&s2, "bob", tx(vec![("z", Some(c1), other)], vec![], "ok")).unwrap();
}

#[test]
fn holder_publishes_with_current_token_only() {
    let e = env();
    let (s, c) = base(&e);
    e.run(&s, "alice", tx(vec![], vec![acquire("a.bin", 500)], "lock")).unwrap();
    let n = commit(&s, "z", Some(c), &[("a.bin", "2"), ("b.txt", "1"), ("d/c.bin", "1")]);
    let no_token = e.run(&s, "alice", tx(vec![("z", Some(c), n)], vec![], "x")).unwrap_err();
    assert!(matches!(no_token, PublishError::LockBroken { .. }), "{no_token}");
    let stale = e
        .run(&s, "alice", tx(vec![("z", Some(c), n)], vec![present("a.bin", 7)], "y"))
        .unwrap_err();
    assert!(matches!(stale, PublishError::LockBroken { .. }), "{stale}");
    e.run(&s, "alice", tx(vec![("z", Some(c), n)], vec![present("a.bin", 0)], "z"))
        .unwrap();
}

#[test]
fn expired_then_retaken_lock_fences_the_old_holder() {
    let e = env();
    let (s, c) = base(&e);
    e.run(&s, "alice", tx(vec![], vec![acquire("a.bin", 100)], "alice")).unwrap(); // token 0
    e.now.set(1200);
    e.run(&s, "bob", tx(vec![], vec![acquire("a.bin", 100)], "bob")).unwrap(); // token 1
    let n = commit(&s, "z", Some(c), &[("a.bin", "2"), ("b.txt", "1"), ("d/c.bin", "1")]);
    let err = e
        .run(&s, "alice", tx(vec![("z", Some(c), n)], vec![present("a.bin", 0)], "late"))
        .unwrap_err();
    assert!(matches!(err, PublishError::LockBroken { .. }), "{err}");
    // after bob lets it lapse and releases, the stale token is STILL rejected (tokens never reused)
    e.run(
        &s,
        "bob",
        tx(
            vec![],
            vec![LockOp::Release {
                zone: "z".into(),
                path: "a.bin".into(),
                token: 1,
            }],
            "rel",
        ),
    )
    .unwrap();
    let err = e
        .run(&s, "alice", tx(vec![("z", Some(c), n)], vec![present("a.bin", 0)], "late2"))
        .unwrap_err();
    assert!(matches!(err, PublishError::LockBroken { .. }), "{err}");
    assert_eq!(e.manifest().next_token, 2);
}

#[test]
fn expired_unclaimed_lock_counts_as_unlocked() {
    let e = env();
    let (s, c) = base(&e);
    e.run(&s, "alice", tx(vec![], vec![acquire("a.bin", 100)], "lock")).unwrap();
    e.now.set(1100); // now == expires_at: expired
    let n = commit(&s, "z", Some(c), &[("a.bin", "2"), ("b.txt", "1"), ("d/c.bin", "1")]);
    e.run(&s, "bob", tx(vec![("z", Some(c), n)], vec![], "bob")).unwrap();
}

#[test]
fn backward_clock_never_unexpires_a_lock() {
    let e = env();
    let (s, c) = base(&e);
    e.run(&s, "alice", tx(vec![], vec![acquire("a.bin", 100)], "lock")).unwrap();
    e.now.set(1500);
    e.run(&s, "bob", tx(vec![], vec![acquire("d/c.bin", 10)], "tick")).unwrap(); // records time_hw = 1500
    e.now.set(1050); // clock jumps backward: alice's lock would look live again
    assert_eq!(e.manifest().time_hw, 1500);
    let n = commit(&s, "z", Some(c), &[("a.bin", "2"), ("b.txt", "1"), ("d/c.bin", "1")]);
    e.run(&s, "bob", tx(vec![("z", Some(c), n)], vec![], "bob"))
        .expect("lock stays expired");
    assert_eq!(e.manifest().time_hw, 1500, "time_hw never decreases");
}

#[test]
fn duplicate_acquire_applies_once_with_one_token() {
    let e = env();
    let (s, _) = base(&e);
    let t = tx(vec![], vec![acquire("a.bin", 500)], "dup");
    assert!(matches!(e.run(&s, "alice", t.clone()).unwrap(), Outcome::Applied { .. }));
    assert_eq!(e.run(&s, "alice", t).unwrap(), Outcome::AlreadyApplied);
    assert_eq!(e.manifest().next_token, 1);
}

#[test]
fn release_by_non_holder_and_break() {
    let e = env();
    let (s, c) = base(&e);
    e.run(&s, "alice", tx(vec![], vec![acquire("a.bin", 500)], "lock")).unwrap();
    let rel = LockOp::Release {
        zone: "z".into(),
        path: "a.bin".into(),
        token: 0,
    };
    let err = e.run(&s, "bob", tx(vec![], vec![rel], "steal")).unwrap_err();
    assert!(matches!(err, PublishError::NotHolder { .. }), "{err}");
    e.run(
        &s,
        "admin",
        tx(
            vec![],
            vec![LockOp::Break {
                zone: "z".into(),
                path: "a.bin".into(),
            }],
            "break",
        ),
    )
    .unwrap();
    let n = commit(&s, "z", Some(c), &[("a.bin", "2"), ("b.txt", "1"), ("d/c.bin", "1")]);
    let err = e
        .run(&s, "alice", tx(vec![("z", Some(c), n)], vec![present("a.bin", 0)], "late"))
        .unwrap_err();
    assert!(matches!(err, PublishError::LockBroken { .. }), "{err}");
}

#[test]
fn acquire_and_publish_in_one_tx_and_rename_touches_both_paths() {
    let e = env();
    let (s, c) = base(&e);
    e.run(&s, "alice", tx(vec![], vec![acquire("d/c.bin", 500)], "lock")).unwrap();
    // bob renames the locked file: both sides count
    let ren = commit(&s, "z", Some(c), &[("a.bin", "1"), ("b.txt", "1"), ("d/moved.bin", "1")]);
    let err = e.run(&s, "bob", tx(vec![("z", Some(c), ren)], vec![], "mv")).unwrap_err();
    assert!(matches!(err, PublishError::LockedBy { ref path, .. } if path == "d/c.bin"), "{err}");
    // acquire-on-publish for a free path, atomic
    let n = commit(&s, "z", Some(c), &[("a.bin", "2"), ("b.txt", "1"), ("d/c.bin", "1")]);
    e.run(&s, "bob", tx(vec![("z", Some(c), n)], vec![acquire("a.bin", 500)], "both"))
        .unwrap();
    let m = e.manifest();
    assert_eq!(m.head("z", "main"), Some(n));
    assert_eq!(m.locks[&("z".into(), "a.bin".into())].holder, "bob");
}

#[test]
fn rejected_multi_zone_tx_applies_nothing() {
    let e = env();
    let (s, c) = base(&e);
    e.run(&s, "alice", tx(vec![], vec![acquire("a.bin", 500)], "lock")).unwrap();
    let n = commit(&s, "z", Some(c), &[("a.bin", "2"), ("b.txt", "1"), ("d/c.bin", "1")]);
    let y = commit(&s, "y", None, &[("f", "1")]);
    let before = e.manifest();
    e.run(&s, "bob", tx(vec![("y", None, y), ("z", Some(c), n)], vec![], "multi"))
        .unwrap_err();
    assert_eq!(e.manifest(), before);
}

#[test]
fn mutated_validators_are_observably_wrong() {
    // Each fault must flip an outcome the correct validator gets right.
    let run =
        |fault: LockFault,
         f: &dyn Fn(&Env, &MemoryStore, ObjectId, &dyn Fn(&str, Transaction) -> Result<Outcome, PublishError>) -> bool| {
            let e = env();
            let (s, c) = base(&e);
            let faulty = |who: &str, t: Transaction| {
                Publisher::new(&e.b, &s, t)
                    .with_principal(who)
                    .with_clock(|| e.now.get())
                    .with_lock_fault(fault)
                    .run()
            };
            f(&e, &s, c, &faulty)
        };
    // skip token check: stale token accepted
    assert!(run(LockFault::SkipTokenCheck, &|e, s, c, p| {
        e.run(s, "alice", tx(vec![], vec![acquire("a.bin", 500)], "l")).unwrap();
        e.run(
            s,
            "admin",
            tx(
                vec![],
                vec![LockOp::Break {
                    zone: "z".into(),
                    path: "a.bin".into(),
                }],
                "b",
            ),
        )
        .unwrap();
        let n = commit(s, "z", Some(c), &[("a.bin", "2"), ("b.txt", "1"), ("d/c.bin", "1")]);
        p("alice", tx(vec![("z", Some(c), n)], vec![present("a.bin", 0)], "late")).is_ok()
    }));
    // release by anyone
    assert!(run(LockFault::ReleaseAnyone, &|e, s, _, p| {
        e.run(s, "alice", tx(vec![], vec![acquire("a.bin", 500)], "l")).unwrap();
        p(
            "bob",
            tx(
                vec![],
                vec![LockOp::Release {
                    zone: "z".into(),
                    path: "a.bin".into(),
                    token: 0,
                }],
                "r",
            ),
        )
        .is_ok()
    }));
    // no high-water mark: backward clock revives an expired lock
    assert!(run(LockFault::NoHighWater, &|e, s, c, p| {
        e.run(s, "alice", tx(vec![], vec![acquire("a.bin", 100)], "l")).unwrap();
        e.now.set(1500);
        e.run(s, "bob", tx(vec![], vec![acquire("d/c.bin", 10)], "t")).unwrap();
        e.now.set(1050);
        let n = commit(s, "z", Some(c), &[("a.bin", "2"), ("b.txt", "1"), ("d/c.bin", "1")]);
        p("bob", tx(vec![("z", Some(c), n)], vec![], "b")).is_err()
    }));
    // token reuse: second acquire after expiry gets the same token as the first
    assert!(run(LockFault::ReuseTokens, &|e, s, _, p| {
        e.run(s, "alice", tx(vec![], vec![acquire("a.bin", 100)], "l")).unwrap();
        e.now.set(1200);
        p("bob", tx(vec![], vec![acquire("a.bin", 100)], "b")).unwrap();
        e.manifest().locks[&("z".into(), "a.bin".into())].token == 0
    }));
}
