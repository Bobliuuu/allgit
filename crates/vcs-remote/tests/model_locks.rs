//! Exhaustive-interleaving model of lock operations (docs/locks.md, "Model checker plan").
//!
//! Same style as `model.rs`: every interleaving of the writers' protocol steps, plus one
//! injected fault (crash at any step, or a lost CAS request/ack) per schedule, plus explicit
//! `Tick` events so lease expiry interleaves with every other step; one scenario also jumps
//! the validator clock forward and backward. Objects are pre-seeded on the remote so the
//! search focuses on Load/Cas (upload order is covered by `model.rs`).
//!
//! Invariants are recomputed from the stored `tx/<txid>` records and the manifest history, not
//! from validator output: L-ATOMIC, L-EXCL, L-FENCE, L-EXPIRY, L-IDEM, L-VISIBLE.
//! Mutation checks (`#[should_panic]`) run the same search against a deliberately broken
//! validator and require the checker to fail.
//!
//! Not covered (see docs/locks.md): real clocks, `require-lock` (not implemented), replicas with
//! clock skew (epsilon is 0 here), authentication, contention cost.

use std::cell::Cell;
use std::collections::BTreeMap;
use vcs_objects::*;
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore};

const ZONE: &str = "z";
const T0: u64 = 1000;
const TICK: u64 = 100;

struct Writer {
    store: MemoryStore,
    tx: Transaction,
    who: &'static str,
}

struct Scenario {
    name: &'static str,
    build: fn() -> Vec<Writer>,
    /// Fault-free preparation run before the schedule (e.g. an existing lock).
    setup: fn(&MemBackend, &Cell<u64>, &[Writer]),
    ticks: usize,
    /// Allow one forward (+400) and one backward (-500) validator clock jump.
    jumps: bool,
    expect: fn(&[bool], &Manifest, &[Transaction]),
    min_leaves: u64,
}

#[derive(Clone, Copy, Debug)]
enum Choice {
    Step(usize),
    Fault(usize, CasFault),
    Crash(usize),
    Tick,
    Fwd,
    Back,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum W {
    Running,
    Done(bool),
    Crashed,
}

fn sig(who: &str) -> Signature {
    Signature {
        name: who.into(),
        email: who.into(),
        timestamp: 1,
    }
}

fn mk_commit(store: &MemoryStore, zone: &str, parent: Option<ObjectId>, files: &[(&str, &str)]) -> ObjectId {
    let entries = files
        .iter()
        .map(|(n, c)| TreeEntry {
            name: (*n).into(),
            mode: EntryMode::File,
            id: store.put_blob(c.as_bytes()).unwrap(),
        })
        .collect();
    let t = store.put(&Object::Tree(Tree::new(entries).unwrap())).unwrap();
    store
        .put(&Object::Commit(Commit {
            zone: zone.into(),
            parents: parent.into_iter().collect(),
            root: t,
            change_id: ChangeId::from_bytes([files.len() as u8; 16]),
            author: sig("c"),
            message: format!("{files:?}"),
        }))
        .unwrap()
}

fn tx(who: &str, updates: Vec<(&str, Option<ObjectId>, ObjectId)>, lock_ops: Vec<LockOp>, msg: &str) -> Transaction {
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
        author: sig(who),
        message: msg.into(),
        lock_ops,
    }
}

fn acquire(path: &str, lease_ms: u64) -> LockOp {
    LockOp::Acquire {
        zone: ZONE.into(),
        path: path.into(),
        lease_ms,
        reason: String::new(),
    }
}
fn present(path: &str, token: u64) -> LockOp {
    LockOp::Present {
        zone: ZONE.into(),
        path: path.into(),
        token,
    }
}

fn base_files() -> [(&'static str, &'static str); 2] {
    [("a.bin", "0"), ("b.txt", "0")]
}

fn deps_of(o: &Object) -> Vec<ObjectId> {
    match o {
        Object::Tree(t) => t.entries().iter().map(|e| e.id).collect(),
        Object::Commit(c) => c.parents.iter().copied().chain([c.root]).collect(),
        _ => vec![],
    }
}

