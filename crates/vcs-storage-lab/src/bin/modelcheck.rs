//! Exhaustive interleaving check (BFS over all schedules) of cache modes against concurrent publish and GC on one object X.
//! It proves (for this small instance) which cache rules keep the invariants, and prints the shortest counterexample otherwise.
//! usage: modelcheck [--out f.jsonl]
use std::collections::{HashMap, VecDeque};
use vcs_storage_lab::util::*;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct S {
    // remote
    has: bool,
    age: u8, // ticks since last touch; GC may delete at age >= GRACE
    refs: bool, // manifest references X
    gen: u8,    // manifest generation
    epoch: u8,  // GC epoch
    // client cache
    pos: Option<(u8, u8)>, // (gc epoch at fill, conservative age estimate = age at fill + ticks since)
    neg: Option<u8>,         // manifest gen at fill
    // actors' program counters
    p: u8, // publisher: 0 start,1 observed,2 done-upload/touch,3 cas done
    p_obs: bool,
    p_cached: bool,
    ticks_in_window: u8,
    gticks: u8,
    g: u8, // gc: 0 start,1 marked,2 rechecked,3 done
    g_marked: bool,
    g_recheck_ref: bool,
    c: u8, // cache filler
    q: u8, // other publisher: 0,1 put,2 cas
    r: u8, // reader: 0,1 read manifest,2 done
    n: u8, // negative filler
    bad_dangling: bool,
    bad_missing: bool,
    ticks: u8,
}

const GRACE: u8 = 5; // ticks. A publish and a GC pass each last at most 1 tick; entries older than TOUCH_AFTER are not trusted.
const TOUCH_AFTER: u8 = 2; // safe iff TOUCH_AFTER + publish_window(1) + gc_window(1) < GRACE

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    NoCache,
    PosUnguarded,
    PosGuardedLoose, // negative control: trusts entries up to 2 ticks old (too loose for GRACE=3)
    PosGuarded, // entry valid only while epoch unchanged and object still fresh; a hit may skip the touch because fresh
    NegUnguarded,
    NegGuarded, // entry valid only while manifest generation unchanged
}

fn steps(s: &S, mode: Mode, gc_recheck: bool, scenario_gc: bool) -> Vec<(&'static str, S)> {
    let mut out = vec![];
    let mut push = |name: &'static str, mut n: S| {
        // invariants
        if n.refs && !n.has {
            n.bad_dangling = true;
        }
        out.push((name, n));
    };
    if scenario_gc {
        // Publisher P: p1 observe, p2 act, p3 CAS manifest to reference X
        if s.p == 0 {
            let mut n = *s;
            let use_cache = match (mode, s.pos) {
                (Mode::PosUnguarded, Some(_)) => true,
                (Mode::PosGuardedLoose, Some((e, est))) => e == s.epoch && est <= TOUCH_AFTER + 1,
                (Mode::PosGuarded, Some((e, est))) => e == s.epoch && est <= TOUCH_AFTER,
                _ => false,
            };
            n.p_cached = use_cache;
            n.p_obs = if use_cache { true } else { s.has };
            n.p = 1;
            n.ticks_in_window = 0;
            push("P: observe remote (cache or request)", n);
        }
        if s.p == 1 {
            let mut n = *s;
            if n.p_cached {
                // cache hit: skip upload and touch
            } else if n.p_obs {
                // dedup hit: touch (copy-to-self); fails if the object vanished meanwhile, then falls back to put
                if n.has {
                    n.age = 0;
                } else {
                    n.has = true;
                    n.age = 0;
                }
            } else {
                n.has = true;
                n.age = 0;
            }
            n.p = 2;
            push("P: upload or touch", n);
        }
        if s.p == 2 {
            let mut n = *s;
            n.refs = true;
            n.gen += 1;
            n.p = 3;
            push("P: CAS manifest referencing X", n);
        }
        // GC: mark (read manifest), optional recheck, then delete if old
        if s.g == 0 {
            let mut n = *s;
            n.g_marked = s.refs;
            n.g = 1;
            n.gticks = 0;
            push("G: mark (read roots)", n);
        }
        if s.g == 1 {
            let mut n = *s;
            n.g_recheck_ref = if gc_recheck { s.refs } else { false };
            n.g = 2;
            push("G: re-read roots before delete", n);
        }
        if s.g == 2 {
            let mut n = *s;
            if s.has && !s.g_marked && !s.g_recheck_ref && s.age >= GRACE {
                n.has = false;
            }
            n.epoch += 1;
            n.g = 3;
            push("G: sweep (delete X if unreferenced and old)", n);
        }
        // cache filler
        if s.c == 0 {
            let mut n = *s;
            if s.has {
                n.pos = Some((s.epoch, s.age));
            }
            n.c = 1;
            push("C: client caches 'remote has X'", n);
        }
        // time: object ages; at most one tick inside the publisher's window (publish duration < grace - touch_after)
        let in_pwin = s.p >= 1 && s.p < 3;
        let in_gwin = s.g >= 1 && s.g < 3;
        if s.ticks < 7 && !(in_pwin && s.ticks_in_window >= 1) && !(in_gwin && s.gticks >= 1) {
            let mut n = *s;
            n.age = (n.age + 1).min(GRACE);
            n.ticks += 1;
            if let Some((_, est)) = &mut n.pos {
                *est = (*est + 1).min(GRACE);
            }
            if in_pwin {
                n.ticks_in_window += 1;
            }
            if in_gwin {
                n.gticks += 1;
            }
            push("T: time passes (object ages)", n);
        }
    } else {
        // negative-cache scenario: Q publishes X, N caches an absence, R follows the manifest and looks X up
        if s.q == 0 {
            let mut n = *s;
            n.has = true;
            n.q = 1;
            push("Q: upload X", n);
        }
        if s.q == 1 {
            let mut n = *s;
            n.refs = true;
            n.gen += 1;
            n.q = 2;
            push("Q: CAS manifest referencing X", n);
        }
        if s.n == 0 {
            let mut n = *s;
            if !s.has {
                n.neg = Some(s.gen);
            }
            n.n = 1;
            push("N: client caches 'X absent'", n);
        }
        if s.r == 0 && s.refs {
            let mut n = *s;
            n.r = 1;
            push("R: read manifest, sees X referenced", n);
        }
        if s.r == 1 {
            let mut n = *s;
            let trust_neg = match (mode, s.neg) {
                (Mode::NegUnguarded, Some(_)) => true,
                (Mode::NegGuarded, Some(g)) => g == s.gen,
                _ => false,
            };
            if trust_neg && s.has {
                n.bad_missing = true; // reader reports "object missing" although the remote has it
            }
            n.r = 2;
            push("R: look X up (cache first)", n);
        }
    }
    out
}

