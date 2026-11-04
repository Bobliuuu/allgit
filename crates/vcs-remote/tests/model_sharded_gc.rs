//! Executable model of SHARD-AWARE remote GC roots (issue #313), the sharded counterpart of
//! `model_gc.rs` (protocol G1: tombstone, remark, young objects, publisher touch).
//!
//! In sharded mode there is no single manifest to read the roots from. Roots are, for every
//! zone shard: the branch heads, AND the new heads of a pending record (a prepared or committed
//! but not yet finalized transaction). Each read of the roots is a set of per-zone reads.
//!
//! World: zone 0 head X0, zone 1 head X1, and Y, an old orphan nobody references (left by an
//! abandoned publish). One cross-zone transaction adopts Y as the head of zone 0 and gives zone 1
//! a new object W (uploaded first): P0 upload W, P1 touch Y
//! (fails if gone), P2 check tomb(Y), P3 check Y exists, P4 prepare zone 0 (pending Y), P5 below; both prepares are fenced to
//! at most H ticks after the first write (P0), P5 prepare zone 1 (pending W), P6 decide (create-only
//! record), P7 finalize zone 1, P8 finalize zone 0. Time may tick at any point, so the writer may
//! stall for longer than the grace period between any two steps (a crashed finaliser is the
//! extreme case: a helper finishes later).
//! Sweeper (G1): S0 candidates (old and not in roots), S1 tomb all, S2 remark (roots again; keep
//! roots and young objects), S3 delete + untomb each.
//! Invariant G-REACH after every step: every object reachable from the effective heads exists
//! (effective head = pending new when its transaction is Committed).
//! Variants of the roots read: ZONEWISE reads zone 0 and zone 1 in separate steps with writer
//! steps in between (so roots can be torn); ATOMIC is a single step. The model shows the
//! G1 touch/tomb handshake makes torn reads safe, but pending records MUST be roots.

use std::collections::{BTreeSet, HashSet};

const X0: usize = 0;
const Y: usize = 1;
const W: usize = 2;
const X1: usize = 3;
const N: usize = 4;
const OLD: i32 = -1000;
const G: i32 = 2;
const H: i32 = 1;
const MAX_TICKS: i32 = 6;

