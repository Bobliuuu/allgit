//! Experiments A and C: delta packing baselines, orderings, depth/checkpoint/block constraints, reconstruction cost.

use crate::dataset::*;
use crate::delta::{self, Index};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Codec {
    Zstd3,
    Zlib6,
}

impl Codec {
    pub fn name(&self) -> &'static str {
        match self {
            Codec::Zstd3 => "zstd-3",
            Codec::Zlib6 => "zlib-6",
        }
    }
    pub fn compress(&self, d: &[u8]) -> Vec<u8> {
        match self {
            Codec::Zstd3 => zstd::bulk::compress(d, 3).unwrap(),
            Codec::Zlib6 => {
                use std::io::Write;
                let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
                e.write_all(d).unwrap();
                e.finish().unwrap()
            }
        }
    }
    pub fn decompress(&self, d: &[u8], raw_len: usize) -> Vec<u8> {
        match self {
            Codec::Zstd3 => zstd::bulk::decompress(d, raw_len.max(1)).unwrap(),
            Codec::Zlib6 => {
                use std::io::Read;
                let mut o = Vec::with_capacity(raw_len);
                flate2::read::ZlibDecoder::new(d).read_to_end(&mut o).unwrap();
                o
            }
        }
    }
}

pub fn threads() -> usize {
    std::env::var("LAB_THREADS").ok().and_then(|s| s.parse().ok()).unwrap_or(24)
}

/// Run `f(i)` for i in 0..n on all threads, results in index order.
pub fn par_map<T: Send, F: Fn(usize) -> T + Sync>(n: usize, f: F) -> Vec<T> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<(usize, T)>> = Mutex::new(Vec::with_capacity(n));
    std::thread::scope(|s| {
        for _ in 0..threads() {
            s.spawn(|| {
                let mut local = vec![];
                loop {
                    let a = next.fetch_add(64, Ordering::Relaxed);
                    if a >= n {
                        break;
                    }
                    for i in a..(a + 64).min(n) {
                        local.push((i, f(i)));
                    }
                }
                out.lock().unwrap().extend(local);
            });
        }
    });
    let mut v = out.into_inner().unwrap();
    v.sort_by_key(|x| x.0);
    v.into_iter().map(|x| x.1).collect()
}

pub fn name_hash(p: &str) -> u32 {
    let mut h = 0u32;
    for c in p.bytes() {
        if !c.is_ascii_whitespace() {
            h = (h >> 2).wrapping_add((c as u32) << 24);
        }
    }
    h
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ord_ {
    Git,
    PathCommitAsc,
    PathCommitDesc,
    Commit,
    Id,
    Size,
}

impl Ord_ {
    pub fn name(&self) -> &'static str {
        match self {
            Ord_::Git => "git-like(kind,namehash,size desc)",
            Ord_::PathCommitAsc => "path,commit asc",
            Ord_::PathCommitDesc => "path,commit desc (newest first)",
            Ord_::Commit => "commit order",
            Ord_::Id => "id order (control)",
            Ord_::Size => "size desc",
        }
    }
}

/// Position -> object index. Objects are always grouped by kind first so a delta window never mixes kinds.
pub fn ordering(ds: &Dataset, o: Ord_) -> Vec<u32> {
    let mut v: Vec<u32> = (0..ds.objs.len() as u32).collect();
    let nh: Vec<u32> = ds.paths.iter().map(|p| name_hash(p)).collect();
    // path rank for lexicographic path order
    let mut pr: Vec<u32> = (0..ds.paths.len() as u32).collect();
    pr.sort_by(|a, b| ds.paths[*a as usize].cmp(&ds.paths[*b as usize]));
    let mut rank = vec![0u32; pr.len()];
    for (r, p) in pr.iter().enumerate() {
        rank[*p as usize] = r as u32;
    }
    v.sort_by(|&a, &b| {
        let (x, y) = (&ds.objs[a as usize], &ds.objs[b as usize]);
        let k = y.kind.cmp(&x.kind);
        if k != std::cmp::Ordering::Equal {
            return k;
        }
        match o {
            Ord_::Git => nh[y.path as usize].cmp(&nh[x.path as usize]).then(y.data.len().cmp(&x.data.len())).then(a.cmp(&b)),
            Ord_::PathCommitAsc => rank[x.path as usize].cmp(&rank[y.path as usize]).then(x.commit.cmp(&y.commit)).then(a.cmp(&b)),
            Ord_::PathCommitDesc => rank[x.path as usize].cmp(&rank[y.path as usize]).then(y.commit.cmp(&x.commit)).then(b.cmp(&a)),
            Ord_::Commit => x.commit.cmp(&y.commit).then(a.cmp(&b)),
            Ord_::Id => x.sha.cmp(&y.sha),
            Ord_::Size => y.data.len().cmp(&x.data.len()).then(a.cmp(&b)),
        }
    });
    v
}

