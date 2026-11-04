//! The per-zone sharded manifest (`sharded.rs`) driven by the REAL `Publisher`, fetch, locks-free
//! paths and remote GC through `ShardedBackend`, with crash/fault injection at every backend call.
//! Models: `model_sharded.rs`, `model_sharded_migrate.rs`, `model_sharded_gc.rs`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use vcs_objects::*;
use vcs_remote::gc::RemoteGcOptions;
use vcs_remote::protocol::object_key;
use vcs_remote::sharded::*;
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore};

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

fn commit(s: &MemoryStore, zone: &str, parent: Option<ObjectId>, n: u32) -> ObjectId {
    let blob = s.put_blob(&n.to_le_bytes()).unwrap();
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
    let mut cid = [0u8; 16];
    cid[..4].copy_from_slice(&n.to_le_bytes());
    cid[4] = zone.bytes().fold(0, |a, b| a.wrapping_add(b));
    s.put(&Object::Commit(Commit {
        zone: zone.into(),
        parents: parent.into_iter().collect(),
        root,
        change_id: ChangeId::from_bytes(cid),
        author: sig(),
        message: format!("{zone}{n}"),
    }))
    .unwrap()
}

fn tx(ups: &[(&str, Option<ObjectId>, ObjectId)], msg: &str) -> Transaction {
    Transaction {
        updates: ups
            .iter()
            .map(|(z, o, n)| RefUpdate {
                zone: (*z).into(),
                branch: "main".into(),
                old: *o,
                new: *n,
            })
            .collect(),
        author: sig(),
        message: msg.into(),
        lock_ops: vec![],
    }
}

/// Shares one MemBackend between a wrapper and the test.
#[derive(Clone)]
struct Shared(Arc<MemBackend>);
impl Backend for Shared {
    fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
        self.0.get(k)
    }
    fn get_conditional(&self, k: &str, h: &str) -> Result<Conditional, BackendError> {
        self.0.get_conditional(k, h)
    }
    fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.0.get_range(k, o, l)
    }
    fn exists(&self, k: &str) -> Result<bool, BackendError> {
        self.0.exists(k)
    }
    fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
        self.0.put_if_absent(k, d)
    }
    fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
        self.0.put_if_match(k, d, t)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
        self.0.list(p)
    }
    fn delete(&self, k: &str) -> Result<(), BackendError> {
        self.0.delete(k)
    }
    fn supports_gc(&self) -> bool {
        self.0.supports_gc()
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        self.0.now_ms()
    }
    fn modified_ms(&self, k: &str) -> Result<Option<u64>, BackendError> {
        self.0.modified_ms(k)
    }
    fn touch(&self, k: &str) -> Result<bool, BackendError> {
        self.0.touch(k)
    }
}

/// After `budget` backend calls every call fails: the client died there (the effect of the call
/// that was in flight may or may not have landed: we fail BEFORE the call, and a second variant
/// lets the call land and loses only the acknowledgement).
struct Dying {
    inner: Shared,
    budget: Arc<AtomicI64>,
    lose_ack: bool,
    tripped: Arc<AtomicBool>,
}
impl Dying {
    fn tick(&self) -> bool {
        // true = proceed; false = dead
        if self.budget.fetch_sub(1, Ordering::SeqCst) > 0 {
            true
        } else {
            self.tripped.store(true, Ordering::SeqCst);
            false
        }
    }
    fn dead<T>(&self) -> Result<T, BackendError> {
        Err(BackendError::Unavailable("client died".into()))
    }
}
impl Backend for Dying {
    fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
        if !self.tick() {
            return self.dead();
        }
        self.inner.get(k)
    }
    fn get_conditional(&self, k: &str, h: &str) -> Result<Conditional, BackendError> {
        if !self.tick() {
            return self.dead();
        }
        self.inner.get_conditional(k, h)
    }
    fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        if !self.tick() {
            return self.dead();
        }
        self.inner.get_range(k, o, l)
    }
    fn exists(&self, k: &str) -> Result<bool, BackendError> {
        if !self.tick() {
            return self.dead();
        }
        self.inner.exists(k)
    }
    fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
        if !self.tick() {
            return self.dead();
        }
        let r = self.inner.put_if_absent(k, d)?;
        if self.lose_ack && self.budget.load(Ordering::SeqCst) <= 0 {
            self.tripped.store(true, Ordering::SeqCst);
            return self.dead();
        }
        Ok(r)
    }
    fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
        if !self.tick() {
            return self.dead();
        }
        let r = self.inner.put_if_match(k, d, t)?;
        if self.lose_ack && self.budget.load(Ordering::SeqCst) <= 0 {
            self.tripped.store(true, Ordering::SeqCst);
            return self.dead();
        }
        Ok(r)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
        if !self.tick() {
            return self.dead();
        }
        self.inner.list(p)
    }
    fn delete(&self, k: &str) -> Result<(), BackendError> {
        self.inner.delete(k)
    }
    fn supports_gc(&self) -> bool {
        self.inner.supports_gc()
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        self.inner.now_ms()
    }
    fn modified_ms(&self, k: &str) -> Result<Option<u64>, BackendError> {
        self.inner.modified_ms(k)
    }
    fn touch(&self, k: &str) -> Result<bool, BackendError> {
        self.inner.touch(k)
    }
}