fn obj_key(id: &ObjectId) -> String {
    let h = id.to_hex();
    format!("objects/{}/{}", &h[..2], &h[2..])
}

/// Put the closure of `id` on the remote, dependencies first.
fn seed(b: &MemBackend, store: &MemoryStore, id: ObjectId) {
    let o = store.get(&id).unwrap();
    for d in deps_of(&o) {
        seed(b, store, d);
    }
    b.put_if_absent(&obj_key(&id), &o.encode()).unwrap();
}

fn run_one(
    b: &MemBackend,
    clock: &Cell<u64>,
    store: &MemoryStore,
    who: &str,
    t: Transaction,
    fault: Option<LockFault>,
) -> Result<Outcome, PublishError> {
    let mut p = Publisher::new(b, store, t).with_principal(who).with_clock(|| clock.get());
    if let Some(f) = fault {
        p = p.with_lock_fault(f);
    }
    p.run()
}

/// Publish the base commit (files a.bin, b.txt) fault-free as principal "setup".
fn publish_base(b: &MemBackend, clock: &Cell<u64>, ws: &[Writer]) -> ObjectId {
    let c0 = mk_commit(&ws[0].store, ZONE, None, &base_files());
    run_one(
        b,
        clock,
        &ws[0].store,
        "setup",
        tx("setup", vec![(ZONE, None, c0)], vec![], "base"),
        None,
    )
    .unwrap();
    c0
}