pub type Cands = Vec<Vec<(u32, u32)>>; // per position: (base position, raw delta size)

pub fn candidates(ds: &Dataset, order: &[u32], window: usize) -> Cands {
    let n = order.len();
    let next = AtomicUsize::new(0);
    let res: Mutex<Vec<(u32, u32, u32)>> = Mutex::new(vec![]);
    std::thread::scope(|s| {
        for _ in 0..threads() {
            s.spawn(|| {
                let mut local = vec![];
                loop {
                    let a = next.fetch_add(32, Ordering::Relaxed);
                    if a >= n {
                        break;
                    }
                    for q in a..(a + 32).min(n) {
                        let b = &ds.objs[order[q] as usize];
                        if b.kind == COMMIT || b.data.len() < 50 {
                            continue;
                        }
                        let mut idx: Option<Index> = None;
                        for p in q + 1..(q + 1 + window).min(n) {
                            let t = &ds.objs[order[p] as usize];
                            if t.kind != b.kind {
                                break;
                            }
                            if t.data.len() < 50 || b.data.len() < t.data.len() / 32 {
                                continue;
                            }
                            let idx = idx.get_or_insert_with(|| Index::new(&b.data));
                            if let Some(d) = delta::create(idx, &t.data, t.data.len() / 2) {
                                local.push((p as u32, q as u32, d.len() as u32));
                            }
                        }
                    }
                }
                res.lock().unwrap().extend(local);
            });
        }
    });
    let mut c: Cands = vec![vec![]; n];
    for (p, q, d) in res.into_inner().unwrap() {
        c[p as usize].push((q, d));
    }
    for v in c.iter_mut() {
        v.sort();
    }
    c
}

/// Greedy base choice in position order. `ok(base,target)` is the block/layout constraint.
pub fn select(c: &Cands, max_depth: u32, ok: &dyn Fn(usize, usize) -> bool) -> Vec<Option<u32>> {
    let n = c.len();
    let mut depth = vec![0u32; n];
    let mut sel = vec![None; n];
    for p in 0..n {
        let mut best: Option<(u32, u32, u32)> = None; // (dsize, depth, q)
        for &(q, d) in &c[p] {
            let dq = depth[q as usize];
            if dq + 1 > max_depth || !ok(q as usize, p) {
                continue;
            }
            let cand = (d, dq, q);
            if best.map_or(true, |b| (cand.0, cand.1) < (b.0, b.1)) {
                best = Some(cand);
            }
        }
        if let Some((_, dq, q)) = best {
            sel[p] = Some(q);
            depth[p] = dq + 1;
        }
    }
    sel
}

pub struct Packed {
    pub stored: Vec<u32>,         // stored bytes per position (compressed full or compressed delta), without framing
    pub base: Vec<Option<u32>>,   // final base position
    pub depth: Vec<u32>,
    pub full_z: Vec<u32>,         // compressed full size per position
    pub raw: u64,
}

pub fn full_sizes(ds: &Dataset, codec: Codec) -> Vec<u32> {
    par_map(ds.objs.len(), |i| codec.compress(&ds.objs[i].data).len() as u32)
}

/// Turn a base selection into final storage: keep a delta only if its compressed size beats the compressed full object
/// (`decide`), compute stored sizes and the true post-decision depths.
pub fn finalize(ds: &Dataset, order: &[u32], sel: &[Option<u32>], zfull_obj: &[u32], codec: Codec, decide: bool) -> Packed {
    let n = order.len();
    let res: Vec<(u32, Option<u32>)> = par_map(n, |p| {
        let o = order[p] as usize;
        let full = zfull_obj[o];
        if let Some(q) = sel[p] {
            let b = &ds.objs[order[q as usize] as usize];
            let idx = Index::new(&b.data);
            if let Some(d) = delta::create(&idx, &ds.objs[o].data, usize::MAX) {
                let z = codec.compress(&d).len() as u32;
                if !decide || z < full {
                    return (z, Some(q));
                }
            }
        }
        (full, None)
    });
    let mut depth = vec![0u32; n];
    for p in 0..n {
        if let Some(q) = res[p].1 {
            depth[p] = depth[q as usize] + 1;
        }
    }
    Packed {
        stored: res.iter().map(|r| r.0).collect(),
        base: res.iter().map(|r| r.1).collect(),
        depth,
        full_z: order.iter().map(|&o| zfull_obj[o as usize]).collect(),
        raw: order.iter().map(|&o| ds.objs[o as usize].data.len() as u64).sum(),
    }
}

