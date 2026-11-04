//! Executable model of a CANDIDATE protocol change: per-zone sharded manifests with a
//! two-phase cross-zone commit (issue #186, experiment K). Nothing here is production code:
//! the current protocol is a single manifest CAS (`protocol.rs`); this file only checks that the
//! sharded alternative described in docs/research/storage/concurrency-and-correctness.md can be
//! made atomic, and that every safety rule in it is necessary.
//!
//! Protocol (all keys on an S3-like store with create-only PUT and CAS):
//! * `shard/<zone>`   = (version, head, pending: Option<(txid, new head)>), replaced by CAS.
//! * `decision/<tx>`  = Committed | Aborted, create-only. The create-only write that makes it
//!   `Committed` is the single linearization point of the transaction. Whoever creates it first
//!   decides; it can never change afterwards.
//! * Writer: Prepare every zone in ascending order (CAS `pending=(tx,new)` iff `head==old` and no
//!   pending), then Decide (create `decision/<tx>`=Committed; if it already says Aborted, clean
//!   up), then Finalize every shard (`head=new, pending=None`). Any actor that finds a foreign
//!   pending resolves it from the decision record: Committed -> finalize it, Aborted -> clear it,
//!   absent -> create `Aborted` (this is the recovery of a crashed writer), then clear it.
//! * Reader: double collect. Read all shards (effective head = pending.new iff the pending
//!   transaction is Committed, else head); repeat until two consecutive collects have identical
//!   (version, effective head) vectors. That is a linearizable multi-zone snapshot.
//!
//! Explored (memoised on the full state, so counts are DISTINCT STATES, not schedules): every interleaving of 2 writers and one reader, plus at most one fault per
//! schedule: crash of a writer at any step (recovered at the end by re-running the same
//! transaction, which also resolves its leftovers), or a lost acknowledgement at any step (the
//! effect is applied but the writer repeats the step). Invariants (checked after EVERY step):
//!   S-ATOMIC  the effective state (pending with a Committed decision counts as applied) equals
//!             the replay of the committed transactions in decision order, and every replayed
//!             transaction saw `head == old` on every zone (no lost update, all-or-nothing).
//!   S-SNAP    every reader result equals the replay of some prefix of the commit order, no
//!             shorter than the prefix committed when the reader started (no torn reads, no
//!             stale reads after a commit was decided).
//!   S-OUTCOME a writer reports success iff its decision is Committed.
//!   S-QUIESCE after recovery no pending record remains.
//! Mutation tests (`#[should_panic]`) delete one safety rule each and require the checker to fail.

use std::collections::BTreeMap;

const Z: usize = 2;

#[derive(Clone, Copy, Debug)]
struct Flags {
    /// reader repeats until two consecutive collects agree
    validate_reads: bool,
    /// the writer decides only after all its zones are prepared
    decide_after_all_prepared: bool,
    /// a prepare never overwrites a foreign pending record
    respect_foreign_pending: bool,
    /// reader derives the effective head from the decision record
    reader_uses_decision: bool,
    /// a helper that finds an undecided foreign pending aborts it (else it applies it)
    undecided_means_abort: bool,
    /// lower txid is never aborted by a higher one (it waits instead): avoids mutual aborts
    wound_wait: bool,
}
const GOOD: Flags = Flags {
    validate_reads: true,
    decide_after_all_prepared: true,
    respect_foreign_pending: true,
    reader_uses_decision: true,
    undecided_means_abort: true,
    wound_wait: true,
};