/// Writer stores each hold the base commit and their own new commit on top of it.
fn writers_on_base(specs: &[(&'static str, &str)]) -> Vec<Writer> {
    specs
        .iter()
        .map(|(who, content)| {
            let store = MemoryStore::new();
            let c0 = mk_commit(&store, ZONE, None, &base_files());
            let n = mk_commit(&store, ZONE, Some(c0), &[("a.bin", content), ("b.txt", "0")]);
            let t = tx(who, vec![(ZONE, Some(c0), n)], vec![], who);
            Writer { store, tx: t, who }
        })
        .collect()
}

fn flat_tree(b: &MemBackend, commit: Option<ObjectId>) -> BTreeMap<String, ObjectId> {
    let Some(c) = commit else { return BTreeMap::new() };
    let get = |id: &ObjectId| Object::decode(&b.get(&obj_key(id)).unwrap().expect("object on remote").data).unwrap();
    let Object::Commit(c) = get(&c) else { panic!("not a commit") };
    let Object::Tree(t) = get(&c.root) else { panic!("not a tree") };
    t.entries().iter().map(|e| (e.name.clone(), e.id)).collect()
}

/// Independent (flat-tree) computation of the changed paths of one ref update.
fn changed(b: &MemBackend, old: Option<ObjectId>, new: ObjectId) -> Vec<String> {
    let (o, n) = (flat_tree(b, old), flat_tree(b, Some(new)));
    let mut out: Vec<String> = n.iter().filter(|(k, v)| o.get(*k) != Some(*v)).map(|(k, _)| k.clone()).collect();
    out.extend(o.keys().filter(|k| !n.contains_key(*k)).cloned());
    out
}

fn check_backend(b: &MemBackend, ctx: &str) -> Manifest {
    for k in b.keys().into_iter().filter(|k| k.starts_with("objects/")) {
        let data = b.get(&k).unwrap().unwrap().data;
        let o = Object::decode(&data).unwrap_or_else(|e| panic!("{ctx}: undecodable {k}: {e}"));
        assert_eq!(obj_key(&id_of_encoded(&data)), k, "{ctx}: hash mismatch");
        for d in deps_of(&o) {
            assert!(b.exists(&obj_key(&d)).unwrap(), "{ctx}: {k} present but dependency {d} missing");
        }
    }
    let hist: Vec<Manifest> = b
        .history()
        .into_iter()
        .filter(|(k, _)| k == "manifest")
        .map(|(_, d)| Manifest::decode(&d).unwrap())
        .collect();
    let mut prev = Manifest::default();
    let mut token_owner: BTreeMap<u64, (String, String, String)> = BTreeMap::new();
    for (i, m) in hist.iter().enumerate() {
        // L-ATOMIC: exactly one whole transaction per version
        assert_eq!(m.generation, prev.generation + 1, "{ctx}: generation gap at {i}");
        assert_eq!(
            &m.applied[..m.applied.len() - 1],
            &prev.applied[..],
            "{ctx}: applied history rewritten"
        );
        let txid = *m.applied.last().unwrap();
        assert!(!prev.applied.contains(&txid), "{ctx}: L-IDEM: tx {txid} applied twice");
        let Object::Transaction(t) = Object::decode(
            &b.get(&format!("tx/{}", txid.to_hex()))
                .unwrap()
                .expect("tx record exists before CAS")
                .data,
        )
        .unwrap() else {
            panic!()
        };
        let principal = t.author.email.clone();
        let mut expect = prev.clone();
        expect.generation += 1;
        expect.applied = m.applied.clone();
        for u in &t.updates {
            assert_eq!(prev.head(&u.zone, &u.branch), u.old, "{ctx}: stale precondition (lost update)");
            expect.branches.insert((u.zone.clone(), u.branch.clone()), u.new);
        }
        expect.locks = m.locks.clone();
        expect.next_token = m.next_token;
        expect.time_hw = m.time_hw;
        assert_eq!(&expect, m, "{ctx}: version {i} is not previous + one whole transaction");

        // L-EXPIRY: validator time never goes backward
        assert!(m.time_hw >= prev.time_hw, "{ctx}: time_hw decreased at version {i}");
        let now = m.time_hw;
        // L-FENCE: counter monotonic; tokens never shared between records
        assert!(m.next_token >= prev.next_token, "{ctx}: next_token decreased");
        for ((z, p), l) in &m.locks {
            assert!(l.token < m.next_token, "{ctx}: token >= next_token");
            let owner = (z.clone(), p.clone(), l.holder.clone());
            assert_eq!(
                token_owner.entry(l.token).or_insert(owner.clone()),
                &owner,
                "{ctx}: L-FENCE: token {} reused",
                l.token
            );
            if prev.locks.get(&(z.clone(), p.clone())).map(|x| x.token) != Some(l.token) {
                assert!(l.token >= prev.next_token, "{ctx}: L-FENCE: fresh lock got a non-fresh token");
            }
        }
        // only paths named by lock ops may change lock state
        let named = |z: &str, p: &str| t.lock_ops.iter().any(|o| o.zone() == z && o.path() == p);
        for k in prev.locks.keys().chain(m.locks.keys()) {
            if prev.locks.get(k) != m.locks.get(k) {
                assert!(named(&k.0, &k.1), "{ctx}: lock {k:?} changed without an op naming it");
            }
        }
        for op in &t.lock_ops {
            let key = (op.zone().to_owned(), op.path().to_owned());
            let pl = prev.locks.get(&key);
            let live_other = pl.filter(|l| l.expires_at > now && l.holder != principal);
            match op {
                LockOp::Acquire { .. } | LockOp::Release { .. } | LockOp::Renew { .. } => {
                    assert!(
                        live_other.is_none(),
                        "{ctx}: L-EXPIRY/L-EXCL: {op:?} by {principal} accepted over live lock of another"
                    );
                }
                LockOp::Present { token, .. } => {
                    let l = pl.unwrap_or_else(|| panic!("{ctx}: L-FENCE: accepted publish presents token {token} but no lock existed"));
                    assert!(
                        l.token == *token && l.holder == principal && l.expires_at > now,
                        "{ctx}: L-FENCE: accepted stale/foreign token {token} (current {})",
                        l.token
                    );
                }
                LockOp::Break { .. } => {}
            }
        }
        // L-EXCL: a tx never changes a path live-locked by another principal
        for u in &t.updates {
            for p in changed(b, u.old, u.new) {
                let key = (u.zone.clone(), p.clone());
                if let Some(l) = prev.locks.get(&key).filter(|l| l.expires_at > now) {
                    assert_eq!(
                        l.holder, principal,
                        "{ctx}: L-EXCL: {p} changed under {}'s live lock by {principal}",
                        l.holder
                    );
                    assert!(
                        t.lock_ops
                            .iter()
                            .any(|o| matches!(o, LockOp::Acquire { .. }) && (o.zone(), o.path()) == (u.zone.as_str(), p.as_str()))
                            || t.lock_ops
                                .iter()
                                .any(|o| matches!(o, LockOp::Present { token, .. } if *token == l.token)
                                    && (o.zone(), o.path()) == (u.zone.as_str(), p.as_str())),
                        "{ctx}: L-FENCE: holder published {p} without naming the current token"
                    );
                }
            }
        }
        prev = m.clone();
    }
    for id in prev.branches.values() {
        assert!(b.exists(&obj_key(id)).unwrap(), "{ctx}: head object missing");
    }
    prev
}

struct Stats {
    leaves: u64,
}

fn mk_pub<'a>(b: &'a MemBackend, clock: &'a Cell<u64>, fault: Option<LockFault>, w: &'a Writer) -> Publisher<'a> {
    let mut p = Publisher::new(b, &w.store, w.tx.clone())
        .with_principal(w.who)
        .with_clock(|| clock.get());
    if let Some(f) = fault {
        p = p.with_lock_fault(f);
    }
    p
}

