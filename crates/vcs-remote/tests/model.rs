//! Executable model of the publish protocol.
//!
//! Explores EVERY interleaving of the writers' protocol steps, and in every schedule
//! additionally either (a) crashes one writer at any point, or (b) loses a CAS request or
//! its response at any CAS. After the schedule, crashed/unfinished writers recover by
//! re-running the same transaction. Invariants are then checked on the final remote state.

use std::collections::BTreeSet;
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

/// One local commit (blob+tree+commit) in `zone`, stored in `store`.
fn mk_commit(store: &MemoryStore, zone: &str, parent: Option<ObjectId>, content: &str) -> ObjectId {
    // content "" => empty tree (fewer protocol steps => smaller state space)
    let entries = if content.is_empty() {
        vec![]
    } else {
        vec![TreeEntry {
            name: "f".into(),
            mode: EntryMode::File,
            id: store.put_blob(content.as_bytes()).unwrap(),
        }]
    };
    let t = store.put(&Object::Tree(Tree::new(entries).unwrap())).unwrap();
    store
        .put(&Object::Commit(Commit {
            zone: zone.into(),
            parents: parent.into_iter().collect(),
            root: t,
            change_id: ChangeId::from_bytes([content.len() as u8; 16]),
            author: sig(),
            message: content.into(),
        }))
        .unwrap()
}

