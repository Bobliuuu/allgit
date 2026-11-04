//! Shared pieces for experiments E, F, G: object sizes, layouts (object order -> byte offsets), range-read cost model,
//! and workload generators (sparse checkout of zones, zone history, file history, recent-commit pulls).

use crate::dataset::*;
use crate::packlab::*;
use std::collections::HashMap;

pub const FRAME: u64 = 4; // per-object length prefix, as in the current pack format

pub struct Sizes {
    pub z: Vec<u32>, // zstd-3 compressed size per object (stored form, no deltas)
}

pub fn sizes(ds: &Dataset) -> Sizes {
    Sizes { z: full_sizes(ds, Codec::Zstd3) }
}

pub struct Layout {
    pub name: String,
    pub perm: Vec<u32>,   // position -> object
    pub pos: Vec<u32>,    // object -> position
    pub start: Vec<u64>,  // position -> byte offset (len = n+1)
}

impl Layout {
    pub fn new(name: &str, perm: Vec<u32>, sz: &Sizes) -> Layout {
        let mut pos = vec![0u32; perm.len()];
        let mut start = Vec::with_capacity(perm.len() + 1);
        let mut acc = 0u64;
        for (p, &o) in perm.iter().enumerate() {
            pos[o as usize] = p as u32;
            start.push(acc);
            acc += sz.z[o as usize] as u64 + FRAME;
        }
        start.push(acc);
        Layout { name: name.into(), perm, pos, start }
    }
    pub fn total(&self) -> u64 {
        *self.start.last().unwrap()
    }
    pub fn len_of(&self, o: u32) -> u64 {
        let p = self.pos[o as usize] as usize;
        self.start[p + 1] - self.start[p]
    }
    pub fn block_of(&self, o: u32, bsz: u64) -> u64 {
        self.start[self.pos[o as usize] as usize] / bsz
    }
    /// Ranged-read cost of fetching `objs`: coalesce reads whose gap is <= `gap` bytes.
    pub fn cost(&self, objs: &[u32], gap: u64) -> Cost {
        let mut ps: Vec<u32> = objs.iter().map(|&o| self.pos[o as usize]).collect();
        ps.sort_unstable();
        ps.dedup();
        let mut c = Cost::default();
        let mut run: Option<(u64, u64)> = None;
        for p in ps {
            let (s, e) = (self.start[p as usize], self.start[p as usize + 1]);
            c.needed += e - s;
            match run {
                Some((rs, re)) if s <= re + gap => run = Some((rs, e.max(re))),
                Some((rs, re)) => {
                    c.gets += 1;
                    c.bytes += re - rs;
                    run = Some((s, e));
                }
                None => run = Some((s, e)),
            }
        }
        if let Some((rs, re)) = run {
            c.gets += 1;
            c.bytes += re - rs;
        }
        c
    }
    pub fn blocks(&self, objs: &[u32], bsz: u64) -> Vec<u64> {
        let mut b: Vec<u64> = objs.iter().map(|&o| self.block_of(o, bsz)).collect();
        b.sort_unstable();
        b.dedup();
        b
    }
}

#[derive(Default, Clone, Copy)]
pub struct Cost {
    pub gets: u64,
    pub bytes: u64,
    pub needed: u64,
}

pub const LAT_EQ_BYTES: f64 = 5e6; // 1 GET ~ 50 ms first byte at 100 MB/s ~ 5 MB of transfer time (MODEL ASSUMPTION)

impl Cost {
    pub fn eq(&self) -> f64 {
        self.bytes as f64 + self.gets as f64 * LAT_EQ_BYTES
    }
    pub fn add(&mut self, o: Cost) {
        self.gets += o.gets;
        self.bytes += o.bytes;
        self.needed += o.needed;
    }
}

pub struct Meta {
    pub pathrank: Vec<u32>,
    pub dirrank: Vec<u32>,
    pub is_head: Vec<bool>,
    pub head_by_zone: Vec<Vec<u32>>,
    pub all_by_zone: Vec<Vec<u32>>,
    pub by_path: Vec<Vec<u32>>, // blobs per path with >= 2 versions
    pub by_commit: Vec<Vec<u32>>,
    pub live_zones: Vec<usize>, // zones with head objects
}

pub fn meta(ds: &Dataset) -> Meta {
    let rank_of = |keyf: &dyn Fn(&String) -> String| -> Vec<u32> {
        let mut idx: Vec<u32> = (0..ds.paths.len() as u32).collect();
        idx.sort_by_key(|&i| keyf(&ds.paths[i as usize]));
        let mut r = vec![0u32; idx.len()];
        let mut cur = 0u32;
        let mut last: Option<String> = None;
        for &i in &idx {
            let k = keyf(&ds.paths[i as usize]);
            if last.as_ref().map_or(true, |l| *l != k) {
                cur += 1;
                last = Some(k);
            }
            r[i as usize] = cur;
        }
        r
    };
    let pathrank = rank_of(&|p| p.clone());
    let dirrank = rank_of(&|p| p.rsplit_once('/').map(|x| x.0.to_string()).unwrap_or_default());
    let mut is_head = vec![false; ds.objs.len()];
    let nz = ds.zones.len();
    let mut head_by_zone = vec![vec![]; nz];
    for &h in &ds.head {
        is_head[h as usize] = true;
        head_by_zone[ds.objs[h as usize].zone as usize].push(h);
    }
    let mut all_by_zone = vec![vec![]; nz];
    let mut bp: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut by_commit = vec![vec![]; ds.ncommits as usize + 1];
    for (i, o) in ds.objs.iter().enumerate() {
        all_by_zone[o.zone as usize].push(i as u32);
        by_commit[o.commit as usize].push(i as u32);
        if o.kind == BLOB {
            bp.entry(o.path).or_default().push(i as u32);
        }
    }
    let mut by_path: Vec<Vec<u32>> = bp.into_values().filter(|v| v.len() >= 2).collect();
    by_path.sort();
    let live_zones = (0..nz).filter(|&z| !head_by_zone[z].is_empty()).collect();
    Meta { pathrank, dirrank, is_head, head_by_zone, all_by_zone, by_path, by_commit, live_zones }
}

pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut x = self.0;
        x ^= x >> 33;
        x
    }
    pub fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

#[derive(Clone)]
pub enum Req {
    Checkout(Vec<usize>), // zones at HEAD
    ZoneHistory(Vec<usize>),
    FileHistory(usize),
    Recent(usize, usize), // from commit, n commits
}

pub fn req_objs(r: &Req, m: &Meta) -> Vec<u32> {
    match r {
        Req::Checkout(z) => z.iter().flat_map(|&z| m.head_by_zone[z].iter().copied()).collect(),
        Req::ZoneHistory(z) => z.iter().flat_map(|&z| m.all_by_zone[z].iter().copied()).collect(),
        Req::FileHistory(p) => m.by_path[*p].clone(),
        Req::Recent(c, n) => (*c..(*c + *n).min(m.by_commit.len())).flat_map(|c| m.by_commit[c].iter().copied()).collect(),
    }
}

pub fn k_for(m: &Meta, f: f64) -> usize {
    ((m.live_zones.len() as f64 * f).round() as usize).clamp(1, m.live_zones.len())
}

/// Zipf popularity over a permutation of the live zones; sample k distinct zones.
pub struct Zipf {
    pub perm: Vec<usize>,
    cum: Vec<f64>,
}
impl Zipf {
    pub fn new(m: &Meta, s: f64, rng: &mut Rng) -> Zipf {
        Zipf::over(m.live_zones.clone(), s, rng)
    }
    pub fn over(items: Vec<usize>, s: f64, rng: &mut Rng) -> Zipf {
        let mut perm = items;
        for i in (1..perm.len()).rev() {
            perm.swap(i, rng.below(i + 1));
        }
        let mut cum = vec![];
        let mut a = 0.0;
        for r in 0..perm.len() {
            a += 1.0 / ((r + 1) as f64).powf(s);
            cum.push(a);
        }
        Zipf { perm, cum }
    }
    pub fn sample(&self, rng: &mut Rng, k: usize) -> Vec<usize> {
        let mut out = vec![];
        let tot = *self.cum.last().unwrap();
        let mut guard = 0;
        while out.len() < k.min(self.perm.len()) && guard < 100000 {
            guard += 1;
            let x = rng.f() * tot;
            let i = self.cum.partition_point(|c| *c < x).min(self.perm.len() - 1);
            if !out.contains(&self.perm[i]) {
                out.push(self.perm[i]);
            }
        }
        out
    }
}

pub fn uniform_zones(m: &Meta, k: usize, rng: &mut Rng) -> Vec<usize> {
    let mut v = m.live_zones.clone();
    for i in 0..k {
        let j = i + rng.below(v.len() - i);
        v.swap(i, j);
    }
    v.truncate(k);
    v
}

pub fn orderings_static(ds: &Dataset, m: &Meta) -> Vec<(&'static str, Vec<u32>)> {
    let n = ds.objs.len() as u32;
    let mk = |f: &dyn Fn(u32) -> (u64, u64, u64, u64, u64)| -> Vec<u32> {
        let mut v: Vec<u32> = (0..n).collect();
        v.sort_by_key(|&i| (f(i), i));
        v
    };
    let o = |i: u32| &ds.objs[i as usize];
    vec![
        ("id (hash) order [current pack]", mk(&|i| (u64::from_be_bytes(o(i).sha[..8].try_into().unwrap()), 0, 0, 0, 0))),
        ("commit (arrival) order", mk(&|i| (o(i).commit as u64, 0, 0, 0, 0))),
        ("zone, commit", mk(&|i| (o(i).zone as u64, o(i).commit as u64, 0, 0, 0))),
        ("zone, directory, commit", mk(&|i| (o(i).zone as u64, m.dirrank[o(i).path as usize] as u64, m.pathrank[o(i).path as usize] as u64, o(i).commit as u64, 0))),
        ("zone, path, commit (file history)", mk(&|i| (o(i).zone as u64, m.pathrank[o(i).path as usize] as u64, o(i).commit as u64, 0, 0))),
        ("zone, head-first, path, commit", mk(&|i| (o(i).zone as u64, !m.is_head[i as usize] as u64, m.pathrank[o(i).path as usize] as u64, o(i).commit as u64, 0))),
    ]
}

/// Co-access layout trained from a trace: objects touched by the same set of requests become neighbours,
/// frequently touched groups first, untouched objects last (in zone,path,commit order).
pub fn coaccess_order(ds: &Dataset, m: &Meta, trace: &[Req]) -> Vec<u32> {
    let n = ds.objs.len();
    let mut sig = vec![0u64; n];
    let mut cnt = vec![0u32; n];
    for (ri, r) in trace.iter().enumerate() {
        let h = (ri as u64 + 1).wrapping_mul(0x9E3779B97F4A7C15);
        for o in req_objs(r, m) {
            sig[o as usize] = sig[o as usize].wrapping_add(h ^ (h >> 29)).rotate_left(7);
            cnt[o as usize] += 1;
        }
    }
    let mut v: Vec<u32> = (0..n as u32).collect();
    v.sort_by_key(|&i| {
        let o = &ds.objs[i as usize];
        let c = cnt[i as usize];
        (c == 0, std::cmp::Reverse(c), sig[i as usize], o.zone, m.pathrank[o.path as usize], o.commit, i)
    });
    v
}