#[derive(Clone, Copy, PartialEq, Debug)]
struct Shard {
    ver: u32,
    head: u8,
    pend: Option<(usize, u8)>,
}
#[derive(Clone, Copy, PartialEq, Debug)]
enum Dec {
    Committed,
    Aborted,
}
#[derive(Clone, Debug)]
struct Store {
    shards: [Shard; Z],
    dec: BTreeMap<usize, Dec>,
    order: Vec<usize>,
}
#[derive(Clone, Debug)]
struct Spec {
    id: usize,
    ups: Vec<(usize, u8, u8)>, // zone, old, new; ascending zone
}
#[derive(Clone, Copy, PartialEq, Debug)]
enum Ph {
    Start,
    Prep(usize),
    Decide,
    Fin(usize),
    Clean(usize),
    Done(bool),
}
#[derive(Clone, Copy, PartialEq, Debug)]
enum Run {
    Live,
    Crashed,
}
#[derive(Clone, Debug)]
struct Writer {
    spec: Spec,
    ph: Ph,
    run: Run,
}
#[derive(Clone, Debug, Default)]
struct Reader {
    k_start: Option<usize>,
    stage: u8,
    idx: usize,
    prev: Vec<(u32, u8)>,
    cur: Vec<(u32, u8)>,
    result: Option<[u8; Z]>,
}
#[derive(Clone, Debug)]
struct State {
    store: Store,
    w: Vec<Writer>,
    r: Option<Reader>,
    faulted: bool,
}

fn effective(store: &Store, z: usize, f: &Flags) -> u8 {
    let s = store.shards[z];
    match s.pend {
        Some((t, n)) if f.reader_uses_decision && store.dec.get(&t) == Some(&Dec::Committed) => n,
        _ => s.head,
    }
}

fn replay(store: &Store, specs: &[Spec], upto: usize) -> [u8; Z] {
    let mut h = [0u8; Z];
    for t in &store.order[..upto] {
        let sp = specs.iter().find(|s| s.id == *t).unwrap();
        for (z, old, new) in &sp.ups {
            assert_eq!(
                h[*z], *old,
                "S-ATOMIC: tx {t} committed with a stale precondition on zone {z} (lost update)"
            );
            h[*z] = *new;
        }
    }
    h
}

/// Apply a pending record that belongs to a decided transaction, or clear it.
fn resolve(store: &mut Store, z: usize, _t: usize, n: u8, commit: bool) {
    let s = &mut store.shards[z];
    if commit {
        s.head = n;
    }
    s.pend = None;
    s.ver += 1;
}

/// One protocol step of writer `i`. `None` = blocked (waiting for a higher-priority tx).
/// `recovery` disables waiting (the blocker's lease is considered expired).
fn writer_step(st: &mut State, i: usize, f: &Flags, recovery: bool) -> Option<()> {
    let Writer { spec, ph, .. } = st.w[i].clone();
    let me = spec.id;
    let next_prep = |k: usize| {
        if k >= spec.ups.len() || (!f.decide_after_all_prepared && k >= 1) {
            Ph::Decide
        } else {
            Ph::Prep(k)
        }
    };
    let new_ph = match ph {
        Ph::Done(_) => return Some(()),
        Ph::Start => match st.store.dec.get(&me) {
            Some(Dec::Committed) => Ph::Fin(0),
            Some(Dec::Aborted) => Ph::Clean(0),
            None => Ph::Prep(0),
        },
        Ph::Prep(k) => {
            let (z, old, new) = spec.ups[k];
            let sh = st.store.shards[z];
            match sh.pend {
                Some((t, _)) if t == me => next_prep(k + 1),
                Some((t, n)) if f.respect_foreign_pending => {
                    match st.store.dec.get(&t).copied() {
                        Some(Dec::Committed) => resolve(&mut st.store, z, t, n, true),
                        Some(Dec::Aborted) => resolve(&mut st.store, z, t, n, false),
                        None if f.undecided_means_abort => {
                            if f.wound_wait && t < me && !recovery {
                                return None;
                            }
                            st.store.dec.insert(t, Dec::Aborted);
                        }
                        None => resolve(&mut st.store, z, t, n, true), // MUTATION: apply undecided
                    }
                    Ph::Prep(k)
                }
                _ => {
                    if sh.head != old {
                        st.store.dec.entry(me).or_insert(Dec::Aborted);
                        Ph::Clean(0)
                    } else {
                        let s = &mut st.store.shards[z];
                        s.pend = Some((me, new));
                        s.ver += 1;
                        next_prep(k + 1)
                    }
                }
            }
        }
        Ph::Decide => match st.store.dec.get(&me) {
            None => {
                st.store.dec.insert(me, Dec::Committed);
                st.store.order.push(me);
                Ph::Fin(0)
            }
            Some(Dec::Committed) => Ph::Fin(0),
            Some(Dec::Aborted) => Ph::Clean(0),
        },
        Ph::Fin(k) => {
            if k >= spec.ups.len() {
                Ph::Done(true)
            } else {
                let (z, _, new) = spec.ups[k];
                if st.store.shards[z].pend == Some((me, new)) {
                    resolve(&mut st.store, z, me, new, true);
                }
                Ph::Fin(k + 1)
            }
        }
        Ph::Clean(k) => {
            if k >= spec.ups.len() {
                Ph::Done(false)
            } else {
                let (z, _, new) = spec.ups[k];
                if st.store.shards[z].pend == Some((me, new)) {
                    resolve(&mut st.store, z, me, new, false);
                }
                Ph::Clean(k + 1)
            }
        }
    };
    st.w[i].ph = new_ph;
    Some(())
}

