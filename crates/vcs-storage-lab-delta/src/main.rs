//! Delta gate experiment on real allgit-encoded objects (issues #250, #244).
//!
//! usage: deltagate <repo-name> <path/to/.vcs/objects> [--time]
//! Reads every object of a LocalStore (the repo was made with `vcs import-git`), classifies it,
//! and prints one `R` line per (class, variant) with the stored bytes. Variants:
//!   loose, loose4k (4 KiB file-system blocks), v1, v2 (zstd-3 per object), solid4m, delta<D>.
//! With --time it also reconstructs every object from the delta plans (single thread, in memory)
//! and verifies the result equals the original bytes.
#[path = "../../vcs-storage-lab-packing/src/delta.rs"]
#[allow(dead_code)]
mod delta;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use vcs_objects::{EntryMode, Object, ObjectId};
use vcs_store::pack::{decode_payload, encode_payload, PackFormat};
use vcs_store::{LocalStore, ObjectStore};

const WINDOW: usize = 8;
const REF_BYTES: usize = 4; // base reference inside a pack: ordinal of the base entry
const BLOCK: usize = 4 << 20;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
enum Class {
    BlobText,
    BlobBin,
    Tree,
    Commit,
    Manifest,
    Other,
}
const CLASSES: [Class; 6] = [
    Class::BlobText,
    Class::BlobBin,
    Class::Tree,
    Class::Commit,
    Class::Manifest,
    Class::Other,
];

struct Obj {
    class: Class,
    enc: Vec<u8>,
    group: String,
    rank: u64, // 0 = newest commit that contains the object
}

fn is_binary(enc: &[u8]) -> bool {
    enc.get(2..).map_or(false, |p| p[..p.len().min(8000)].contains(&0))
}

fn par_map<T: Send, R: Send>(items: Vec<T>, f: impl Fn(T) -> R + Sync) -> Vec<R> {
    let n = items.len();
    let slots: Vec<Mutex<Option<T>>> = items.into_iter().map(|x| Mutex::new(Some(x))).collect();
    let out: Vec<Mutex<Option<R>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let th = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..th {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n {
                    break;
                }
                let x = slots[i].lock().unwrap().take().unwrap();
                *out[i].lock().unwrap() = Some(f(x));
            });
        }
    });
    out.into_iter().map(|m| m.into_inner().unwrap().unwrap()).collect()
}

fn z3(b: &[u8]) -> usize {
    zstd::bulk::compress(b, 3).map(|c| c.len()).unwrap_or(b.len())
}

fn v2len(enc: &[u8]) -> usize {
    encode_payload(PackFormat::V2, enc).len()
}

// ---- loading -------------------------------------------------------------------------------

struct Loaded {
    objs: Vec<Obj>,
    ids: Vec<ObjectId>,
}

fn load(dir: &str) -> Loaded {
    let st = LocalStore::open(dir).expect("open store");
    let mut ids = st.list().expect("list");
    ids.sort();
    let st = &st;
    let decoded: Vec<(ObjectId, Object)> = par_map(ids.clone(), |id| (id, st.get(&id).expect("get")));
    // commits newest first
    let mut commits: Vec<(u64, ObjectId, ObjectId)> = Vec::new();
    let mut by_id: HashMap<ObjectId, &Object> = HashMap::new();
    for (id, o) in &decoded {
        if let Object::Commit(c) = o {
            commits.push((c.author.timestamp, *id, c.root));
        }
        by_id.insert(*id, o);
    }
    commits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    // first-seen path + rank per object
    let mut meta: HashMap<ObjectId, (String, u64)> = HashMap::new();
    let mut crank: HashMap<ObjectId, u64> = HashMap::new();
    for (rank, (_, cid, root)) in commits.iter().enumerate() {
        crank.insert(*cid, rank as u64);
        let mut stack = vec![(*root, String::new())];
        while let Some((tid, path)) = stack.pop() {
            if meta.contains_key(&tid) {
                continue;
            }
            meta.insert(tid, (path.clone(), rank as u64));
            let Some(Object::Tree(t)) = by_id.get(&tid) else { continue };
            for e in t.entries() {
                let p = if path.is_empty() {
                    e.name.clone()
                } else {
                    format!("{path}/{}", e.name)
                };
                if e.mode == EntryMode::Directory {
                    stack.push((e.id, p));
                } else if !meta.contains_key(&e.id) {
                    meta.insert(e.id, (p.clone(), rank as u64));
                    if let Some(Object::Chunked(m)) = by_id.get(&e.id) {
                        for (ci, c) in m.chunks.iter().enumerate() {
                            meta.entry(c.id).or_insert((format!("{p}#{ci}"), rank as u64));
                        }
                    }
                }
            }
        }
    }
    let mut objs = Vec::with_capacity(decoded.len());
    for (id, o) in &decoded {
        let enc = o.encode();
        let (class, group, rank) = match o {
            Object::Blob(_) => {
                let (g, r) = meta.get(id).cloned().unwrap_or(("~unreachable".into(), u64::MAX / 2));
                (if is_binary(&enc) { Class::BlobBin } else { Class::BlobText }, g, r)
            }
            Object::Tree(_) => {
                let (g, r) = meta.get(id).cloned().unwrap_or(("~unreachable".into(), u64::MAX / 2));
                (Class::Tree, g, r)
            }
            Object::Commit(_) => (Class::Commit, "commits".into(), crank[id]),
            Object::Chunked(_) => {
                let (g, r) = meta.get(id).cloned().unwrap_or(("~unreachable".into(), u64::MAX / 2));
                (Class::Manifest, g, r)
            }
            _ => (Class::Other, "other".into(), 0),
        };
        objs.push(Obj { class, enc, group, rank });
    }
    Loaded { objs, ids }
}