fn sharded_over(b: &Shared) -> ShardedBackend {
    ShardedBackend::new(Box::new(b.clone()))
}

/// A repository with zones z0..zn each at one commit, migrated to sharded.
fn repo(zones: usize) -> (Shared, MemoryStore, Vec<ObjectId>, ShardedBackend) {
    let b = Shared(Arc::new(MemBackend::new().with_gc()));
    let s = MemoryStore::new();
    let mut heads = vec![];
    for i in 0..zones {
        let z = format!("z{i}");
        let c = commit(&s, &z, None, 1);
        Publisher::new(&b, &s, tx(&[(&z, None, c)], "init")).run().unwrap();
        heads.push(c);
    }
    let rep = migrate_to_sharded(&b).unwrap();
    assert_eq!(rep.zones, zones);
    let sb = sharded_over(&b);
    (b, s, heads, sb)
}

fn heads_of(b: &dyn Backend) -> BTreeMap<(String, String), ObjectId> {
    load_manifest(b).unwrap().0.branches
}

#[test]
fn migration_preserves_manifest_and_old_readers_refuse() {
    let b = Shared(Arc::new(MemBackend::new().with_gc()));
    let s = MemoryStore::new();
    let c = commit(&s, "a", None, 1);
    let d = commit(&s, "b", None, 1);
    Publisher::new(&b, &s, tx(&[("a", None, c), ("b", None, d)], "init")).run().unwrap();
    let before = load_manifest(&b).unwrap().0;
    let rep = migrate_to_sharded(&b).unwrap();
    assert!(!rep.already);
    assert!(migrate_to_sharded(&b).unwrap().already, "idempotent");
    assert!(is_sharded(&b).unwrap());
    // an old binary (plain load) refuses with a clear error
    let e = load_manifest(&b).unwrap_err().to_string();
    assert!(e.contains("sharded"), "{e}");
    let sb = sharded_over(&b);
    let after = load_manifest(&sb).unwrap().0;
    assert_eq!(after.branches, before.branches);
    assert_eq!(after.zones, before.zones);
    assert_eq!(after.snapshots, before.snapshots);
    // fetch works through the adapter
    let local = MemoryStore::new();
    let got = fetch_heads(&sb, &local, None).unwrap();
    assert_eq!(got.len(), 2);
}