fn reader_step(st: &mut State, f: &Flags) {
    let r = st.r.as_mut().unwrap();
    if r.k_start.is_none() {
        r.k_start = Some(st.store.order.len());
    }
    let z = r.idx;
    let s = st.store.shards[z];
    // effective() needs an immutable store; recompute inline
    let eff = match s.pend {
        Some((t, n)) if f.reader_uses_decision && st.store.dec.get(&t) == Some(&Dec::Committed) => n,
        _ => s.head,
    };
    r.cur.push((s.ver, eff));
    r.idx += 1;
    if r.idx == Z {
        r.idx = 0;
        let cur = std::mem::take(&mut r.cur);
        let done = if !f.validate_reads {
            true
        } else if r.stage == 0 {
            r.stage = 1;
            r.prev = cur.clone();
            false
        } else if r.prev == cur {
            true
        } else {
            r.prev = cur.clone();
            false
        };
        if done {
            let mut res = [0u8; Z];
            for (i, (_, e)) in cur.iter().enumerate() {
                res[i] = *e;
            }
            r.result = Some(res);
        }
    }
}

fn check(st: &State, specs: &[Spec], f: &Flags, ctx: &str) {
    let want = replay(&st.store, specs, st.store.order.len());
    let got: Vec<u8> = (0..Z)
        .map(|z| {
            effective(
                &st.store,
                z,
                &Flags {
                    reader_uses_decision: true,
                    ..*f
                },
            )
        })
        .collect();
    assert_eq!(
        got,
        want.to_vec(),
        "S-ATOMIC: effective state differs from replay of committed txs ({ctx})"
    );
    if let Some(r) = &st.r {
        if let Some(res) = r.result {
            let k0 = r.k_start.unwrap();
            let ok = (k0..=st.store.order.len()).any(|k| replay(&st.store, specs, k) == res);
            assert!(
                ok,
                "S-SNAP: reader saw {res:?}, not a committed prefix >= {k0} of {:?} ({ctx})",
                st.store.order
            );
        }
    }
}

#[derive(Default)]
struct Stats {
    seen: std::collections::HashSet<String>,
    nodes: u64,
    leaves: u64,
    fault_free: u64,
    fault_free_no_commit: u64,
}