// ---- layouts -------------------------------------------------------------------------------

/// Groups of object indices (one per path), each newest first.
fn groups_of(objs: &[Obj], idx: &[usize]) -> Vec<Vec<usize>> {
    let mut m: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for &i in idx {
        m.entry(objs[i].group.as_str()).or_default().push(i);
    }
    m.into_values()
        .map(|mut v| {
            v.sort_by_key(|&i| (objs[i].rank, i));
            v
        })
        .collect()
}

struct Solid {
    blocks: Vec<Vec<u8>>,                       // zstd-3 compressed, 1 codec byte not included
    loc: HashMap<usize, (usize, usize, usize)>, // object -> (block, offset, len)
    bytes: usize,
}

/// Path order, newest first inside a path; blocks of at most `block` raw bytes (an oversized object gets its own block).
fn build_solid(objs: &[Obj], groups: &[Vec<usize>], block: usize) -> Solid {
    let mut raw_blocks: Vec<Vec<u8>> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut loc = HashMap::new();
    for g in groups {
        for &i in g {
            let e = &objs[i].enc;
            if !cur.is_empty() && cur.len() + e.len() > block {
                raw_blocks.push(std::mem::take(&mut cur));
            }
            loc.insert(i, (raw_blocks.len(), cur.len(), e.len()));
            cur.extend_from_slice(e);
        }
    }
    if !cur.is_empty() {
        raw_blocks.push(cur);
    }
    let blocks: Vec<Vec<u8>> = par_map(raw_blocks, |b| zstd::bulk::compress(&b, 3).unwrap());
    // 1 codec byte per block + 4 bytes per object for its length in the block directory
    let bytes = blocks.iter().map(|b| b.len() + 1).sum::<usize>() + 4 * loc.len();
    Solid { blocks, loc, bytes }
}

fn solid_read(sol: &Solid, i: usize) -> Vec<u8> {
    let (b, off, len) = sol.loc[&i];
    let raw = zstd::bulk::decompress(&sol.blocks[b], 1 << 28).unwrap();
    raw[off..off + len].to_vec()
}

#[derive(Clone)]
struct Plan {
    base: Option<usize>, // object index of the base
    payload: Vec<u8>,    // v2 payload (whole) or zstd(delta) with codec byte
    depth: u32,
}

const CODEC_DELTA: u8 = 2;