#[test]
fn publisher_through_the_adapter_and_idempotent_retry() {
    let (_b, s, h, sb) = repo(3);
    let c2 = commit(&s, "z1", Some(h[1]), 2);
    let t = tx(&[("z1", Some(h[1]), c2)], "m2");
    assert!(matches!(Publisher::new(&sb, &s, t.clone()).run().unwrap(), Outcome::Applied { .. }));
    assert_eq!(heads_of(&sb)[&("z1".into(), "main".into())], c2);
    assert_eq!(heads_of(&sb)[&("z0".into(), "main".into())], h[0]);
    // the same transaction again is recognised, not re-applied
    assert!(matches!(Publisher::new(&sb, &s, t).run().unwrap(), Outcome::AlreadyApplied));
    // stale old head is Diverged
    let c3 = commit(&s, "z1", Some(h[1]), 3);
    let e = Publisher::new(&sb, &s, tx(&[("z1", Some(h[1]), c3)], "m3")).run().unwrap_err();
    assert!(matches!(e, PublishError::Diverged { .. }), "{e}");
}

#[test]
fn new_zone_after_migration_and_snapshot_roundtrip() {
    let (_b, s, _h, sb) = repo(1);
    let c = commit(&s, "fresh", None, 1);
    Publisher::new(&sb, &s, tx(&[("fresh", None, c)], "new zone"))
        .with_zones([("fresh".to_owned(), "fresh".to_owned())].into_iter().collect())
        .run()
        .unwrap();
    let m = load_manifest(&sb).unwrap().0;
    assert_eq!(m.zones.get("fresh").map(String::as_str), Some("fresh"));
    assert_eq!(m.head("fresh", "main"), Some(c));
}