fn check(mode: Mode, gc_recheck: bool, scenario_gc: bool, init: S) -> (usize, Option<Vec<String>>) {
    let mut seen: HashMap<S, (Option<S>, &'static str)> = HashMap::new();
    let mut q = VecDeque::new();
    seen.insert(init, (None, "init"));
    q.push_back(init);
    while let Some(s) = q.pop_front() {
        if s.bad_dangling || s.bad_missing {
            let mut tr = vec![];
            let mut cur = s;
            while let Some((Some(p), a)) = seen.get(&cur).map(|x| (x.0, x.1)) {
                tr.push(format!("{a}   [has={} age={} refs={} epoch={} cache_pos={:?} cache_neg={:?}]", cur.has, cur.age, cur.refs, cur.epoch, cur.pos, cur.neg));
                cur = p;
            }
            tr.reverse();
            return (seen.len(), Some(tr));
        }
        for (name, n) in steps(&s, mode, gc_recheck, scenario_gc) {
            if !seen.contains_key(&n) {
                seen.insert(n, (Some(s), name));
                q.push_back(n);
            }
        }
    }
    (seen.len(), None)
}

fn main() {
    let mut out = Out::new(arg("--out"));
    out.row(Row::new().s("exp", "env").s("env", &env_json()));
    let base = S { has: false, age: 0, refs: false, gen: 0, epoch: 0, pos: None, neg: None, p: 0, p_obs: false, p_cached: false, ticks_in_window: 0, gticks: 0, g: 0, g_marked: false, g_recheck_ref: false, c: 0, q: 0, r: 0, n: 0, bad_dangling: false, bad_missing: false, ticks: 0 };
    // GC scenario: X exists as an old-ish unreferenced orphan (age mid), a publisher re-adopts it while GC runs
    for (mode, label) in [(Mode::NoCache, "no cache (touch-on-dedup, product design)"), (Mode::PosUnguarded, "positive cache, TTL only, hit skips touch"), (Mode::PosGuarded, "positive cache guarded by gc epoch + freshness"), (Mode::PosGuardedLoose, "NEGATIVE CONTROL: guard trusts entries up to TOUCH_AFTER+1 ticks (margin violated)")] {
        for recheck in [false, true] {
            for init_age in [0u8, 2, 4, 5] {
                let init = S { has: true, age: init_age, ..base };
                let (states, cex) = check(mode, recheck, true, init);
                out.row(Row::new().s("exp", "modelcheck").s("scenario", "publish_vs_gc").s("mode", label).s("gc_rechecks_roots", if recheck { "yes" } else { "no" }).i("initial_age", init_age as u64)
                    .i("states_explored", states as u64).s("dangling_reference_possible", if cex.is_some() { "YES" } else { "no" }).s("counterexample", &cex.map(|c| c.join(" -> ")).unwrap_or_default()));
            }
        }
    }
    // Negative scenario: X absent initially, appears via Q while N cached the absence and R follows the manifest
    for (mode, label) in [(Mode::NoCache, "no negative cache"), (Mode::NegUnguarded, "negative cache, TTL only"), (Mode::NegGuarded, "negative cache guarded by manifest generation")] {
        let (states, cex) = check(mode, false, false, base);
        out.row(Row::new().s("exp", "modelcheck").s("scenario", "negative_cache_vs_publish").s("mode", label).i("states_explored", states as u64)
            .s("spurious_missing_possible", if cex.is_some() { "YES" } else { "no" }).s("counterexample", &cex.map(|c| c.join(" -> ")).unwrap_or_default()));
    }
}
