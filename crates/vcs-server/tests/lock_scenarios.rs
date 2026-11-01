//! The `model_locks.rs` scenarios (docs/locks.md "Model checker plan") replayed through the real
//! HTTP server path: one deterministic schedule per scenario, with the server clock as the `Tick`.
//! The exhaustive-interleaving search stays in vcs-remote; this proves the server enforces the same
//! outcomes, and that `time_hw` is stored in the same CAS as the lock change.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use vcs_objects::*;
use vcs_remote::protocol::object_key;
use vcs_remote::*;
use vcs_server::{Config, HttpBackend, Policy, Principal, Role, Server, TokenTable};
use vcs_store::{MemoryStore, ObjectStore};

fn p(name: &str, role: Role) -> Principal {
    Principal {
        name: name.into(),
        role,
        zones: None::<BTreeSet<String>>,
    }
}
fn table() -> TokenTable {
    TokenTable::from_entries(vec![
        ("t-alice", p("alice", Role::Write)),
        ("t-bob", p("bob", Role::Write)),
        ("t-root", p("root", Role::Admin)),
    ])
}

/// A server with a settable clock over `mem`.
fn start(mem: Arc<MemBackend>, clock: Arc<AtomicU64>, policy: &str, eps: u64) -> (Server, String) {
    let mut cfg = Config::new(table());
    let c = clock.clone();
    cfg.clock = Arc::new(move || c.load(Ordering::SeqCst));
    cfg.validator.policy = Policy::parse(policy).unwrap();
    cfg.validator.skew_epsilon_ms = eps;
    cfg.validator.max_lease_ms = 3_600_000;
    let srv = Server::start(mem, cfg, "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    (srv, url)
}
fn client(url: &str, tok: &str) -> HttpBackend {
    HttpBackend::new(url, tok).unwrap()
}

fn sig(who: &str) -> Signature {
    Signature {
        name: who.into(),
        email: who.into(),
        timestamp: 1,
    }
}

fn commit(s: &MemoryStore, zone: &str, parent: Option<ObjectId>, files: &[(&str, &str)]) -> ObjectId {
    let entries = files
        .iter()
        .map(|(n, c)| TreeEntry {
            name: (*n).into(),
            mode: EntryMode::File,
            id: s.put_blob(c.as_bytes()).unwrap(),
        })
        .collect();
    let t = s.put(&Object::Tree(Tree::new(entries).unwrap())).unwrap();
    s.put(&Object::Commit(Commit {
        zone: zone.into(),
        parents: parent.into_iter().collect(),
        root: t,
        change_id: ChangeId::from_bytes([files.len() as u8; 16]),
        author: sig("c"),
        message: format!("{files:?}"),
    }))
    .unwrap()
}

fn tx(zone: &str, old: Option<ObjectId>, new: ObjectId, lock_ops: Vec<LockOp>, who: &str) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: zone.into(),
            branch: "main".into(),
            old,
            new,
        }],
        author: sig(who),
        message: "m".into(),
        lock_ops,
    }
}

fn publish(b: &dyn Backend, s: &MemoryStore, t: Transaction, principal: &str, now: u64) -> Result<Outcome, PublishError> {
    Publisher::new(b, s, t).with_principal(principal).with_clock(move || now).run()
}

fn put_closure(b: &dyn Backend, s: &MemoryStore, id: ObjectId) {
    let o = s.get(&id).unwrap();
    let deps: Vec<ObjectId> = match &o {
        Object::Tree(t) => t.entries().iter().map(|e| e.id).collect(),
        Object::Commit(c) => c.parents.iter().copied().chain([c.root]).collect(),
        _ => vec![],
    };
    for d in deps {
        put_closure(b, s, d);
    }
    b.put_if_absent(&object_key(&id), &o.encode()).unwrap();
}

fn acquire(path: &str, lease: u64) -> LockOp {
    LockOp::Acquire {
        zone: "z".into(),
        path: path.into(),
        lease_ms: lease,
        reason: String::new(),
    }
}
fn lock_of(b: &dyn Backend, path: &str) -> Option<Lock> {
    load_manifest(b).unwrap().0.locks.get(&("z".to_owned(), path.to_owned())).cloned()
}

struct Env {
    _srv: Server,
    url: String,
    clock: Arc<AtomicU64>,
    s: MemoryStore,
    base: ObjectId,
}