fn tx(updates: Vec<(&str, Option<ObjectId>, ObjectId)>, msg: &str) -> Transaction {
    let mut u: Vec<RefUpdate> = updates
        .into_iter()
        .map(|(a, old, new)| RefUpdate {
            zone: a.into(),
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
        lock_ops: vec![],
    }
}

struct Scenario {
    name: &'static str,
    /// (local store, transaction) per writer
    build: fn() -> Vec<(MemoryStore, Transaction)>,
    /// check outcomes (true = writer ended Applied/AlreadyApplied) and final manifest
    expect: fn(&[bool], &Manifest, &[Transaction]),
}

#[derive(Clone, Copy, Debug)]
enum Choice {
    Step(usize),
    Fault(usize, CasFault),
    Crash(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum W {
    Running,
    Done(bool),
    Crashed,
}

fn deps_of(o: &Object) -> Vec<ObjectId> {
    match o {
        Object::Tree(t) => t.entries().iter().map(|e| e.id).collect(),
        Object::Commit(c) => c.parents.iter().copied().chain([c.root]).collect(),
        Object::Snapshot(s) => s.zones.values().copied().collect(),
        _ => vec![],
    }
}

fn obj_key(id: &ObjectId) -> String {
    let h = id.to_hex();
    format!("objects/{}/{}", &h[..2], &h[2..])
}

fn check_backend(b: &MemBackend, ctx: &str) -> Manifest {
    // closure invariant: every stored object is intact and all its dependencies are stored
    for k in b.keys().into_iter().filter(|k| k.starts_with("objects/")) {
        let data = b.get(&k).unwrap().unwrap().data;
        let o = Object::decode(&data).unwrap_or_else(|e| panic!("{ctx}: undecodable {k}: {e}"));
        assert_eq!(obj_key(&id_of_encoded(&data)), k, "{ctx}: hash mismatch");
        for d in deps_of(&o) {
            assert!(
                b.exists(&obj_key(&d)).unwrap(),
                "{ctx}: object {k} present but dependency {d} missing (closure invariant broken)"
            );
        }
    }
    // manifest history: each version = previous + exactly one whole transaction
    let hist: Vec<Manifest> = b
        .history()
        .into_iter()
        .filter(|(k, _)| k == "manifest")
        .map(|(_, d)| Manifest::decode(&d).unwrap())
        .collect();
    let mut prev = Manifest::default();
    for (i, m) in hist.iter().enumerate() {
        assert_eq!(m.generation, prev.generation + 1, "{ctx}: generation gap at {i}");
        assert_eq!(
            &m.applied[..m.applied.len() - 1],
            &prev.applied[..],
            "{ctx}: applied history rewritten"
        );
        let txid = *m.applied.last().unwrap();
        assert!(!prev.applied.contains(&txid), "{ctx}: tx {txid} applied twice");
        let Object::Transaction(t) = Object::decode(
            &b.get(&format!("tx/{}", txid.to_hex()))
                .unwrap()
                .expect("tx record exists before CAS")
                .data,
        )
        .unwrap() else {
            panic!()
        };
        let mut expect = prev.clone();
        expect.generation += 1;
        expect.applied = m.applied.clone();
        for u in &t.updates {
            assert_eq!(
                prev.head(&u.zone, &u.branch),
                u.old,
                "{ctx}: tx applied with stale precondition (lost update)"
            );
            expect.branches.insert((u.zone.clone(), u.branch.clone()), u.new);
        }
        assert_eq!(&expect, m, "{ctx}: manifest step is not exactly one whole transaction (atomicity)");
        prev = m.clone();
    }
    // every head's closure is present
    for id in prev.branches.values() {
        assert!(b.exists(&obj_key(id)).unwrap(), "{ctx}: head object missing");
    }
    prev
}

struct Stats {
    leaves: u64,
}

fn run_schedule(sc: &Scenario, sched: &[Choice]) -> Option<Vec<Choice>> {
    let writers = (sc.build)();
    let backend = MemBackend::new();
    let mut pubs: Vec<Option<Publisher>> = writers.iter().map(|(s, t)| Some(Publisher::new(&backend, s, t.clone()))).collect();
    let mut st = vec![W::Running; writers.len()];
    let mut special = false;
    let step = |w: usize, pubs: &mut Vec<Option<Publisher>>, st: &mut Vec<W>| match pubs[w].as_mut().unwrap().step() {
        Ok(Step::Continue) => {}
        Ok(Step::Done(_)) => st[w] = W::Done(true),
        Err(PublishError::Diverged { .. }) => st[w] = W::Done(false),
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
        return Some(avail);
    }
    // terminal: recovery by retrying the same transaction (fault-free)
    let ctx = format!("{} schedule {:?}", sc.name, sched);
    let mut ok: Vec<bool> = st.iter().map(|s| matches!(s, W::Done(true))).collect();
    for w in 0..writers.len() {
        if st[w] == W::Crashed {
            ok[w] = match Publisher::new(&backend, &writers[w].0, writers[w].1.clone()).run() {
                Ok(_) => true,
                Err(PublishError::Diverged { .. }) => false,
                Err(e) => panic!("{ctx}: recovery failed: {e}"),
            };
        }
    }
    let m = check_backend(&backend, &ctx);
    let txs: Vec<Transaction> = writers.iter().map(|(_, t)| t.clone()).collect();
    // a writer reporting success must be durably visible
    for (w, t) in txs.iter().enumerate() {
        let id = Object::Transaction(t.clone()).id();
        assert_eq!(
            ok[w],
            m.applied.contains(&id),
            "{ctx}: writer {w} outcome {} disagrees with remote state",
            ok[w]
        );
    }
    (sc.expect)(&ok, &m, &txs);
    None
}

fn explore(sc: &Scenario, sched: &mut Vec<Choice>, stats: &mut Stats) {
    match run_schedule(sc, sched) {
        None => stats.leaves += 1,
        Some(avail) => {
            for c in avail {
                sched.push(c);
                explore(sc, sched, stats);
                sched.pop();
            }
        }
    }
}

fn go(sc: Scenario) {
    let mut stats = Stats { leaves: 0 };
    explore(&sc, &mut vec![], &mut stats);
    println!("{}: {} complete schedules checked", sc.name, stats.leaves);
    assert!(stats.leaves > 1000, "exploration suspiciously small");
}

#[test]
fn same_branch_race_exactly_one_wins() {
    go(Scenario {
        name: "same-branch",
        build: || {
            let (s1, s2) = (MemoryStore::new(), MemoryStore::new());
            let x = mk_commit(&s1, "a", None, "x");
            let y = mk_commit(&s2, "a", None, "yy");
            vec![(s1, tx(vec![("a", None, x)], "w1")), (s2, tx(vec![("a", None, y)], "w2"))]
        },
        expect: |ok, m, txs| {
            assert_eq!(ok.iter().filter(|x| **x).count(), 1, "exactly one writer wins");
            let winner = if ok[0] { &txs[0] } else { &txs[1] };
            assert_eq!(m.head("a", "main"), Some(winner.updates[0].new));
        },
    });
}

#[test]
fn disjoint_zones_both_succeed() {
    go(Scenario {
        name: "disjoint-zones",
        build: || {
            let (s1, s2) = (MemoryStore::new(), MemoryStore::new());
            let x = mk_commit(&s1, "a", None, "x");
            let y = mk_commit(&s2, "b", None, "yy");
            vec![(s1, tx(vec![("a", None, x)], "w1")), (s2, tx(vec![("b", None, y)], "w2"))]
        },
        expect: |ok, m, txs| {
            assert_eq!(
                ok,
                [true, true],
                "writers on different zones must both commit (reconcile after CAS loss)"
            );
            assert_eq!(m.head("a", "main"), Some(txs[0].updates[0].new));
            assert_eq!(m.head("b", "main"), Some(txs[1].updates[0].new));
        },
    });
}

#[test]
fn multi_zone_is_atomic_against_single_zone() {
    go(Scenario {
        name: "multi-vs-single",
        build: || {
            let (s1, s2) = (MemoryStore::new(), MemoryStore::new());
            let a1 = mk_commit(&s1, "a", None, "");
            let b1 = mk_commit(&s1, "b", None, "");
            let a2 = mk_commit(&s2, "a", None, "y");
            vec![
                (s1, tx(vec![("a", None, a1), ("b", None, b1)], "multi")),
                (s2, tx(vec![("a", None, a2)], "single")),
            ]
        },
        expect: |ok, m, txs| {
            assert_eq!(ok.iter().filter(|x| **x).count(), 1);
            if ok[0] {
                assert_eq!(m.head("a", "main"), Some(txs[0].updates[0].new));
                assert_eq!(m.head("b", "main"), Some(txs[0].updates[1].new));
            } else {
                assert_eq!(m.head("a", "main"), Some(txs[1].updates[0].new));
                assert_eq!(m.head("b", "main"), None, "loser's second zone must not leak");
            }
        },
    });
}

#[test]
fn duplicate_submission_applies_once() {
    go(Scenario {
        name: "duplicate-tx",
        build: || {
            let (s1, s2) = (MemoryStore::new(), MemoryStore::new());
            let x = mk_commit(&s1, "a", None, "x");
            let x2 = mk_commit(&s2, "a", None, "x");
            assert_eq!(x, x2);
            let t = tx(vec![("a", None, x)], "same");
            vec![(s1, t.clone()), (s2, t)]
        },
        expect: |ok, m, _| {
            assert_eq!(ok, [true, true], "both submissions of one tx report success");
            assert_eq!(m.applied.len(), 1, "applied exactly once");
            assert_eq!(m.generation, 1);
        },
    });
}

#[test]
fn shared_objects_across_zones_and_unique_set_sanity() {
    // a tiny sanity check on the harness itself: distinct scenarios produce distinct txids
    let s = MemoryStore::new();
    let x = mk_commit(&s, "a", None, "x");
    let y = mk_commit(&s, "a", None, "yy");
    let ids: BTreeSet<_> = [tx(vec![("a", None, x)], "m"), tx(vec![("a", None, y)], "m")]
        .iter()
        .map(|t| Object::Transaction(t.clone()).id())
        .collect();
    assert_eq!(ids.len(), 2);
}