fn plan_group(objs: &[Obj], g: &[usize], maxd: u32) -> Vec<(usize, Plan)> {
    let mut out: Vec<(usize, Plan)> = Vec::with_capacity(g.len());
    // window of recent (newer) members: (object index, depth, delta index over its bytes)
    let mut win: Vec<(usize, u32, delta::Index)> = Vec::new();
    // Safety: Index borrows from objs[].enc, which outlives this function
    for &i in g {
        let target = &objs[i].enc;
        let whole = encode_payload(PackFormat::V2, target);
        let mut best: Option<(usize, u32, Vec<u8>)> = None;
        let mut best_len = whole.len();
        if target.len() >= 32 {
            for (bi, bd, idx) in win.iter().rev().take(WINDOW) {
                if *bd >= maxd {
                    continue;
                }
                if let Some(d) = delta::create(idx, target, target.len()) {
                    let c = zstd::bulk::compress(&d, 3).unwrap();
                    let l = 1 + REF_BYTES + c.len();
                    if l < best_len {
                        best_len = l;
                        let mut p = vec![CODEC_DELTA];
                        p.extend_from_slice(&c);
                        best = Some((*bi, bd + 1, p));
                    }
                }
            }
        }
        let (plan, depth) = match best {
            Some((b, d, p)) => (
                Plan {
                    base: Some(b),
                    payload: p,
                    depth: d,
                },
                d,
            ),
            None => (
                Plan {
                    base: None,
                    payload: whole,
                    depth: 0,
                },
                0,
            ),
        };
        out.push((i, plan));
        // newest-first: this object may serve as base for the following (older) ones
        let idx = delta::Index::new(unsafe { std::mem::transmute::<&[u8], &'static [u8]>(target.as_slice()) });
        win.push((i, depth, idx));
        if win.len() > WINDOW * 2 {
            win.drain(..WINDOW);
        }
    }
    out
}

fn plan_all(objs: &[Obj], groups: &[Vec<usize>], maxd: u32) -> HashMap<usize, Plan> {
    let items: Vec<&Vec<usize>> = groups.iter().collect();
    par_map(items, |g| plan_group(objs, g, maxd)).into_iter().flatten().collect()
}

fn reconstruct(objs: &[Obj], plans: &HashMap<usize, Plan>, i: usize) -> Vec<u8> {
    let p = &plans[&i];
    match p.base {
        None => decode_payload(2, &p.payload).unwrap().into_owned(),
        Some(b) => {
            let base = reconstruct(objs, plans, b);
            let d = zstd::bulk::decompress(&p.payload[1..], 1 << 28).unwrap();
            delta::apply(&base, &d).expect("apply")
        }
    }
}

/// Content sketch independent of paths: (min, max) over a rolling hash of every 16-byte window.
fn sketch(enc: &[u8]) -> (u64, u64) {
    const P: u64 = 0x9E37_79B1_85EB_CA87;
    const W: usize = 16;
    if enc.len() < W + 2 {
        return (u64::MAX, 0);
    }
    let mut pw = 1u64;
    for _ in 0..W - 1 {
        pw = pw.wrapping_mul(P);
    }
    let mut h = 0u64;
    for &c in &enc[2..2 + W] {
        h = h.wrapping_mul(P).wrapping_add(c as u64 + 1);
    }
    let mix = |x: u64| (x ^ (x >> 29)).wrapping_mul(0xff51afd7ed558ccd) ^ (x >> 32);
    let (mut mn, mut mx) = (mix(h), mix(h));
    for i in 2 + W..enc.len() {
        h = h
            .wrapping_sub((enc[i - W] as u64 + 1).wrapping_mul(pw))
            .wrapping_mul(P)
            .wrapping_add(enc[i] as u64 + 1);
        let m = mix(h);
        mn = mn.min(m);
        mx = mx.max(m);
    }
    (mn, mx)
}

/// Path-free grouping: sort by sketch, then cut into chunks of 4096 objects (so planning parallelises).
fn sketch_groups(objs: &[Obj], idx: &[usize]) -> Vec<Vec<usize>> {
    let mut v: Vec<(u64, u64, usize, usize)> = par_map(idx.to_vec(), |i| {
        let (a, b) = sketch(&objs[i].enc);
        (a, b, usize::MAX - objs[i].enc.len(), i)
    });
    v.sort();
    v.chunks(4096).map(|c| c.iter().map(|x| x.3).collect()).collect()
}

// ---- trees ---------------------------------------------------------------------------------

fn varint(out: &mut Vec<u8>, mut v: usize) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// (frontcoded, names+modes stream, ids stream, entries, id bytes)
fn tree_variants(enc: &[u8]) -> (usize, usize, usize, usize) {
    let Object::Tree(t) = Object::decode(enc).unwrap() else {
        unreachable!()
    };
    let mut fc = Vec::new();
    let mut names = Vec::new();
    let mut ids = Vec::new();
    let mut prev: &str = "";
    for e in t.entries() {
        let sh = prev.bytes().zip(e.name.bytes()).take_while(|(a, b)| a == b).count();
        varint(&mut fc, sh);
        varint(&mut fc, e.name.len() - sh);
        fc.extend_from_slice(&e.name.as_bytes()[sh..]);
        fc.push(e.mode as u8);
        fc.extend_from_slice(e.id.as_bytes());
        varint(&mut names, sh);
        varint(&mut names, e.name.len() - sh);
        names.extend_from_slice(&e.name.as_bytes()[sh..]);
        names.push(e.mode as u8);
        ids.extend_from_slice(e.id.as_bytes());
        prev = &e.name;
    }
    let fcz = 1 + z3(&fc).min(fc.len());
    let col = 1 + z3(&names).min(names.len()) + ids.len(); // ids are hashes: stored raw
    (fcz, col, t.entries().len(), ids.len())
}

// ---- main ----------------------------------------------------------------------------------

fn r(repo: &str, class: &str, variant: &str, count: usize, bytes: usize, v2: usize, raw: usize) {
    println!(
        "R\t{repo}\t{class}\t{variant}\t{count}\t{bytes}\t{:.4}\t{:.4}",
        bytes as f64 / v2.max(1) as f64,
        v2 as f64 / bytes.max(1) as f64
    );
    let _ = raw;
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (repo, dir) = (a[1].as_str(), a[2].as_str());
    if a.iter().any(|x| x == "--readall") {
        // product read path: LocalStore::get (pack read + delta resolve + hash verify) for every object, newest pack format as stored
        let st = LocalStore::open(dir).expect("open store");
        let mut ids = st.list().expect("list");
        ids.sort();
        let mut us = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            for id in &ids {
                std::hint::black_box(st.get(id).expect("get"));
            }
            us.push(t.elapsed().as_secs_f64() * 1e6 / ids.len() as f64);
        }
        println!(
            "READALL\t{repo}\tobjects={}\tus_per_get_median={:.2}\truns={:?}",
            ids.len(),
            median(&mut us.clone()),
            us.iter().map(|x| (x * 10.0).round() / 10.0).collect::<Vec<_>>()
        );
        return;
    }
    let timing = a.iter().any(|x| x == "--time");
    let t0 = Instant::now();
    let l = load(dir);
    eprintln!("loaded {} objects in {:.1}s", l.objs.len(), t0.elapsed().as_secs_f64());
    let _ = &l.ids;
    let objs = &l.objs;
    println!("H\trepo\tclass\tvariant\tobjects\tbytes\tratio_to_v2\tshrink_vs_v2");
    let mut views: Vec<(String, Vec<usize>)> = CLASSES
        .iter()
        .map(|c| (format!("{c:?}"), (0..objs.len()).filter(|&i| objs[i].class == *c).collect()))
        .collect();
    views.push(("ALL".into(), (0..objs.len()).collect()));
    for (name, idx) in &views {
        if idx.is_empty() {
            continue;
        }
        let n = idx.len();
        let raw: usize = idx.iter().map(|&i| objs[i].enc.len()).sum();
        let loose4k: usize = idx.iter().map(|&i| objs[i].enc.len().div_ceil(4096) * 4096).sum();
        let v2: usize = par_map(idx.clone(), |i| v2len(&objs[i].enc)).into_iter().sum();
        r(repo, name, "raw", n, raw, v2, raw);
        r(repo, name, "loose_4k_blocks", n, loose4k, v2, raw);
        r(repo, name, "pack_v1", n, raw, v2, raw);
        r(repo, name, "pack_v2_zstd3", n, v2, v2, raw);
        let groups = groups_of(objs, idx);
        let mut solids = Vec::new();
        for (label, blk) in [("64k", 64usize << 10), ("256k", 256 << 10), ("1m", 1 << 20), ("4m", 4 << 20)] {
            let so = build_solid(objs, &groups, blk);
            r(repo, name, &format!("pack_v2_solid{label}"), n, so.bytes, v2, raw);
            solids.push((label, so));
        }
        let mut plans_by_d = Vec::new();
        for d in [4u32, 8, 16] {
            let t = Instant::now();
            let plans = plan_all(objs, &groups, d);
            let bytes: usize = plans
                .values()
                .map(|p| p.payload.len() + if p.base.is_some() { REF_BYTES } else { 0 })
                .sum();
            let nd = plans.values().filter(|p| p.base.is_some()).count();
            let md = plans.values().map(|p| p.depth as f64).sum::<f64>() / n as f64;
            r(repo, name, &format!("delta_d{d}"), n, bytes, v2, raw);
            println!(
                "D\t{repo}\t{name}\tdelta_d{d}\tdeltified={nd}\tmean_depth={md:.2}\tplan_s={:.1}",
                t.elapsed().as_secs_f64()
            );
            plans_by_d.push((d, plans));
        }
        let sg = sketch_groups(objs, idx);
        for d in [4u32, 8, 16] {
            let plans = plan_all(objs, &sg, d);
            let bytes: usize = plans
                .values()
                .map(|p| p.payload.len() + if p.base.is_some() { REF_BYTES } else { 0 })
                .sum();
            let nd = plans.values().filter(|p| p.base.is_some()).count();
            r(repo, name, &format!("delta_sketch_d{d}"), n, bytes, v2, raw);
            println!("D\t{repo}\t{name}\tdelta_sketch_d{d}\tdeltified={nd}");
        }
        if timing {
            // baseline: v2 decode of every object
            let pre: Vec<(usize, Vec<u8>)> = idx.iter().map(|&i| (i, encode_payload(PackFormat::V2, &objs[i].enc))).collect();
            let mut base_us = Vec::new();
            for _ in 0..3 {
                let t = Instant::now();
                let mut s = 0usize;
                for (_, p) in &pre {
                    s += decode_payload(2, p).unwrap().len();
                }
                std::hint::black_box(s);
                base_us.push(t.elapsed().as_secs_f64() * 1e6 / n as f64);
            }
            let bm = median(&mut base_us);
            println!("T\t{repo}\t{name}\tv2_decode\tus_per_object={bm:.2}");
            let head: Vec<usize> = idx.iter().copied().filter(|&i| objs[i].rank == 0).collect();
            // solid blocks: a cold random read decompresses the whole block (no block cache); sample objects
            for (label, so) in &solids {
                let step = (n / 1500).max(1);
                let sample: Vec<usize> = idx.iter().copied().step_by(step).collect();
                let mut us = Vec::new();
                for _ in 0..3 {
                    let t = Instant::now();
                    let mut s = 0usize;
                    for &i in &sample {
                        s += solid_read(so, i).len();
                    }
                    std::hint::black_box(s);
                    us.push(t.elapsed().as_secs_f64() * 1e6 / sample.len() as f64);
                }
                let m = median(&mut us);
                let ok = sample.iter().all(|&i| solid_read(so, i) == objs[i].enc);
                println!(
                    "T\t{repo}\t{name}\tsolid{label}\tus_per_random_object={m:.2}\tx_v2={:.2}\tsampled={}\tOK={ok}",
                    m / bm,
                    sample.len()
                );
            }
            for (d, plans) in &plans_by_d {
                let mut us = Vec::new();
                for _ in 0..3 {
                    let t = Instant::now();
                    let mut s = 0usize;
                    for &i in idx {
                        s += reconstruct(objs, plans, i).len();
                    }
                    std::hint::black_box(s);
                    us.push(t.elapsed().as_secs_f64() * 1e6 / n as f64);
                }
                let m = median(&mut us);
                let mut hu = Vec::new();
                for _ in 0..3 {
                    let t = Instant::now();
                    let mut s = 0usize;
                    for &i in &head {
                        s += reconstruct(objs, plans, i).len();
                    }
                    std::hint::black_box(s);
                    hu.push(t.elapsed().as_secs_f64() * 1e6 / head.len().max(1) as f64);
                }
                let hm = median(&mut hu);
                let mut bad = 0;
                for &i in idx {
                    if reconstruct(objs, plans, i) != objs[i].enc {
                        bad += 1;
                    }
                }
                println!(
                    "T\t{repo}\t{name}\tdelta_d{d}\tus_per_object={m:.2}\tx_v2={:.2}\tus_per_head_object={hm:.2}\thead_objects={}\tMISMATCHES={bad}",
                    m / bm,
                    head.len()
                );
            }
        }
        if name == "Tree" {
            let rs: Vec<(usize, usize, usize, usize)> = par_map(idx.clone(), |i| tree_variants(&objs[i].enc));
            let fc: usize = rs.iter().map(|x| x.0).sum();
            let col: usize = rs.iter().map(|x| x.1).sum();
            let ents: usize = rs.iter().map(|x| x.2).sum();
            let idb: usize = rs.iter().map(|x| x.3).sum();
            r(repo, name, "tree_frontcoded_zstd3", n, fc, v2, raw);
            r(repo, name, "tree_columnar_ids_raw", n, col, v2, raw);
            println!(
                "D\t{repo}\tTree\tentries={ents}\tid_bytes={idb}\tid_share_of_raw={:.3}\tid_share_of_v2={:.3}",
                idb as f64 / raw as f64,
                idb as f64 / v2 as f64
            );
        }
    }
}
