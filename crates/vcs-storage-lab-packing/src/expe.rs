//! Experiment E: placement policies, static vs adaptive, on a simulated sparse-checkout workload.

use crate::dataset::*;
use crate::json::*;
use crate::layout::*;

const GAP: u64 = 256 << 10;
const BLK: u64 = 1 << 20;

struct Agg {
    gets: f64,
    gets0: f64,
    amp: f64,
    eq: f64,
    blocks: f64,
    n: f64,
}

fn eval(l: &Layout, reqs: &[Vec<u32>]) -> Agg {
    let mut a = Agg { gets: 0.0, gets0: 0.0, amp: 0.0, eq: 0.0, blocks: 0.0, n: 0.0 };
    for r in reqs {
        if r.is_empty() {
            continue;
        }
        let c = l.cost(r, GAP);
        let c0 = l.cost(r, 0);
        a.gets += c.gets as f64;
        a.gets0 += c0.gets as f64;
        a.amp += c.bytes as f64 / c.needed.max(1) as f64;
        a.eq += c.eq();
        a.blocks += l.blocks(r, BLK).len() as f64;
        a.n += 1.0;
    }
    a
}

fn fields(a: &Agg, f: &mut Vec<(&'static str, J)>) {
    f.push(("gets_g256k_mean", J::F(a.gets / a.n)));
    f.push(("gets_g0_mean", J::F(a.gets0 / a.n)));
    f.push(("read_amp_g256k_mean", J::F(a.amp / a.n)));
    f.push(("cost_eq_bytes_mean", J::F(a.eq / a.n)));
    f.push(("blocks_1MiB_mean", J::F(a.blocks / a.n)));
    f.push(("requests", J::U(a.n as u64)));
}

pub fn run_e(ds: &Dataset, out: &Out) {
    let sz = sizes(ds);
    let m = meta(ds);
    if m.live_zones.is_empty() {
        return;
    }
    let mut layouts: Vec<Layout> = orderings_static(ds, &m).into_iter().map(|(n, p)| Layout::new(n, p, &sz)).collect();
    let total = layouts[0].total();
    // E1: static policies under four workload families
    let mut rng = Rng(42);
    for f in [0.01, 0.05, 0.10, 0.50] {
        let k = k_for(&m, f);
        for (wname, kind) in [("sparse checkout of HEAD", 0), ("zone with full history", 1)] {
            let trials = 40;
            let reqs: Vec<Vec<u32>> = (0..trials)
                .map(|_| {
                    let z = uniform_zones(&m, k, &mut rng);
                    req_objs(&if kind == 0 { Req::Checkout(z) } else { Req::ZoneHistory(z) }, &m)
                })
                .collect();
            for l in &layouts {
                let a = eval(l, &reqs);
                let mut fl = vec![("policy", J::s(l.name.clone())), ("workload", J::s(wname)), ("zone_fraction", J::F(f)), ("zones_k", J::U(k as u64)), ("total_stored_bytes", J::U(total))];
                fields(&a, &mut fl);
                out.emit("E1", fl);
            }
        }
    }
    let fh: Vec<Vec<u32>> = (0..200).map(|_| req_objs(&Req::FileHistory(rng.below(m.by_path.len().max(1))), &m)).filter(|v| !v.is_empty()).collect();
    let rc: Vec<Vec<u32>> = (0..100).map(|_| req_objs(&Req::Recent(rng.below(ds.ncommits as usize), 20), &m)).collect();
    for (wname, reqs) in [("history of one file", &fh), ("incremental pull of 20 commits", &rc)] {
        if reqs.is_empty() {
            continue;
        }
        for l in &layouts {
            let a = eval(l, reqs);
            let mut fl = vec![("policy", J::s(l.name.clone())), ("workload", J::s(wname)), ("total_stored_bytes", J::U(total))];
            fields(&a, &mut fl);
            out.emit("E1", fl);
        }
    }
    // E2: adaptive vs static under a skewed, drifting workload (4 phases of Q requests; drift between B and C)
    if m.by_path.is_empty() {
        return;
    }
    let q = 150usize;
    let mut rng = Rng(7);
    let zipf_z1 = Zipf::new(&m, 1.0, &mut rng);
    let zipf_z2 = Zipf::new(&m, 1.0, &mut rng);
    let paths: Vec<usize> = (0..m.by_path.len()).collect();
    let zipf_p1 = Zipf::over(paths.clone(), 1.0, &mut rng);
    let zipf_p2 = Zipf::over(paths, 1.0, &mut rng);
    let k5 = k_for(&m, 0.05);
    let mut phase_reqs: Vec<Vec<Req>> = vec![];
    for ph in 0..4 {
        let (zz, zp) = if ph < 2 { (&zipf_z1, &zipf_p1) } else { (&zipf_z2, &zipf_p2) };
        let reqs: Vec<Req> = (0..q)
            .map(|_| {
                let x = rng.f();
                if x < 0.6 {
                    Req::Checkout(zz.sample(&mut rng, k5))
                } else if x < 0.85 {
                    Req::FileHistory(zp.sample(&mut rng, 1)[0])
                } else {
                    Req::Recent(rng.below(ds.ncommits as usize), 20)
                }
            })
            .collect();
        phase_reqs.push(reqs);
    }
    let phase_objs: Vec<Vec<Vec<u32>>> = phase_reqs.iter().map(|p| p.iter().map(|r| req_objs(r, &m)).collect()).collect();
    let by_name = |n: &str| layouts.iter().position(|l| l.name.starts_with(n)).unwrap();
    let base_commit = by_name("commit (arrival)");
    let base_zone = by_name("zone, commit");
    let base_head = by_name("zone, head-first");
    // adaptive layouts: full rewrite vs hot-set-only rewrite
    let moved_bytes = |from: &Layout, to: &Layout| -> u64 {
        let mut b = 0u64;
        for o in 0..ds.objs.len() as u32 {
            if from.block_of(o, BLK) != to.block_of(o, BLK) {
                b += to.len_of(o);
            }
        }
        b
    };
    let hot_only = |old: &Layout, trace: &[Req]| -> (Layout, u64) {
        let co = coaccess_order(ds, &m, trace);
        let mut touched = vec![false; ds.objs.len()];
        for r in trace {
            for o in req_objs(r, &m) {
                touched[o as usize] = true;
            }
        }
        let mut perm: Vec<u32> = co.iter().copied().filter(|&o| touched[o as usize]).collect();
        let hot_bytes: u64 = perm.iter().map(|&o| old.len_of(o)).sum();
        perm.extend(old.perm.iter().copied().filter(|&o| !touched[o as usize]));
        (Layout::new("hot-only", perm, &sz), hot_bytes)
    };
    let start = &layouts[base_zone];
    let trace_a = phase_reqs[0].clone();
    let trace_c = phase_reqs[2].clone();
    let full_a = Layout::new("adaptive-full(A)", coaccess_order(ds, &m, &trace_a), &sz);
    let full_a_cost = moved_bytes(start, &full_a);
    let (hot_a, hot_a_cost) = hot_only(start, &trace_a);
    let full_c = Layout::new("adaptive-full(C)", coaccess_order(ds, &m, &trace_c), &sz);
    let full_c_cost = moved_bytes(&full_a, &full_c);
    let (hot_c, hot_c_cost) = hot_only(&hot_a, &trace_c);
    // timeline per policy: layout used in phase 0..3 and reorganization bytes paid before phases 1 and 3
    struct Pol<'a> {
        name: &'static str,
        lay: [&'a Layout; 4],
        reorg: [u64; 4],
    }
    let pols = vec![
        Pol { name: "static: commit order", lay: [&layouts[base_commit]; 4], reorg: [0; 4] },
        Pol { name: "static: zone, commit", lay: [start; 4], reorg: [0; 4] },
        Pol { name: "static: zone, head-first, path, commit", lay: [&layouts[base_head]; 4], reorg: [0; 4] },
        Pol { name: "adaptive: full co-access rewrite after phase A (once)", lay: [start, &full_a, &full_a, &full_a], reorg: [0, full_a_cost, 0, 0] },
        Pol { name: "adaptive: hot-set-only rewrite after phase A (once)", lay: [start, &hot_a, &hot_a, &hot_a], reorg: [0, hot_a_cost, 0, 0] },
        Pol { name: "adaptive: full rewrite after A and again after C", lay: [start, &full_a, &full_a, &full_c], reorg: [0, full_a_cost, 0, full_c_cost] },
        Pol { name: "adaptive: hot-set-only after A and again after C", lay: [start, &hot_a, &hot_a, &hot_c], reorg: [0, hot_a_cost, 0, hot_c_cost] },
    ];
    // static reference per phase for break-even
    let mut static_eq = [0f64; 4];
    for ph in 0..4 {
        let a = eval(start, &phase_objs[ph]);
        static_eq[ph] = a.eq / a.n;
    }
    for p in &pols {
        let mut cum_saved = 0f64;
        let mut cum_reorg = 0f64;
        for ph in 0..4 {
            let a = eval(p.lay[ph], &phase_objs[ph]);
            cum_reorg += 2.0 * p.reorg[ph] as f64; // read + write
            cum_saved += (static_eq[ph] - a.eq / a.n) * q as f64;
            let mut fl = vec![
                ("policy", J::s(p.name)),
                ("phase", J::s(["A (train)", "B (same dist)", "C (drift)", "D (same as C)"][ph])),
                ("reorg_rewrite_bytes_before_phase", J::U(p.reorg[ph])),
                ("cum_saved_minus_reorg_cost_eq_bytes_vs_static_zone_commit", J::F(cum_saved - cum_reorg)),
                ("total_stored_bytes", J::U(total)),
            ];
            fields(&a, &mut fl);
            out.emit("E2", fl);
        }
    }
    layouts.clear();
}