impl Packed {
    pub fn total(&self) -> u64 {
        self.stored.iter().map(|&x| x as u64).sum()
    }
    pub fn ndelta(&self) -> usize {
        self.base.iter().filter(|b| b.is_some()).count()
    }
    pub fn depth_hist(&self) -> Vec<(u32, usize)> {
        let mut m: std::collections::BTreeMap<u32, usize> = Default::default();
        for d in &self.depth {
            *m.entry(*d).or_default() += 1;
        }
        m.into_iter().collect()
    }
    pub fn mean_depth(&self) -> f64 {
        self.depth.iter().map(|&d| d as f64).sum::<f64>() / self.depth.len().max(1) as f64
    }
    pub fn max_depth(&self) -> u32 {
        self.depth.iter().copied().max().unwrap_or(0)
    }
}

/// Logical blocks: consecutive positions grouped until the cumulative *full compressed* size reaches `bytes`.
pub fn blocks_by_size(zfull_pos: &[u32], bytes: u64) -> Vec<u32> {
    let mut b = vec![0u32; zfull_pos.len()];
    let (mut cur, mut acc) = (0u32, 0u64);
    for (i, z) in zfull_pos.iter().enumerate() {
        if acc >= bytes {
            cur += 1;
            acc = 0;
        }
        b[i] = cur;
        acc += *z as u64;
    }
    b
}

pub struct ReconStats {
    pub targets: usize,
    pub mean_reads: f64,
    pub mean_blocks: f64,
    pub mean_bytes: f64,
    pub max_reads: u32,
    pub ns_per_obj_median: f64,
    pub ns_spread: (f64, f64),
}

/// Reconstruct `targets` (positions): count reads / distinct blocks / stored bytes, and time decompress+apply
/// (hot, from in-memory compressed forms; 3 repetitions). `cache` models a per-batch cache of reconstructed objects.
pub fn recon(ds: &Dataset, order: &[u32], pk: &Packed, blk: &[u32], targets: &[usize], codec: Codec, cache: bool) -> ReconStats {
    // collect chains and prepare compressed forms outside the timed region
    let mut comp: HashMap<usize, Vec<u8>> = HashMap::new();
    let mut reads = 0u64;
    let mut blocks = 0u64;
    let mut bytes = 0u64;
    let mut maxr = 0u32;
    let mut seen_global: std::collections::HashSet<usize> = Default::default();
    let mut need: Vec<usize> = vec![];
    for &t in targets {
        let mut cur = t;
        let mut bs: std::collections::HashSet<u32> = Default::default();
        let mut r = 0u32;
        loop {
            if cache && seen_global.contains(&cur) {
                break;
            }
            r += 1;
            bytes += pk.stored[cur] as u64;
            bs.insert(blk[cur]);
            if cache {
                seen_global.insert(cur);
            }
            need.push(cur);
            match pk.base[cur] {
                Some(q) => cur = q as usize,
                None => break,
            }
        }
        reads += r as u64;
        blocks += bs.len() as u64;
        maxr = maxr.max(r);
    }
    need.sort();
    need.dedup();
    for &p in &need {
        let o = &ds.objs[order[p] as usize];
        let bytes = match pk.base[p] {
            Some(q) => {
                let idx = Index::new(&ds.objs[order[q as usize] as usize].data);
                codec.compress(&delta::create(&idx, &o.data, usize::MAX).unwrap())
            }
            None => codec.compress(&o.data),
        };
        comp.insert(p, bytes);
    }
    let mut times = vec![];
    for _ in 0..3 {
        let t0 = Instant::now();
        let mut sink = 0usize;
        let mut cache_m: HashMap<usize, Vec<u8>> = HashMap::new();
        for &t in targets {
            let v = rebuild(ds, order, pk, &comp, t, codec, if cache { Some(&mut cache_m) } else { None });
            sink += v.len();
        }
        std::hint::black_box(sink);
        times.push(t0.elapsed().as_nanos() as f64 / targets.len().max(1) as f64);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = targets.len().max(1) as f64;
    ReconStats {
        targets: targets.len(),
        mean_reads: reads as f64 / n,
        mean_blocks: blocks as f64 / n,
        mean_bytes: bytes as f64 / n,
        max_reads: maxr,
        ns_per_obj_median: times[1],
        ns_spread: (times[0], times[2]),
    }
}

fn rebuild(ds: &Dataset, order: &[u32], pk: &Packed, comp: &HashMap<usize, Vec<u8>>, p: usize, codec: Codec, mut cache: Option<&mut HashMap<usize, Vec<u8>>>) -> Vec<u8> {
    if let Some(c) = cache.as_deref() {
        if let Some(v) = c.get(&p) {
            return v.clone();
        }
    }
    let raw_len = ds.objs[order[p] as usize].data.len();
    let out = match pk.base[p] {
        None => codec.decompress(&comp[&p], raw_len),
        Some(q) => {
            let b = rebuild(ds, order, pk, comp, q as usize, codec, cache.as_deref_mut());
            let d = codec.decompress(&comp[&p], raw_len * 2 + 4096);
            delta::apply(&b, &d).expect("delta apply")
        }
    };
    if let Some(c) = cache {
        c.insert(p, out.clone());
    }
    out
}