#[test]
fn disjoint_writers_land_every_publish() {
    let (b, s, h, _sb) = repo(8);
    let s = Arc::new(s);
    let sb = Arc::new(sharded_over(&b));
    let finals: Vec<ObjectId> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..8usize)
            .map(|i| {
                let (s, sb, h) = (s.clone(), sb.clone(), h.clone());
                sc.spawn(move || {
                    let z = format!("z{i}");
                    let mut head = h[i];
                    for n in 2..12u32 {
                        let c = commit(&s, &z, Some(head), n);
                        Publisher::new(&*sb, &*s, tx(&[(&z, Some(head), c)], &format!("{z}-{n}")))
                            .run()
                            .unwrap();
                        head = c;
                    }
                    head
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let m = load_manifest(&*sb).unwrap().0;
    for (i, f) in finals.iter().enumerate() {
        assert_eq!(m.head(&format!("z{i}"), "main"), Some(*f), "writer {i} lost an update");
    }
}

/// All-or-nothing across zones at EVERY backend call, with the client dying (or losing only the
/// ack) there; then anyone re-running the transaction, or just a reader, sees all or nothing.
#[test]
fn multi_zone_publish_survives_a_crash_at_every_backend_call() {
    for lose_ack in [false, true] {
        let mut budget = 0i64;
        loop {
            let (b, s, h, _sb) = repo(3);
            let ca = commit(&s, "z0", Some(h[0]), 2);
            let cb = commit(&s, "z2", Some(h[2]), 2);
            let t = tx(&[("z0", Some(h[0]), ca), ("z2", Some(h[2]), cb)], "pair");
            let tripped = Arc::new(AtomicBool::new(false));
            let dying = Dying {
                inner: b.clone(),
                budget: Arc::new(AtomicI64::new(budget)),
                lose_ack,
                tripped: tripped.clone(),
            };
            let sb = ShardedBackend::new(Box::new(dying));
            let r = Publisher::new(&sb, &s, t.clone()).run();
            // a healthy reader: all or nothing
            let healthy = sharded_over(&b);
            let m = load_manifest(&healthy).unwrap().0;
            let (a, c) = (m.head("z0", "main").unwrap(), m.head("z2", "main").unwrap());
            let new = (a == ca, c == cb);
            assert_eq!(new.0, new.1, "torn pair after budget {budget} lose_ack {lose_ack}: {r:?}");
            // a fresh publisher of the same transaction converges (or reports it already applied)
            let out = Publisher::new(&healthy, &s, t.clone()).run();
            assert!(out.is_ok(), "re-run after budget {budget}: {out:?}");
            let m = load_manifest(&healthy).unwrap().0;
            assert_eq!(m.head("z0", "main"), Some(ca));
            assert_eq!(m.head("z2", "main"), Some(cb));
            // no pending record survives a healthy re-run
            for n in Engine::new(&healthy).names().unwrap() {
                assert!(Engine::new(&healthy).read(&n).unwrap().0.pending.is_none(), "pending left on {n}");
            }
            if !tripped.load(Ordering::SeqCst) {
                assert!(r.is_ok(), "no fault was injected but the publish failed: {r:?}");
                break;
            }
            budget += 1;
            assert!(budget < 400, "never completed");
        }
        println!("multi-zone publish: completes without a fault after {budget} backend calls (lose_ack={lose_ack})");
    }
}

/// Another transaction on the same zones finds the dead writer's pending records and resolves them
/// (helping), without the dead writer ever coming back.
#[test]
fn a_dead_writers_pending_records_are_resolved_by_the_next_writer() {
    let (b, s, h, sb) = repo(2);
    // prepare by hand: pending on z0 and z1 for a tx that never decides
    let eng = Engine::new(&b);
    let ghost = ObjectId::from_bytes([7; 32]);
    let x = commit(&s, "z0", Some(h[0]), 9);
    let y = commit(&s, "z1", Some(h[1]), 9);
    for (z, o, n) in [("z0", h[0], x), ("z1", h[1], y)] {
        let (mut p, tag) = eng.read(z).unwrap();
        p.pending = Some((
            ghost,
            PartDelta {
                heads: vec![("main".into(), Some(o), Some(n))],
                ..Default::default()
            },
        ));
        p.generation += 1;
        assert!(b.put_if_match(&part_key(z), &p.encode(), &tag.unwrap()).unwrap().is_some());
    }
    // readers see the old state (undecided), a new writer aborts the ghost and proceeds
    assert_eq!(heads_of(&sb)[&("z0".into(), "main".into())], h[0]);
    let c = commit(&s, "z0", Some(h[0]), 3);
    Publisher::new(&sb, &s, tx(&[("z0", Some(h[0]), c)], "next")).run().unwrap();
    assert_eq!(heads_of(&sb)[&("z0".into(), "main".into())], c);
    // the ghost is aborted for good: re-running it cannot revive it
    assert_eq!(eng.decision(&ghost).unwrap().map(|d| d.0), Some(false));
}

/// Stress: pairs of zones written together; a concurrent reader must always see a matching pair.
#[test]
fn atomic_pair_invariant_under_threads() {
    let (b, s, _h, _sb) = repo(2);
    let s = Arc::new(s);
    let sbs: Vec<Arc<ShardedBackend>> = (0..4).map(|_| Arc::new(sharded_over(&b))).collect();
    let tagmap: Arc<Mutex<HashMap<ObjectId, u32>>> = Arc::new(Mutex::new(HashMap::new()));
    let stop = Arc::new(AtomicBool::new(false));
    std::thread::scope(|sc| {
        let reader_b = sharded_over(&b);
        let (tm, st) = (tagmap.clone(), stop.clone());
        let rd = sc.spawn(move || {
            let mut checks = 0;
            while !st.load(Ordering::SeqCst) {
                let m = load_manifest(&reader_b).unwrap().0;
                let (a, c) = (m.head("z0", "main").unwrap(), m.head("z1", "main").unwrap());
                let t = tm.lock().unwrap();
                if let (Some(x), Some(y)) = (t.get(&a), t.get(&c)) {
                    assert_eq!(x, y, "torn read: z0 from tx {x}, z1 from tx {y}");
                }
                checks += 1;
            }
            checks
        });
        let mut ws = vec![];
        for (w, sb) in sbs.iter().enumerate() {
            let (s, sb, tagmap) = (s.clone(), sb.clone(), tagmap.clone());
            ws.push(sc.spawn(move || {
                let mut done = 0;
                for i in 0..15u32 {
                    let n = 100 + (w as u32) * 100 + i;
                    loop {
                        let m = load_manifest(&*sb).unwrap().0;
                        let (a, c) = (m.head("z0", "main").unwrap(), m.head("z1", "main").unwrap());
                        let (na, nb) = (commit(&s, "z0", Some(a), n), commit(&s, "z1", Some(c), n));
                        tagmap.lock().unwrap().insert(na, n);
                        tagmap.lock().unwrap().insert(nb, n);
                        match Publisher::new(&*sb, &*s, tx(&[("z0", Some(a), na), ("z1", Some(c), nb)], &format!("w{w}-{i}"))).run() {
                            Ok(_) => {
                                done += 1;
                                break;
                            }
                            Err(PublishError::Diverged { .. }) => continue,
                            Err(e) => panic!("{e}"),
                        }
                    }
                }
                done
            }));
        }
        let total: i32 = ws.into_iter().map(|w| w.join().unwrap()).sum();
        stop.store(true, Ordering::SeqCst);
        let checks = rd.join().unwrap();
        println!("pair stress: {total} commits, {checks} reader checks");
        assert_eq!(total, 60);
    });
}

fn id(n: u64) -> ObjectId {
    ObjectId::from_bytes(*blake3::hash(&n.to_le_bytes()).as_bytes())
}

/// Decision GC + per-shard applied ids: after GC, re-running a committed txid after an ABA revert
/// is a no-op (the model's S-ONCE), and the decision record is gone.
#[test]
fn decision_gc_then_rerun_is_a_noop_even_after_revert() {
    let b = Shared(Arc::new(MemBackend::new().with_gc()));
    let eng = Engine::new(&b);
    let delta = |o: Option<ObjectId>, n: Option<ObjectId>| PartDelta {
        heads: vec![("main".into(), o, n)],
        ..Default::default()
    };
    let t1 = id(1);
    let fwd: BTreeMap<String, PartDelta> = [
        ("a".to_owned(), delta(None, Some(id(10)))),
        ("b".to_owned(), delta(None, Some(id(11)))),
    ]
    .into();
    assert_eq!(eng.commit(t1, &fwd).unwrap(), CommitOutcome::Committed);
    let back: BTreeMap<String, PartDelta> = [
        ("a".to_owned(), delta(Some(id(10)), None)),
        ("b".to_owned(), delta(Some(id(11)), None)),
    ]
    .into();
    assert_eq!(eng.commit(id(2), &back).unwrap(), CommitOutcome::Committed);
    assert!(eng.decision(&t1).unwrap().is_some());
    let rep = gc_decisions(&b, u64::MAX).unwrap();
    assert_eq!(rep.committed_deleted, 2, "{rep:?}");
    assert!(eng.decision(&t1).unwrap().is_none());
    // the re-run of t1 must NOT re-apply it
    assert_eq!(eng.commit(t1, &fwd).unwrap(), CommitOutcome::Committed);
    let (pa, _) = eng.read("a").unwrap();
    assert!(pa.state.heads.is_empty(), "t1 was applied a second time after GC");
}

/// Aborted decisions are only collected when old.
#[test]
fn aborted_decisions_wait_for_their_grace() {
    let b = Shared(Arc::new(MemBackend::new().with_gc()));
    b.0.set_now_ms(1_000);
    let eng = Engine::new(&b);
    let delta = |o: Option<ObjectId>, n: Option<ObjectId>| PartDelta {
        heads: vec![("main".into(), o, n)],
        ..Default::default()
    };
    let bad: BTreeMap<String, PartDelta> = [
        ("a".to_owned(), delta(Some(id(5)), Some(id(6)))),
        ("b".to_owned(), delta(None, Some(id(7)))),
    ]
    .into();
    assert_eq!(eng.commit(id(3), &bad).unwrap(), CommitOutcome::Conflict);
    assert_eq!(gc_decisions(&b, 1_000_000).unwrap().aborted_deleted, 0);
    b.0.advance_ms(2_000_000);
    assert_eq!(gc_decisions(&b, 1_000_000).unwrap().aborted_deleted, 1);
}

/// Remote GC on a sharded repository: roots come from every shard; an object referenced ONLY by a
/// pending (prepared) record survives, and the same object is collected when no record refers to it.
#[test]
fn remote_gc_keeps_objects_of_pending_records() {
    const DAY: u64 = 86_400_000;
    for with_pending in [true, false] {
        let (b, s, h, sb) = repo(2);
        b.0.set_now_ms(1_000_000);
        let orphan = commit(&s, "z0", Some(h[0]), 77);
        let mut seen = HashSet::new();
        fn up(b: &dyn Backend, s: &MemoryStore, id: ObjectId, seen: &mut HashSet<ObjectId>) {
            if !seen.insert(id) {
                return;
            }
            let o = s.get(&id).unwrap();
            let deps: Vec<ObjectId> = match &o {
                Object::Commit(c) => c.parents.iter().copied().chain([c.root]).collect(),
                Object::Tree(t) => t.entries().iter().map(|e| e.id).collect(),
                _ => vec![],
            };
            for d in deps {
                up(b, s, d, seen);
            }
            b.put_if_absent(&object_key(&id), &o.encode()).unwrap();
        }
        up(&b, &s, orphan, &mut seen);
        for hh in &h {
            up(&b, &s, *hh, &mut seen);
        }
        if with_pending {
            let eng = Engine::new(&b);
            let (mut p, tag) = eng.read("z0").unwrap();
            p.pending = Some((
                id(99),
                PartDelta {
                    heads: vec![("main".into(), Some(h[0]), Some(orphan))],
                    ..Default::default()
                },
            ));
            p.generation += 1;
            b.put_if_match(&part_key("z0"), &p.encode(), &tag.unwrap()).unwrap().unwrap();
        }
        b.0.advance_ms(10 * DAY);
        let opts = RemoteGcOptions {
            grace_ms: DAY,
            hold_ms: 3_600_000,
            dry_run: false,
            lock_ttl_ms: 6 * 3_600_000,
            owner: "t".into(),
            allow_short_grace: false,
            rewrite_packs: true,
        };
        let rep = remote_gc(&sb, &opts).unwrap();
        let alive = b.exists(&object_key(&orphan)).unwrap();
        assert_eq!(alive, with_pending, "with_pending={with_pending} {rep:?}");
        // heads of both zones are intact either way
        for hh in &h {
            assert!(b.exists(&object_key(hh)).unwrap());
        }
    }
}

/// Locks through the adapter on a REAL directory backend (FsBackend skips dotfiles when listing:
/// the root record must be listed, or the lock token counter vanishes and the composed manifest is invalid).
#[test]
fn locks_on_a_directory_backend_survive_composition() {
    let dir = tempfile::tempdir().unwrap();
    let raw = FsBackend::open(dir.path()).unwrap();
    let s = MemoryStore::new();
    let c1 = commit(&s, "z", None, 1);
    Publisher::new(&raw, &s, tx(&[("z", None, c1)], "base")).run().unwrap();
    migrate_to_sharded(&raw).unwrap();
    let sb = ShardedBackend::new(Box::new(FsBackend::open(dir.path()).unwrap()));
    let lock = |who: &str, ops: Vec<LockOp>, upd: Vec<(&str, Option<ObjectId>, ObjectId)>| {
        let mut t = tx(&upd, "m");
        t.lock_ops = ops;
        Publisher::new(&sb, &s, t).with_principal(who).with_clock(|| 1000).run()
    };
    let acq = LockOp::Acquire {
        zone: "z".into(),
        path: "f".into(),
        lease_ms: 60_000,
        reason: String::new(),
    };
    lock("alice", vec![acq], vec![]).unwrap();
    let m = load_manifest(&sb).unwrap().0; // decode validates "lock token below next_token"
    assert_eq!(m.locks[&("z".into(), "f".into())].holder, "alice");
    assert_eq!(m.next_token, 1);
    // bob may not publish a change to the locked path; alice may (she presents the token)
    let c2 = commit(&s, "z", Some(c1), 2);
    let e = lock("bob", vec![], vec![("z", Some(c1), c2)]).unwrap_err();
    assert!(e.to_string().contains("locked by alice"), "{e}");
    assert_eq!(
        load_manifest(&sb).unwrap().0.head("z", "main"),
        Some(c1),
        "refused push left no trace"
    );
}