fn run_schedule(sc: &Scenario, fault: Option<LockFault>, sched: &[Choice]) -> Option<Vec<Choice>> {
    let writers = (sc.build)();
    let backend = MemBackend::new();
    let clock = Cell::new(T0);
    // pre-seed every writer's objects, then fault-free setup
    for w in &writers {
        for u in &w.tx.updates {
            seed(&backend, &w.store, u.new);
        }
    }
    (sc.setup)(&backend, &clock, &writers);
    let mut pubs: Vec<Option<Publisher>> = writers.iter().map(|w| Some(mk_pub(&backend, &clock, fault, w))).collect();
    let mut st = vec![W::Running; writers.len()];
    let mut special = false;
    let (mut ticks, mut fwd, mut back) = (0, false, false);
    let step = |w: usize, pubs: &mut Vec<Option<Publisher>>, st: &mut Vec<W>| match pubs[w].as_mut().unwrap().step() {
        Ok(Step::Continue) => {}
        Ok(Step::Done(_)) => st[w] = W::Done(true),
        Err(
            PublishError::Diverged { .. }
            | PublishError::LockedBy { .. }
            | PublishError::LockBroken { .. }
            | PublishError::NotHolder { .. },
        ) => st[w] = W::Done(false),
        Err(e) => panic!("{}: unexpected error {e}", sc.name),
    };
    for c in sched {
        match *c {
            Choice::Step(w) => step(w, &mut pubs, &mut st),
            Choice::Fault(w, f) => {
                special = true;
                backend.inject_cas_fault(f);
                step(w, &mut pubs, &mut st);
            }
            Choice::Crash(w) => {
                special = true;
                st[w] = W::Crashed;
                pubs[w] = None;
            }
            Choice::Tick => {
                ticks += 1;
                clock.set(clock.get() + TICK);
            }
            Choice::Fwd => {
                fwd = true;
                clock.set(clock.get() + 400);
            }
            Choice::Back => {
                back = true;
                clock.set(clock.get().saturating_sub(500));
            }
        }
    }
    let mut avail = Vec::new();
    for w in 0..writers.len() {
        if st[w] == W::Running {
            avail.push(Choice::Step(w));
            if !special {
                if pubs[w].as_ref().unwrap().next_is_cas() {
                    avail.push(Choice::Fault(w, CasFault::FailBefore));
                    avail.push(Choice::Fault(w, CasFault::LostAck));
                }
                avail.push(Choice::Crash(w));
            }
        }
    }
    if !avail.is_empty() {
        if ticks < sc.ticks {
            avail.push(Choice::Tick);
        }
        if sc.jumps && !fwd {
            avail.push(Choice::Fwd);
        }
        if sc.jumps && !back {
            avail.push(Choice::Back);
        }
        return Some(avail);
    }
    let ctx = format!("{} schedule {:?}", sc.name, sched);
    let mut ok: Vec<bool> = st.iter().map(|s| matches!(s, W::Done(true))).collect();
    for w in 0..writers.len() {
        if st[w] == W::Crashed {
            // recovery: retry the same transaction, faults off (clock keeps its current value)
            ok[w] = match mk_pub(&backend, &clock, fault, &writers[w]).run() {
                Ok(_) => true,
                Err(
                    PublishError::Diverged { .. }
                    | PublishError::LockedBy { .. }
                    | PublishError::LockBroken { .. }
                    | PublishError::NotHolder { .. },
                ) => false,
                Err(e) => panic!("{ctx}: recovery failed: {e}"),
            };
        }
    }
    let m = check_backend(&backend, &ctx);
    let txs: Vec<Transaction> = writers.iter().map(|w| w.tx.clone()).collect();
    // L-VISIBLE: reported success iff durably applied
    for (w, t) in txs.iter().enumerate() {
        let id = Object::Transaction(t.clone()).id();
        assert_eq!(
            ok[w],
            m.applied.contains(&id),
            "{ctx}: L-VISIBLE: writer {w} outcome {} disagrees with remote",
            ok[w]
        );
    }
    // L-LIVE: with faults off and time advanced past every lease, any writer can acquire the path
    clock.set(clock.get().max(m.time_hw) + 100_000);
    let probe = MemoryStore::new();
    let acq = tx("probe", vec![], vec![acquire("a.bin", 10)], "probe");
    run_one(&backend, &clock, &probe, "probe", acq, None).unwrap_or_else(|e| panic!("{ctx}: L-LIVE: acquire after expiry failed: {e}"));
    (sc.expect)(&ok, &m, &txs);
    None
}

