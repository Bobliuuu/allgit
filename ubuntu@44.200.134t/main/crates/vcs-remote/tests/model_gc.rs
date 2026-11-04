//! Executable model of a CANDIDATE remote garbage-collection protocol (issue #186, experiment L)
//! against a concurrent publisher. Nothing here is production code: no destructive remote GC
//! exists (docs/gc.md). The model checks the protocol described in
//! docs/research/storage/concurrency-and-correctness.md, "GC protocol G1", and that every rule in
//! it is necessary (mutation tests below).
//!
//! World: objects C -> D (an old orphan closure left by an abandoned publish), H0 -> Y (the current
//! head's closure, old), and P -> C (a new commit the publisher is about to make the head, which
//! re-adopts the old orphan C by dedup: it uploads only P). Time is a counter that may tick at any
//! point (bounded). `touch[o]` is the backend's last-modified time of o.
//!
//! Publisher (variant A): Collect (C present, so only P is uploaded), PutP, Touch(C) (server-side
//! copy-to-self; fails if C is gone), CheckTomb(C), CheckExists(C), Cas(head=P) fenced to at most H
//! ticks after the first write. Variant B adopts the old orphan C itself as the new head (nothing is
//! uploaded, so no young object protects it and the touch is the only protection). Every test runs both.
//! Sweeper: Candidates (read head; old = now-touch > G and not reachable from head), Tomb(o) for each
//! candidate (create-only `tomb/o`), Remark (read head again, keep = closure(head + every object that
//! is young), delete list = tombed minus keep), Delete(o) then Untomb(o) for each.
//! Invariant G-REACH (after every step): every object in closure(head) exists.
//! The Dekker-style handshake is the heart of it: the sweeper writes the tomb and THEN reads touch
//! times; the publisher writes the touch and THEN reads the tomb (and then re-checks existence
//! because the sweeper removes the tomb after deleting).

use std::collections::{BTreeMap, BTreeSet, HashSet};

const C: usize = 0;
const D: usize = 1;
const H0: usize = 2;
const Y: usize = 3;
const P: usize = 4;
const N: usize = 5;
const DEPS: [&[usize]; N] = [&[D], &[], &[Y], &[], &[C]];
const OLD: i32 = -1000;
const G: i32 = 2; // grace
const H: i32 = 1; // max ticks between publisher touch and its CAS (publisher self-fence)
const MAX_TICKS: i32 = 4;