fn env(eps: u64) -> Env {
    let clock = Arc::new(AtomicU64::new(1_000));
    let (srv, url) = start(Arc::new(MemBackend::new()), clock.clone(), "", eps);
    let s = MemoryStore::new();
    let base = commit(&s, "z", None, &[("a.bin", "0"), ("b.txt", "0")]);
    publish(&client(&url, "t-alice"), &s, tx("z", None, base, vec![], "alice"), "alice", 1_000).unwrap();
    Env {
        _srv: srv,
        url,
        clock,
        s,
        base,
    }
}

impl Env {
    fn tick(&self, to: u64) {
        self.clock.store(to, Ordering::SeqCst);
    }
    fn now(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }
    fn tok(who: &str) -> &'static str {
        match who {
            "alice" => "t-alice",
            "bob" => "t-bob",
            _ => "t-root",
        }
    }
    /// Lock-only transaction (no head change).
    fn lock_only(&self, who: &str, ops: Vec<LockOp>) -> Result<Outcome, PublishError> {
        let mut t = tx("z", Some(self.base), self.base, ops, who);
        t.updates.clear();
        publish(&client(&self.url, Self::tok(who)), &self.s, t, who, self.now())
    }
    /// Change `a.bin` on top of base, with `ops`.
    fn edit(&self, who: &str, content: &str, ops: Vec<LockOp>) -> (ObjectId, Result<Outcome, PublishError>) {
        let c = commit(&self.s, "z", Some(self.base), &[("a.bin", content), ("b.txt", "0")]);
        let cur = load_manifest(&client(&self.url, Self::tok(who))).unwrap().0.head("z", "main");
        let r = publish(
            &client(&self.url, Self::tok(who)),
            &self.s,
            tx("z", cur, c, ops, who),
            who,
            self.now(),
        );
        (c, r)
    }
    fn manifest(&self) -> Manifest {
        load_manifest(&client(&self.url, "t-root")).unwrap().0
    }
    fn lock(&self) -> Option<Lock> {
        lock_of(&client(&self.url, "t-root"), "a.bin")
    }
}

fn present(token: u64) -> LockOp {
    LockOp::Present {
        zone: "z".into(),
        path: "a.bin".into(),
        token,
    }
}

#[test]
fn s1_acquire_race_exactly_one_holder() {
    let e = env(0);
    e.lock_only("alice", vec![acquire("a.bin", 1000)]).unwrap();
    let err = e.lock_only("bob", vec![acquire("a.bin", 1000)]).unwrap_err();
    assert!(
        matches!(err, PublishError::LockedBy { ref holder, .. } if holder == "alice"),
        "{err}"
    );
    let l = e.lock().unwrap();
    assert_eq!((l.holder.as_str(), l.token), ("alice", 0));
}

#[test]
fn s2_holder_publishes_contender_rejected() {
    let e = env(0);
    e.lock_only("alice", vec![acquire("a.bin", 1000)]).unwrap();
    let (_, r) = e.edit("bob", "B", vec![]);
    assert!(matches!(r.unwrap_err(), PublishError::LockedBy { .. }));
    let (c, r) = e.edit("alice", "A", vec![present(0)]);
    r.unwrap();
    assert_eq!(e.manifest().head("z", "main"), Some(c));
}

#[test]
fn s3_expiry_reacquire_then_old_holder_is_rejected() {
    let e = env(0);
    e.lock_only("alice", vec![acquire("a.bin", 150)]).unwrap();
    e.tick(1_200); // past expiry (1000 + 150)
    let (cb, r) = e.edit("bob", "B", vec![acquire("a.bin", 1000)]);
    r.unwrap();
    assert_eq!(e.lock().unwrap().holder, "bob");
    // alice recovers and retries with her old token: rejected, her commit is not the head
    let (ca, r) = e.edit("alice", "A", vec![present(0)]);
    let err = r.unwrap_err();
    assert!(
        matches!(err, PublishError::LockBroken { .. } | PublishError::LockedBy { .. }),
        "{err}"
    );
    assert_eq!(e.manifest().head("z", "main"), Some(cb));
    assert_ne!(e.manifest().head("z", "main"), Some(ca));
}