fn explore(sc: &Scenario, fault: Option<LockFault>, sched: &mut Vec<Choice>, stats: &mut Stats) {
    match run_schedule(sc, fault, sched) {
        None => stats.leaves += 1,
        Some(avail) => {
            for c in avail {
                sched.push(c);
                explore(sc, fault, sched, stats);
                sched.pop();
            }
        }
    }
}

fn go(sc: Scenario, fault: Option<LockFault>) {
    let mut stats = Stats { leaves: 0 };
    explore(&sc, fault, &mut vec![], &mut stats);
    println!("{}: {} complete schedules checked", sc.name, stats.leaves);
    assert!(stats.leaves > sc.min_leaves, "{}: exploration suspiciously small", sc.name);
}

fn setup_with_lock(lease: u64) -> impl Fn(&MemBackend, &Cell<u64>, &[Writer]) {
    move |b, clock, ws| {
        publish_base(b, clock, ws);
        let t = tx("alice", vec![], vec![acquire("a.bin", lease)], "alice locks");
        run_one(b, clock, &ws[0].store, "alice", t, None).unwrap();
    }
}
fn setup_lock_150(b: &MemBackend, c: &Cell<u64>, ws: &[Writer]) {
    setup_with_lock(90)(b, c, ws)
}
fn setup_lock_1000(b: &MemBackend, c: &Cell<u64>, ws: &[Writer]) {
    setup_with_lock(1000)(b, c, ws)
}
fn setup_base(b: &MemBackend, c: &Cell<u64>, ws: &[Writer]) {
    publish_base(b, c, ws);
}

fn lock_only(who: &'static str, ops: Vec<LockOp>) -> Writer {
    Writer {
        store: MemoryStore::new(),
        tx: tx(who, vec![], ops, who),
        who,
    }
}

// ---- scenarios -----------------------------------------------------------------------