#[derive(Clone, Copy)]
struct Flags {
    pending_are_roots: bool,
    writer_touches: bool,
    zonewise_roots: bool,
    /// object ages are evaluated at the START of the (multi-step) root read, not at its end
    age_at_read_start: bool,
}
const GOOD: Flags = Flags {
    pending_are_roots: true,
    writer_touches: true,
    zonewise_roots: true,
    age_at_read_start: true,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct S {
    now: i32,
    exists: BTreeSet<usize>,
    touch: [i32; N],
    tomb: BTreeSet<usize>,
    head: [usize; 2],
    pend: [Option<usize>; 2],
    decided: bool,
    w: u8,
    w_touch: i32,
    // sweeper
    s: u8,
    cands: Vec<usize>,
    del: Vec<usize>,
    pos: usize,
    // zonewise root read in progress: (which zone next, collected roots)
    rr: Option<(usize, BTreeSet<usize>)>,
    read_t: i32,
}

fn effective(s: &S) -> [usize; 2] {
    let mut h = s.head;
    if s.decided {
        for z in 0..2 {
            if let Some(p) = s.pend[z] {
                h[z] = p;
            }
        }
    }
    h
}

fn roots_of_zone(s: &S, z: usize, f: &Flags) -> Vec<usize> {
    let mut r = vec![s.head[z]];
    if f.pending_are_roots {
        r.extend(s.pend[z]);
    }
    r
}

fn writer_step(s: &mut S, f: &Flags) -> bool {
    match s.w {
        0 => {
            s.exists.insert(W);
            s.touch[W] = s.now;
            s.w_touch = s.now; // fence start: first write
        }
        1 => {
            if f.writer_touches {
                if !s.exists.contains(&Y) {
                    s.w = 99;
                    return true;
                }
                s.touch[Y] = s.now;
            }
        }
        2 => {
            if s.tomb.contains(&Y) {
                s.w = 99;
                return true;
            }
        }
        3 => {
            if !s.exists.contains(&Y) {
                s.w = 99;
                return true;
            }
        }
        4 => {
            if s.now - s.w_touch > H {
                s.w = 99;
                return true;
            }
            s.pend[0] = Some(Y);
        }
        5 => {
            if s.now - s.w_touch > H {
                s.w = 99; // fenced: both prepares within H ticks of the first write
                return true;
            }
            s.pend[1] = Some(W)
        }
        6 => s.decided = true,
        7 => {
            s.head[1] = s.pend[1].take().unwrap();
        }
        8 => {
            s.head[0] = s.pend[0].take().unwrap();
        }
        _ => return false,
    }
    s.w += 1;
    true
}

fn read_roots(s: &mut S, f: &Flags, out: &mut BTreeSet<usize>, step: &mut dyn FnMut(&mut S)) {
    let _ = (f, out, step, s);
}

fn sweeper_step(s: &mut S, f: &Flags) -> bool {
    // roots read: ATOMIC in one step, or ZONEWISE in two steps (rr tracks progress)
    let mut read = |s: &mut S, first: bool| -> Option<BTreeSet<usize>> {
        if !f.zonewise_roots {
            let mut r = BTreeSet::new();
            for z in 0..2 {
                r.extend(roots_of_zone(s, z, f));
            }
            return Some(r);
        }
        let (z, mut acc) = if first { (0, BTreeSet::new()) } else { s.rr.take().unwrap() };
        acc.extend(roots_of_zone(s, z, f));
        if z == 1 {
            s.rr = None;
            Some(acc)
        } else {
            s.rr = Some((1, acc));
            None
        }
    };
    match s.s {
        0 | 2 => {
            let first = s.rr.is_none();
            if first {
                s.read_t = s.now;
            }
            let Some(roots) = read(s, first) else { return true };
            let t = if f.age_at_read_start { s.read_t } else { s.now };
            if s.s == 0 {
                s.cands = s
                    .exists
                    .iter()
                    .copied()
                    .filter(|o| t - s.touch[*o] > G && !roots.contains(o))
                    .collect();
            } else {
                let keep: BTreeSet<usize> = roots
                    .iter()
                    .copied()
                    .chain(s.exists.iter().copied().filter(|o| t - s.touch[*o] <= G))
                    .collect();
                s.del = s.tomb.iter().copied().filter(|o| !keep.contains(o)).collect();
                s.pos = 0;
            }
            s.s += 1;
        }
        1 => {
            s.tomb.extend(s.cands.iter().copied());
            s.s = 2;
        }
        3 => {
            if s.pos < s.del.len() {
                let o = s.del[s.pos];
                s.exists.remove(&o);
                s.tomb.remove(&o);
                s.pos += 1;
            } else {
                s.tomb.clear();
                s.s = 4;
            }
        }
        _ => return false,
    }
    true
}

fn check(s: &S) {
    for r in effective(s) {
        assert!(
            s.exists.contains(&r),
            "G-REACH: object {r} reachable from a head was deleted (state {s:?})"
        );
    }
}

fn explore(s: &S, f: &Flags, seen: &mut HashSet<S>) {
    if !seen.insert(s.clone()) {
        return;
    }
    let mut a = s.clone();
    if writer_step(&mut a, f) {
        check(&a);
        explore(&a, f, seen);
    }
    let mut b = s.clone();
    if sweeper_step(&mut b, f) {
        check(&b);
        explore(&b, f, seen);
    }
    if s.now < MAX_TICKS {
        let mut c = s.clone();
        c.now += 1;
        explore(&c, f, seen);
    }
}

fn run(f: Flags) -> usize {
    let s = S {
        now: 0,
        exists: [X0, X1, Y].into_iter().collect(),
        touch: [OLD, OLD, 0, OLD],
        tomb: BTreeSet::new(),
        head: [X0, X1],
        pend: [None, None],
        decided: false,
        w: 0,
        w_touch: 0,
        s: 0,
        cands: vec![],
        del: vec![],
        pos: 0,
        rr: None,
        read_t: 0,
    };
    let mut seen = HashSet::new();
    explore(&s, &f, &mut seen);
    let _ = read_roots;
    seen.len()
}

#[test]
fn sharded_roots_with_pending_never_lose_a_reachable_object() {
    let n = run(GOOD);
    println!("sharded gc roots, zonewise reads: {n} distinct states");
    assert!(n > 1000);
}

#[test]
fn atomic_root_reads_are_also_safe() {
    let n = run(Flags {
        zonewise_roots: false,
        ..GOOD
    });
    println!("sharded gc roots, atomic reads: {n} distinct states");
}

#[test]
#[should_panic(expected = "G-REACH")]
fn mutation_pending_records_are_not_roots() {
    run(Flags {
        pending_are_roots: false,
        ..GOOD
    });
}

#[test]
#[should_panic(expected = "G-REACH")]
fn mutation_writer_does_not_touch_adopted_object() {
    run(Flags {
        writer_touches: false,
        ..GOOD
    });
}

/// Negative result: with torn (zone by zone) root reads, object ages must be taken when the read
/// STARTS. Evaluating them after the last zone was read lets a stalled sweeper miss an adopted
/// orphan whose pending record appeared in a zone it had already read.
#[test]
#[should_panic(expected = "G-REACH")]
fn mutation_age_evaluated_after_the_root_read() {
    run(Flags {
        age_at_read_start: false,
        ..GOOD
    });
}
