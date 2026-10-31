//! Experiment G: hot/cold tiering with migration and miss cost over blocks of a layout.
//! Latency/bandwidth numbers are MODEL PARAMETERS (swept), not measurements of any cloud service.

use crate::dataset::*;
use crate::json::*;
use crate::layout::*;
use std::collections::{HashMap, HashSet, VecDeque};

const BLK: u64 = 1 << 20;

#[derive(Default, Clone)]
struct T {
    reqs: u64,
    hit_blocks: u64,
    miss_blocks: u64,
    gets_hot: u64,
    gets_cold: u64,
    promote_blocks: u64,
    migr_events_blocks: u64,
}

fn runs(sorted: &[u64]) -> u64 {
    let mut r = 0;
    for (i, b) in sorted.iter().enumerate() {
        if i == 0 || *b != sorted[i - 1] + 1 {
            r += 1;
        }
    }
    r
}

fn ms(t: &T, cold_lat_ms: f64) -> f64 {
    let (bw_cold, bw_hot) = (100e6, 1.5e9);
    let cold = t.gets_cold as f64 * cold_lat_ms + t.miss_blocks as f64 * BLK as f64 / bw_cold * 1000.0;
    let hot = t.gets_hot as f64 * 0.3 + t.hit_blocks as f64 * BLK as f64 / bw_hot * 1000.0;
    (cold + hot) / t.reqs.max(1) as f64
}

pub fn run_g(ds: &Dataset, out: &Out) {
    let sz = sizes(ds);
    let m = meta(ds);
    if m.by_path.is_empty() || m.live_zones.is_empty() {
        return;
    }
    let statics = orderings_static(ds, &m);
    let pick = |n: &str| statics.iter().find(|(x, _)| x.starts_with(n)).unwrap().1.clone();
    let lays = vec![Layout::new("commit (arrival) order", pick("commit (arrival)"), &sz), Layout::new("zone, path, commit", pick("zone, path"), &sz)];
    // workload: 2 phases, drift between them
    let q = 1000usize;
    let mut rng = Rng(21);
    let zs = [Zipf::new(&m, 1.0, &mut rng), Zipf::new(&m, 1.0, &mut rng)];
    let paths: Vec<usize> = (0..m.by_path.len()).collect();
    let ps = [Zipf::over(paths.clone(), 1.0, &mut rng), Zipf::over(paths, 1.0, &mut rng)];
    let k5 = k_for(&m, 0.05);
    let mut reqs: Vec<Vec<u32>> = vec![];
    for ph in 0..2 {
        for _ in 0..q {
            let x = rng.f();
            let r = if x < 0.6 {
                Req::Checkout(zs[ph].sample(&mut rng, k5))
            } else if x < 0.85 {
                Req::FileHistory(ps[ph].sample(&mut rng, 1)[0])
            } else {
                Req::Recent(rng.below(ds.ncommits as usize), 20)
            };
            reqs.push(req_objs(&r, &m));
        }
    }
    for l in &lays {
        let nblk = (l.total() / BLK + 1) as usize;
        let mut blk_newest = vec![0u32; nblk];
        for p in 0..l.perm.len() {
            let b = (l.start[p] / BLK) as usize;
            blk_newest[b] = blk_newest[b].max(ds.objs[l.perm[p] as usize].commit);
        }
        let seq: Vec<Vec<u64>> = reqs.iter().map(|r| l.blocks(r, BLK)).collect();
        let distinct: HashSet<u64> = seq.iter().flatten().copied().collect();
        for h in [0.05f64, 0.10, 0.25, 0.50] {
            let cap = ((nblk as f64 * h).round() as usize).max(1);
            // policies produce, per request, a hot-set oracle function; simulate each separately
            let policies: Vec<(&str, Box<dyn Fn() -> (Vec<T>, u64)>)> = vec![
                ("all cold (no hot tier)", Box::new(|| sim_static(&seq, q, &HashSet::new(), 0))),
                ("static hot = newest blocks (age)", Box::new({
                    let mut idx: Vec<usize> = (0..nblk).collect();
                    idx.sort_by_key(|&b| std::cmp::Reverse(blk_newest[b]));
                    let hot: HashSet<u64> = idx.into_iter().take(cap).map(|b| b as u64).collect();
                    let (seq, hot2) = (seq.clone(), hot);
                    move || sim_static(&seq, q, &hot2, cap as u64)
                })),
                ("LRU, promote on miss", Box::new(|| sim_lru(&seq, q, cap))),
                ("epoch frequency (200 req, decay 0.5)", Box::new(|| sim_freq(&seq, q, cap, 200))),
                ("oracle: top blocks by true frequency", Box::new({
                    let mut cnt: HashMap<u64, u32> = HashMap::new();
                    for s in &seq {
                        for b in s {
                            *cnt.entry(*b).or_default() += 1;
                        }
                    }
                    let mut v: Vec<(u64, u32)> = cnt.into_iter().collect();
                    v.sort_by_key(|x| (std::cmp::Reverse(x.1), x.0));
                    let hot: HashSet<u64> = v.into_iter().take(cap).map(|x| x.0).collect();
                    let seq = seq.clone();
                    move || sim_static(&seq, q, &hot, cap as u64)
                })),
            ];
            for (pn, f) in policies {
                let (ph, promoted_initial) = f();
                for (i, t) in ph.iter().enumerate() {
                    let tot_blocks = (t.hit_blocks + t.miss_blocks).max(1) as f64;
                    out.emit(
                        "G",
                        vec![
                            ("layout", J::s(l.name.clone())),
                            ("policy", J::s(pn)),
                            ("hot_capacity_fraction", J::F(h)),
                            ("phase", J::s(["1 (stationary)", "2 (after drift)"][i])),
                            ("blocks_total", J::U(nblk as u64)),
                            ("blocks_ever_touched", J::U(distinct.len() as u64)),
                            ("requests", J::U(t.reqs)),
                            ("block_hit_rate", J::F(t.hit_blocks as f64 / tot_blocks)),
                            ("cold_gets_per_req", J::F(t.gets_cold as f64 / t.reqs.max(1) as f64)),
                            ("cold_MiB_per_req", J::F(t.miss_blocks as f64 / t.reqs.max(1) as f64)),
                            ("promoted_MiB_total", J::U(t.promote_blocks)),
                            ("migration_MiB_epochs_total", J::U(t.migr_events_blocks)),
                            ("initial_placement_MiB", J::U(if i == 0 { promoted_initial } else { 0 })),
                            ("model_ms_per_req_cold20ms", J::F(ms(t, 20.0))),
                            ("model_ms_per_req_cold50ms", J::F(ms(t, 50.0))),
                            ("model_ms_per_req_cold100ms", J::F(ms(t, 100.0))),
                        ],
                    );
                }
            }
        }
    }
}