fn s_acquire_race() -> Scenario {
    Scenario {
        name: "acquire-race",
        build: || {
            vec![
                lock_only("alice", vec![acquire("a.bin", 1000)]),
                lock_only("bob", vec![acquire("a.bin", 1000)]),
            ]
        },
        setup: |_, _, _| {},
        ticks: 1,
        jumps: false,
        expect: |ok, m, _| {
            assert_eq!(ok.iter().filter(|x| **x).count(), 1, "exactly one acquirer wins");
            let l = &m.locks[&(ZONE.to_string(), "a.bin".to_string())];
            assert_eq!(l.holder, if ok[0] { "alice" } else { "bob" });
            assert_eq!(l.token, 0);
        },
        min_leaves: 100,
    }
}

fn s_holder_vs_contender() -> Scenario {
    Scenario {
        name: "holder-vs-contender-publish",
        build: || {
            let mut ws = writers_on_base(&[("alice", "A"), ("bob", "B")]);
            ws[0].tx.lock_ops = vec![present("a.bin", 0)];
            ws
        },
        setup: setup_lock_1000,
        ticks: 1,
        jumps: false,
        expect: |ok, m, txs| {
            assert_eq!(ok, [true, false], "holder accepted, unlocked contender rejected");
            assert_eq!(m.head(ZONE, "main"), Some(txs[0].updates[0].new));
        },
        min_leaves: 100,
    }
}

fn s_crash_expire_reacquire() -> Scenario {
    Scenario {
        name: "crash-expire-reacquire",
        build: || {
            let mut ws = writers_on_base(&[("alice", "A"), ("bob", "B")]);
            ws[0].tx.lock_ops = vec![present("a.bin", 0)];
            ws[1].tx.lock_ops = vec![acquire("a.bin", 1000)];
            ws
        },
        setup: setup_lock_150,
        ticks: 1,
        jumps: false,
        expect: |ok, m, txs| {
            assert!(!(ok[0] && ok[1]), "both publishes cannot win");
            if ok[1] {
                assert_eq!(m.head(ZONE, "main"), Some(txs[1].updates[0].new));
                assert_eq!(m.locks[&(ZONE.to_string(), "a.bin".to_string())].holder, "bob");
            }
        },
        min_leaves: 100,
    }
}

fn s_break_vs_publish() -> Scenario {
    Scenario {
        name: "break-vs-publish",
        build: || {
            let mut ws = writers_on_base(&[("alice", "A")]);
            ws[0].tx.lock_ops = vec![present("a.bin", 0)];
            ws.push(lock_only(
                "admin",
                vec![LockOp::Break {
                    zone: ZONE.into(),
                    path: "a.bin".into(),
                }],
            ));
            ws
        },
        setup: setup_lock_1000,
        ticks: 1,
        jumps: false,
        expect: |ok, m, _| {
            assert!(ok[1], "break always succeeds");
            assert!(m.locks.is_empty(), "lock is gone");
        },
        min_leaves: 100,
    }
}

fn s_renew_expiry_acquire() -> Scenario {
    Scenario {
        name: "renew-vs-expiry-vs-acquire",
        build: || {
            vec![
                lock_only(
                    "alice",
                    vec![LockOp::Renew {
                        zone: ZONE.into(),
                        path: "a.bin".into(),
                        token: 0,
                        lease_ms: 1000,
                    }],
                ),
                lock_only("bob", vec![acquire("a.bin", 1000)]),
            ]
        },
        setup: setup_lock_150,
        ticks: 1,
        jumps: false,
        expect: |ok, _, _| assert!(!(ok[0] && ok[1]), "renew and a contender's acquire are mutually exclusive"),
        min_leaves: 100,
    }
}