fn finish(mut st: State, specs: &[Spec], f: &Flags, stats: &mut Stats, ctx: &str) {
    // recovery: crashed writers re-run the same tx solo; live-but-blocked writers likewise
    for i in 0..st.w.len() {
        st.w[i].run = Run::Live;
        let mut guard = 0;
        while !matches!(st.w[i].ph, Ph::Done(_)) {
            if matches!(st.w[i].ph, Ph::Prep(_) | Ph::Decide | Ph::Fin(_) | Ph::Clean(_)) && st.w[i].ph != Ph::Start {
                // a crashed writer restarts from scratch; a live one continues
            }
            writer_step(&mut st, i, f, true).expect("recovery never blocks");
            check(&st, specs, f, ctx);
            guard += 1;
            assert!(guard < 100, "recovery did not terminate ({ctx})");
        }
    }
    for (i, w) in st.w.iter().enumerate() {
        let Ph::Done(ok) = w.ph else { unreachable!() };
        assert_eq!(
            ok,
            st.store.dec.get(&w.spec.id) == Some(&Dec::Committed),
            "S-OUTCOME: writer {i} ({ctx})"
        );
    }
    for z in 0..Z {
        assert!(st.store.shards[z].pend.is_none(), "S-QUIESCE: pending left on zone {z} ({ctx})");
    }
    let want = replay(&st.store, specs, st.store.order.len());
    let got: Vec<u8> = st.store.shards.iter().map(|s| s.head).collect();
    assert_eq!(got, want.to_vec(), "final heads != replay ({ctx})");
    stats.leaves += 1;
    if !st.faulted {
        stats.fault_free += 1;
        if st.store.order.is_empty() {
            stats.fault_free_no_commit += 1;
        }
    }
}

fn explore(st: &State, specs: &[Spec], f: &Flags, stats: &mut Stats, trace: &mut Vec<String>) {
    // the future of a state does not depend on how it was reached: memoise
    if !stats.seen.insert(format!("{st:?}")) {
        return;
    }
    stats.nodes += 1;
    let mut any = false;
    for i in 0..st.w.len() {
        if st.w[i].run != Run::Live || matches!(st.w[i].ph, Ph::Done(_)) {
            continue;
        }
        // plain step
        let mut n = st.clone();
        if writer_step(&mut n, i, f, false).is_some() {
            any = true;
            trace.push(format!("W{i}:{:?}", st.w[i].ph));
            check(&n, specs, f, &trace.join(","));
            explore(&n, specs, f, stats, trace);
            trace.pop();
            if !st.faulted {
                // lost acknowledgement: effect applied, writer does not advance
                let mut l = n.clone();
                l.w[i].ph = st.w[i].ph;
                l.faulted = true;
                trace.push(format!("W{i}:{:?}!lost", st.w[i].ph));
                check(&l, specs, f, &trace.join(","));
                explore(&l, specs, f, stats, trace);
                trace.pop();
            }
        }
        if !st.faulted {
            // crash: the writer disappears; its in-flight state stays on the store
            let mut c = st.clone();
            c.w[i].run = Run::Crashed;
            c.w[i].ph = Ph::Start; // re-run from scratch at recovery
            c.faulted = true;
            any = true;
            trace.push(format!("W{i}:crash@{:?}", st.w[i].ph));
            explore(&c, specs, f, stats, trace);
            trace.pop();
        }
    }
    if let Some(r) = &st.r {
        if r.result.is_none() {
            let mut n = st.clone();
            reader_step(&mut n, f);
            any = true;
            trace.push("R".into());
            check(&n, specs, f, &trace.join(","));
            explore(&n, specs, f, stats, trace);
            trace.pop();
        }
    }
    if !any {
        finish(st.clone(), specs, f, stats, &trace.join(","));
    }
}

fn run(specs: Vec<Spec>, with_reader: bool, f: Flags) -> Stats {
    let st = State {
        store: Store {
            shards: [Shard {
                ver: 0,
                head: 0,
                pend: None,
            }; Z],
            dec: BTreeMap::new(),
            order: vec![],
        },
        w: specs
            .iter()
            .map(|s| Writer {
                spec: s.clone(),
                ph: Ph::Start,
                run: Run::Live,
            })
            .collect(),
        r: with_reader.then(Reader::default),
        faulted: false,
    };
    let mut stats = Stats::default();
    explore(&st, &specs, &f, &mut stats, &mut vec![]);
    stats
}