#[test]
fn s4_admin_break_vs_holder_publish() {
    let e = env(0);
    e.lock_only("alice", vec![acquire("a.bin", 1000)]).unwrap();
    // a non-admin cannot break
    let brk = || LockOp::Break {
        zone: "z".into(),
        path: "a.bin".into(),
    };
    assert!(e.lock_only("bob", vec![brk()]).is_err());
    e.lock_only("root", vec![brk()]).unwrap();
    assert!(e.manifest().locks.is_empty(), "lock is gone");
    let (_, r) = e.edit("alice", "A", vec![present(0)]);
    assert!(
        matches!(r.unwrap_err(), PublishError::LockBroken { .. }),
        "holder's token is dead after a break"
    );
}

#[test]
fn s5_renew_vs_expiry_vs_acquire() {
    let e = env(0);
    e.lock_only("alice", vec![acquire("a.bin", 150)]).unwrap();
    e.tick(1_200);
    e.lock_only("bob", vec![acquire("a.bin", 1000)]).unwrap();
    let renew = LockOp::Renew {
        zone: "z".into(),
        path: "a.bin".into(),
        token: 0,
        lease_ms: 1000,
    };
    assert!(e.lock_only("alice", vec![renew]).is_err(), "renew after losing the lock must fail");
    assert_eq!(e.lock().unwrap().holder, "bob");
}

#[test]
fn s6_rejected_multi_zone_tx_leaves_no_partial_effect() {
    let e = env(0);
    e.lock_only("alice", vec![acquire("a.bin", 1000)]).unwrap();
    let before = e.manifest();
    // bob updates zone z (locked path) and creates zone w in one transaction
    let cz = commit(&e.s, "z", Some(e.base), &[("a.bin", "B"), ("b.txt", "0")]);
    let cw = commit(&e.s, "w", None, &[("f", "1")]);
    let t = Transaction {
        updates: vec![
            RefUpdate {
                zone: "z".into(),
                branch: "main".into(),
                old: before.head("z", "main"),
                new: cz,
            },
            RefUpdate {
                zone: "w".into(),
                branch: "main".into(),
                old: None,
                new: cw,
            },
        ],
        author: sig("bob"),
        message: "m".into(),
        lock_ops: vec![],
    };
    let r = publish(&client(&e.url, "t-bob"), &e.s, t, "bob", e.now());
    assert!(matches!(r.unwrap_err(), PublishError::LockedBy { .. }));
    let after = e.manifest();
    assert_eq!(after.generation, before.generation, "manifest unchanged");
    assert_eq!(after.head("w", "main"), None, "no partial effect in the second zone");
    assert_eq!(after.head("z", "main"), before.head("z", "main"));
}

#[test]
fn s7_duplicate_acquire_is_applied_once() {
    let e = env(0);
    let mut t = tx("z", Some(e.base), e.base, vec![acquire("a.bin", 1000)], "alice");
    t.updates.clear();
    let a = client(&e.url, "t-alice");
    publish(&a, &e.s, t.clone(), "alice", e.now()).unwrap();
    let m1 = e.manifest();
    publish(&a, &e.s, t, "alice", e.now()).unwrap();
    let m2 = e.manifest();
    assert_eq!(m1.next_token, 1);
    assert_eq!(
        (m2.next_token, m2.generation),
        (m1.next_token, m1.generation),
        "second submission was a no-op"
    );
    assert_eq!(m2.locks, m1.locks);
}

#[test]
fn s9_clock_jumps_never_move_time_hw_back_and_it_is_stored_in_the_same_cas() {
    let e = env(0);
    e.tick(5_000);
    e.lock_only("alice", vec![acquire("a.bin", 1000)]).unwrap();
    let m = e.manifest();
    assert_eq!(m.time_hw, 5_000, "time_hw is the server time written with the lock change");
    assert_eq!(e.lock().unwrap().expires_at, 6_000);
    // server clock jumps backward: the lock is still judged against the high-water mark
    e.tick(2_000);
    let err = e.lock_only("bob", vec![acquire("a.bin", 1000)]).unwrap_err();
    assert!(matches!(err, PublishError::LockedBy { .. }), "{err}");
    let (_, r) = e.edit("alice", "A", vec![present(0)]);
    r.unwrap();
    assert!(e.manifest().time_hw >= 5_000, "time_hw never decreases");
    // forward jump past expiry: contender takes it
    e.tick(9_000);
    e.lock_only("bob", vec![acquire("a.bin", 1000)]).unwrap();
    assert_eq!(e.manifest().time_hw, 9_000);
    assert_eq!(e.lock().unwrap().holder, "bob");
}