fn account(t: &mut T, blocks: &[u64], is_hot: &dyn Fn(u64) -> bool) -> Vec<u64> {
    let (mut hit, mut miss) = (vec![], vec![]);
    for &b in blocks {
        if is_hot(b) {
            hit.push(b)
        } else {
            miss.push(b)
        }
    }
    t.reqs += 1;
    t.hit_blocks += hit.len() as u64;
    t.miss_blocks += miss.len() as u64;
    t.gets_hot += runs(&hit);
    t.gets_cold += runs(&miss);
    miss
}

fn sim_static(seq: &[Vec<u64>], q: usize, hot: &HashSet<u64>, init: u64) -> (Vec<T>, u64) {
    let mut ph = vec![T::default(), T::default()];
    for (i, s) in seq.iter().enumerate() {
        account(&mut ph[i / q], s, &|b| hot.contains(&b));
    }
    (ph, init.min(hot.len() as u64))
}

fn sim_lru(seq: &[Vec<u64>], q: usize, cap: usize) -> (Vec<T>, u64) {
    let mut ph = vec![T::default(), T::default()];
    let mut hot: VecDeque<u64> = VecDeque::new();
    let mut set: HashSet<u64> = HashSet::new();
    for (i, s) in seq.iter().enumerate() {
        let t = &mut ph[i / q];
        let miss = account(t, s, &|b| set.contains(&b));
        // touch hits
        for b in s {
            if set.contains(b) && !miss.contains(b) {
                if let Some(p) = hot.iter().position(|x| x == b) {
                    hot.remove(p);
                }
                hot.push_back(*b);
            }
        }
        for b in miss {
            t.promote_blocks += 1;
            hot.push_back(b);
            set.insert(b);
            while hot.len() > cap {
                let e = hot.pop_front().unwrap();
                set.remove(&e);
            }
        }
    }
    (ph, 0)
}

fn sim_freq(seq: &[Vec<u64>], q: usize, cap: usize, epoch: usize) -> (Vec<T>, u64) {
    let mut ph = vec![T::default(), T::default()];
    let mut score: HashMap<u64, f64> = HashMap::new();
    let mut hot: HashSet<u64> = HashSet::new();
    for (i, s) in seq.iter().enumerate() {
        let t = &mut ph[i / q];
        account(t, s, &|b| hot.contains(&b));
        for b in s {
            *score.entry(*b).or_default() += 1.0;
        }
        if (i + 1) % epoch == 0 {
            let mut v: Vec<(u64, f64)> = score.iter().map(|(k, v)| (*k, *v)).collect();
            v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
            let new: HashSet<u64> = v.into_iter().take(cap).map(|x| x.0).collect();
            t.migr_events_blocks += new.difference(&hot).count() as u64;
            hot = new;
            for v in score.values_mut() {
                *v *= 0.5;
            }
        }
    }
    (ph, 0)
}