fn s_multi_zone_atomic() -> Scenario {
    Scenario {
        name: "multi-zone-atomic",
        build: || {
            let mut ws = writers_on_base(&[("bob", "B"), ("carol", "C")]);
            let by = mk_commit(&ws[0].store, "y", None, &[("f", "bob")]);
            let cy = mk_commit(&ws[1].store, "y", None, &[("f", "carol")]);
            let base = ws[0].tx.updates[0].clone();
            ws[0].tx = tx("bob", vec![("y", None, by), (ZONE, base.old, base.new)], vec![], "bob");
            ws[1].tx = tx("carol", vec![("y", None, cy)], vec![], "carol");
            ws
        },
        setup: setup_lock_1000,
        ticks: 1,
        jumps: false,
        expect: |ok, m, txs| {
            assert_eq!(
                ok,
                [false, true],
                "bob touches alice's locked file: rejected whole; carol unaffected"
            );
            assert_eq!(m.head("y", "main"), Some(txs[1].updates[0].new), "no partial effect of bob's tx");
        },
        min_leaves: 100,
    }
}

fn s_duplicate_acquire() -> Scenario {
    Scenario {
        name: "duplicate-acquire",
        build: || {
            let w = lock_only("alice", vec![acquire("a.bin", 1000)]);
            let w2 = Writer {
                store: MemoryStore::new(),
                tx: w.tx.clone(),
                who: "alice",
            };
            vec![w, w2]
        },
        setup: setup_base,
        ticks: 1,
        jumps: false,
        expect: |ok, m, _| {
            assert_eq!(ok, [true, true]);
            assert_eq!(m.next_token, 1, "applied once, one token");
            assert_eq!(m.applied.len(), 2, "base + one acquire");
        },
        min_leaves: 100,
    }
}

fn s_clock_jumps() -> Scenario {
    Scenario {
        name: "clock-jumps",
        build: || {
            let mut ws = writers_on_base(&[("alice", "A")]);
            ws[0].tx.lock_ops = vec![present("a.bin", 0)];
            ws.push(lock_only("bob", vec![acquire("b.txt", 1000)]));
            ws
        },
        setup: setup_lock_150,
        ticks: 0,
        jumps: true,
        expect: |_, _, _| {},
        min_leaves: 100,
    }
}

macro_rules! scenarios {
    ($($t:ident => $s:ident),* $(,)?) => { $( #[test] fn $t() { go($s(), None); } )* };
}
scenarios! {
    acquire_race_exactly_one_wins => s_acquire_race,
    holder_publish_beats_contender => s_holder_vs_contender,
    crash_expiry_reacquire_fences_old_holder => s_crash_expire_reacquire,
    break_vs_publish => s_break_vs_publish,
    renew_vs_expiry_vs_acquire => s_renew_expiry_acquire,
    multi_zone_rejection_is_atomic => s_multi_zone_atomic,
    duplicate_acquire_applies_once => s_duplicate_acquire,
    clock_jumps_do_not_break_invariants => s_clock_jumps,
}

// ---- mutation checks: each must make the model fail ------------------------------------

#[test]
#[should_panic(expected = "L-FENCE")]
fn mutation_skip_token_comparison_is_caught() {
    go(s_break_vs_publish(), Some(LockFault::SkipTokenCheck));
}

#[test]
#[should_panic(expected = "L-EXPIRY/L-EXCL")]
fn mutation_release_by_non_holder_is_caught() {
    let mut sc = s_holder_vs_contender();
    sc.name = "mutation-release-anyone";
    sc.build = || {
        let mut ws = writers_on_base(&[("alice", "A")]);
        ws.push(lock_only(
            "bob",
            vec![LockOp::Release {
                zone: ZONE.into(),
                path: "a.bin".into(),
                token: 0,
            }],
        ));
        ws
    };
    sc.expect = |_, _, _| {};
    go(sc, Some(LockFault::ReleaseAnyone));
}

#[test]
#[should_panic(expected = "time_hw decreased")]
fn mutation_clock_backward_without_high_water_is_caught() {
    go(s_clock_jumps(), Some(LockFault::NoHighWater));
}

#[test]
#[should_panic(expected = "L-FENCE")]
fn mutation_token_reuse_is_caught() {
    go(s_crash_expire_reacquire(), Some(LockFault::ReuseTokens));
}