fn multi(id: usize, new: u8) -> Spec {
    Spec {
        id,
        ups: vec![(0, 0, new), (1, 0, new)],
    }
}
fn single(id: usize, z: usize, new: u8) -> Spec {
    Spec {
        id,
        ups: vec![(z, 0, new)],
    }
}

fn scenario_conflict(f: Flags) -> Stats {
    run(vec![multi(0, 1), multi(1, 2)], true, f)
}
fn scenario_multi_vs_single(f: Flags) -> Stats {
    run(vec![multi(0, 1), single(1, 1, 2)], true, f)
}
fn scenario_disjoint(f: Flags) -> Stats {
    run(vec![single(0, 0, 1), single(1, 1, 1)], true, f)
}
fn scenario_three(f: Flags) -> Stats {
    run(vec![multi(0, 1), single(1, 0, 2), single(2, 1, 3)], false, f)
}

#[test]
fn conflicting_multi_zone_txs_are_atomic_and_one_wins() {
    let s = scenario_conflict(GOOD);
    println!(
        "conflict: {} distinct states, {} terminal states, {} fault-free terminals",
        s.nodes, s.leaves, s.fault_free
    );
    assert!(s.nodes > 1000);
    assert_eq!(
        s.fault_free_no_commit, 0,
        "with wound-wait some tx always commits when no fault is injected"
    );
}

#[test]
fn multi_zone_vs_single_zone() {
    let s = scenario_multi_vs_single(GOOD);
    println!("multi-vs-single: {} distinct states, {} terminal", s.nodes, s.leaves);
    assert!(s.nodes > 1000);
}

#[test]
fn disjoint_single_zone_txs_both_commit() {
    let s = scenario_disjoint(GOOD);
    println!("disjoint: {} distinct states, {} terminal", s.nodes, s.leaves);
    assert!(s.nodes > 500);
}

#[test]
fn three_writers() {
    let s = scenario_three(GOOD);
    println!("three: {} distinct states, {} terminal", s.nodes, s.leaves);
    assert!(s.nodes > 1000);
}

/// Negative result recorded by the model: without a priority rule the protocol is safe but two
/// conflicting transactions can abort each other (no progress). The test asserts that this
/// happens, so the claim in the doc is executable.
#[test]
fn without_wound_wait_mutual_abort_is_possible_but_safe() {
    let s = scenario_conflict(Flags { wound_wait: false, ..GOOD });
    println!(
        "no wound-wait: {} fault-free terminal states, {} with zero commits",
        s.fault_free, s.fault_free_no_commit
    );
    assert!(s.fault_free_no_commit > 0, "expected a livelock-prone schedule where both abort");
}

// ---- mutation checks: each removes one safety rule and the checker must fail ----

#[test]
#[should_panic(expected = "S-SNAP")]
fn mutation_single_collect_reader_sees_torn_state() {
    scenario_conflict(Flags {
        validate_reads: false,
        ..GOOD
    });
}

#[test]
#[should_panic(expected = "S-ATOMIC")]
fn mutation_decide_before_all_zones_prepared() {
    scenario_conflict(Flags {
        decide_after_all_prepared: false,
        ..GOOD
    });
}

#[test]
#[should_panic(expected = "S-ATOMIC")]
fn mutation_prepare_overwrites_foreign_pending() {
    scenario_conflict(Flags {
        respect_foreign_pending: false,
        ..GOOD
    });
}

#[test]
#[should_panic(expected = "S-ATOMIC")]
fn mutation_helper_applies_undecided_pending() {
    scenario_conflict(Flags {
        undecided_means_abort: false,
        ..GOOD
    });
}

#[test]
#[should_panic(expected = "S-SNAP")]
fn mutation_reader_ignores_decision_record_gives_stale_read() {
    scenario_multi_vs_single(Flags {
        reader_uses_decision: false,
        ..GOOD
    });
}