#[derive(Clone, Copy, Debug)]
struct Flags {
    writer_touches: bool,
    writer_checks_tomb: bool,
    writer_rechecks_exists: bool,
    writer_cas_fence: bool,
    sweeper_tombs_before_remark: bool,
    young_objects_are_roots: bool,
    remark_reads_head: bool,
    candidates_exclude_reachable: bool,
}
const GOOD: Flags = Flags {
    writer_touches: true,
    writer_checks_tomb: true,
    writer_rechecks_exists: true,
    writer_cas_fence: true,
    sweeper_tombs_before_remark: true,
    young_objects_are_roots: true,
    remark_reads_head: true,
    candidates_exclude_reachable: true,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct S {
    now: i32,
    exists: BTreeSet<usize>,
    touch: BTreeMap<usize, i32>,
    tomb: BTreeSet<usize>,
    head: usize,
    // publisher
    w: u8,            // 0 collect,1 put P (variant A only),2 touch C,3 chk tomb,4 chk exists,5 cas,6 done
    adopt_only: bool, // variant B: the new head IS the old orphan C, nothing is uploaded
    w_start: i32,
    // sweeper
    s: u8, // 0 candidates,1 tomb*,2 remark,3 delete/untomb*,4 done
    cands: Vec<usize>,
    del: Vec<usize>,
    pos: usize,
    del_done: bool, // within phase 3: current object deleted, untomb pending
}

fn closure(exists: &BTreeSet<usize>, roots: &[usize]) -> BTreeSet<usize> {
    let mut seen = BTreeSet::new();
    let mut st: Vec<usize> = roots.to_vec();
    while let Some(o) = st.pop() {
        if seen.insert(o) {
            for d in DEPS[o] {
                st.push(*d);
            }
        }
    }
    let _ = exists;
    seen
}

fn check(s: &S, ctx: &str) {
    for o in closure(&s.exists, &[s.head]) {
        assert!(
            s.exists.contains(&o),
            "G-REACH: head {} needs object {o} which was deleted ({ctx})",
            s.head
        );
    }
}

fn writer_step(s: &mut S, f: &Flags) -> bool {
    match s.w {
        0 => {
            // Collect: C present => nothing below it is uploaded; C absent => the real publisher
            // would also upload C and D (not modelled: that path never relies on an old object).
            s.w = if s.exists.contains(&C) {
                if s.adopt_only {
                    s.w_start = s.now;
                    2
                } else {
                    1
                }
            } else {
                6
            };
        }
        1 => {
            // first write of this publish: the hold-time clock starts here
            s.exists.insert(P);
            s.touch.insert(P, s.now);
            s.w_start = s.now;
            s.w = 2;
        }
        2 => {
            if s.exists.contains(&C) {
                if f.writer_touches {
                    s.touch.insert(C, s.now);
                }
                s.w = 3;
            } else {
                s.w = 6; // touch failed: C is gone, redo Collect (abort in the model)
            }
        }
        3 => s.w = if f.writer_checks_tomb && s.tomb.contains(&C) { 6 } else { 4 },
        4 => s.w = if f.writer_rechecks_exists && !s.exists.contains(&C) { 6 } else { 5 },
        5 => {
            if f.writer_cas_fence && s.now - s.w_start > H {
                s.w = 6;
            } else {
                s.head = if s.adopt_only { C } else { P };
                s.w = 6;
            }
        }
        _ => return false,
    }
    true
}

fn sweeper_step(s: &mut S, f: &Flags) -> bool {
    let old = |s: &S, o: usize| s.now - s.touch[&o] > G;
    match s.s {
        0 => {
            let reach = closure(&s.exists, &[s.head]);
            s.cands = s
                .exists
                .iter()
                .copied()
                .filter(|o| old(s, *o) && !(f.candidates_exclude_reachable && reach.contains(o)))
                .collect();
            s.pos = 0;
            s.s = if f.sweeper_tombs_before_remark { 1 } else { 2 };
        }
        1 => {
            if s.pos < s.cands.len() {
                s.tomb.insert(s.cands[s.pos]);
                s.pos += 1;
            } else {
                s.s = if f.sweeper_tombs_before_remark { 2 } else { 3 };
                if !f.sweeper_tombs_before_remark {
                    s.pos = 0;
                }
            }
        }
        2 => {
            let mut roots = vec![];
            if f.remark_reads_head {
                roots.push(s.head);
            }
            if f.young_objects_are_roots {
                roots.extend(s.exists.iter().copied().filter(|o| !old(s, *o)));
            }
            let keep = closure(&s.exists, &roots);
            s.del = s.cands.iter().copied().filter(|o| !keep.contains(o)).collect();
            s.pos = 0;
            s.del_done = false;
            s.s = if f.sweeper_tombs_before_remark { 3 } else { 1 };
            if !f.sweeper_tombs_before_remark {
                // mutation: tombs written only after the remark decision
                s.pos = 0;
            }
        }
        3 => {
            if s.pos < s.cands.len() {
                let o = s.cands[s.pos];
                if s.del.contains(&o) && !s.del_done {
                    s.exists.remove(&o);
                    s.del_done = true;
                } else {
                    s.tomb.remove(&o);
                    s.del_done = false;
                    s.pos += 1;
                }
            } else {
                s.s = 4;
            }
        }
        _ => return false,
    }
    // mutation path (tomb after remark): phase 1 loop then goes straight to deletes
    if !f.sweeper_tombs_before_remark && s.s == 1 && s.pos >= s.cands.len() {
        s.s = 3;
        s.pos = 0;
    }
    true
}

fn explore(s: &S, f: &Flags, seen: &mut HashSet<S>, nodes: &mut u64) {
    if !seen.insert(s.clone()) {
        return;
    }
    *nodes += 1;
    let mut any = false;
    let mut a = s.clone();
    if writer_step(&mut a, f) {
        any = true;
        check(&a, "writer step");
        explore(&a, f, seen, nodes);
    }
    let mut b = s.clone();
    if sweeper_step(&mut b, f) {
        any = true;
        check(&b, "sweeper step");
        explore(&b, f, seen, nodes);
    }
    if s.now < MAX_TICKS && !(s.w == 6 && s.s == 4) {
        let mut t = s.clone();
        t.now += 1;
        explore(&t, f, seen, nodes);
        any = true;
    }
    let _ = any;
}

fn run(f: Flags) -> u64 {
    run_variant(f, false) + run_variant(f, true)
}

fn run_variant(f: Flags, adopt_only: bool) -> u64 {
    let init = S {
        now: 0,
        exists: [C, D, H0, Y].into_iter().collect(),
        touch: [C, D, H0, Y].into_iter().map(|o| (o, OLD)).collect(),
        tomb: BTreeSet::new(),
        head: H0,
        w: 0,
        adopt_only,
        w_start: 0,
        s: 0,
        cands: vec![],
        del: vec![],
        pos: 0,
        del_done: false,
    };
    let (mut seen, mut nodes) = (HashSet::new(), 0);
    explore(&init, &f, &mut seen, &mut nodes);
    println!("gc model (adopt_only={adopt_only}): {nodes} distinct states");
    nodes
}

#[test]
fn gc_protocol_never_deletes_reachable_data() {
    assert!(run(GOOD) > 200);
}

#[test]
#[should_panic(expected = "G-REACH")]
fn mutation_publisher_does_not_touch_on_dedup() {
    run(Flags {
        writer_touches: false,
        ..GOOD
    });
}
#[test]
#[should_panic(expected = "G-REACH")]
fn mutation_publisher_does_not_check_tomb() {
    run(Flags {
        writer_checks_tomb: false,
        ..GOOD
    });
}
#[test]
#[should_panic(expected = "G-REACH")]
fn mutation_publisher_does_not_recheck_existence_after_tomb_check() {
    run(Flags {
        writer_rechecks_exists: false,
        ..GOOD
    });
}
#[test]
#[should_panic(expected = "G-REACH")]
fn mutation_publisher_cas_not_fenced_by_hold_time() {
    run(Flags {
        writer_cas_fence: false,
        ..GOOD
    });
}
#[test]
#[should_panic(expected = "G-REACH")]
fn mutation_sweeper_writes_tombs_after_deciding() {
    run(Flags {
        sweeper_tombs_before_remark: false,
        ..GOOD
    });
}
#[test]
#[should_panic(expected = "G-REACH")]
fn mutation_young_objects_are_not_roots() {
    run(Flags {
        young_objects_are_roots: false,
        ..GOOD
    });
}
#[test]
#[should_panic(expected = "G-REACH")]
fn mutation_sweeper_ignores_the_manifest() {
    run(Flags {
        remark_reads_head: false,
        candidates_exclude_reachable: false,
        ..GOOD
    });
}
