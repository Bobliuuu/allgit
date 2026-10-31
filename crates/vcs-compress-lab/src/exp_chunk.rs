//! Experiment D: chunking strategies, dedup across revisions and zones, manifest/index overhead.
use crate::codec::{Codec, Engine};
use crate::corpus::{Class, Corpus};
use crate::rabin::Rabin;
use crate::sys::*;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

#[derive(Clone, Debug)]
pub enum Strat {
    Whole,
    Fixed(usize),
    Fast(usize),
    Rabin(usize),
    /// whole file if <= threshold else FastCDC(avg)
    HybridSize(usize, usize),
    /// by type: text + small binary whole; large binary FastCDC 64K; model FastCDC 256K
    HybridType,
}
impl Strat {
    pub fn name(&self) -> String {
        let k = |n: usize| if n >= 1 << 20 { format!("{}M", n >> 20) } else { format!("{}K", n >> 10) };
        match self {
            Strat::Whole => "whole".into(),
            Strat::Fixed(n) => format!("fixed{}", k(*n)),
            Strat::Fast(n) => format!("fastcdc{}", k(*n)),
            Strat::Rabin(n) => format!("rabin{}", k(*n)),
            Strat::HybridSize(t, a) => format!("hybrid_size<={}->fastcdc{}", k(*t), k(*a)),
            Strat::HybridType => "hybrid_type(text,smallbin whole; largebin cdc64K; model cdc256K)".into(),
        }
    }
}
pub fn all_strats() -> Vec<Strat> {
    let mut v = vec![Strat::Whole];
    for n in [4, 16, 64] {
        v.push(Strat::Fixed(n << 10));
    }
    for n in [4, 16, 64, 256] {
        v.push(Strat::Fast(n << 10));
    }
    for n in [4, 16, 64] {
        v.push(Strat::Rabin(n << 10));
    }
    v.push(Strat::HybridSize(64 << 10, 16 << 10));
    v.push(Strat::HybridSize(256 << 10, 64 << 10));
    v.push(Strat::HybridType);
    v
}

fn fast_cuts(d: &[u8], avg: usize) -> Vec<usize> {
    fastcdc::v2020::FastCDC::new(d, avg / 4, avg, avg * 4).map(|c| c.length).collect()
}
fn cuts(s: &Strat, d: &[u8], class: Class, rb: &Rabin) -> Vec<usize> {
    if d.is_empty() {
        return vec![];
    }
    match s {
        Strat::Whole => vec![d.len()],
        Strat::Fixed(n) => {
            let mut v = vec![*n; d.len() / n];
            if d.len() % n != 0 {
                v.push(d.len() % n);
            }
            v
        }
        Strat::Fast(a) => fast_cuts(d, *a),
        Strat::Rabin(a) => rb.cuts(d, a / 4, *a, a * 4),
        Strat::HybridSize(t, a) => {
            if d.len() <= *t {
                vec![d.len()]
            } else {
                fast_cuts(d, *a)
            }
        }
        Strat::HybridType => match class {
            Class::SourceText | Class::SmallBinary => vec![d.len()],
            Class::LargeBinary => fast_cuts(d, 64 << 10),
            Class::ModelCkpt => fast_cuts(d, 256 << 10),
        },
    }
}
fn key(d: &[u8]) -> u128 {
    u128::from_le_bytes(blake3::hash(d).as_bytes()[..16].try_into().unwrap())
}

// Overhead model (documented in the report, MODEL not measurement):
const MANIFEST_HDR: u64 = 16; // per multi-chunk file manifest header
const MANIFEST_ENTRY: u64 = 36; // 32-byte chunk id + 4-byte length
const INDEX_ENTRY: u64 = 44; // 32-byte id + 8-byte pack offset + 4-byte stored length

pub fn run(c: &Corpus, reps: usize, threads: usize) -> Value {
    let strats = all_strats();
    let rb = Rabin::new();
    let total_unique: usize = c.blobs.iter().map(|b| b.data.len()).sum();
    // Pass 1: single-threaded chunking+hashing throughput (no compression), median of reps.
    let mut thr: HashMap<String, (f64, u64)> = HashMap::new();
    for s in &strats {
        let mut t = Vec::new();
        let mut pk = 0;
        for r in 0..reps {
            let st = Instant::now();
            let ((), p) = with_peak(|| {
                let mut n = 0usize;
                for b in &c.blobs {
                    let mut off = 0;
                    for l in cuts(s, &b.data, b.class, &rb) {
                        n ^= key(&b.data[off..off + l]) as usize;
                        off += l;
                    }
                }
                std::hint::black_box(n);
            });
            if r == 0 {
                pk = p;
            }
            t.push(st.elapsed().as_secs_f64());
        }
        let mut tt = t.clone();
        thr.insert(s.name(), (total_unique as f64 / 1e6 / median(&mut tt), pk));
        eprintln!("[chunk] throughput {} {:.0} MB/s", s.name(), thr[&s.name()].0);
    }
    // Pass 2: dedup + compression, strategies in parallel.
    let results: Vec<Value> = std::thread::scope(|sc| {
        let mut out: Vec<Value> = Vec::new();
        for batch in strats.chunks(threads.max(1)) {
            let hs: Vec<_> = batch.iter().map(|s| sc.spawn(|| analyze(c, s, &rb))).collect();
            for h in hs {
                out.push(h.join().unwrap());
            }
        }
        out
    });
    let mut rows = Vec::new();
    for (s, mut r) in strats.iter().zip(results) {
        let (mbps, pk) = thr[&s.name()];
        r["chunk_hash_MBps_median_1thread"] = json!(mbps);
        r["chunk_pass_peak_rss_delta_KiB"] = json!(pk);
        rows.push(r);
    }
    json!({"experiment": "chunking", "dataset": c.summary(), "compression_after_chunking": "zstd3 per unique chunk",
           "overhead_model": {"manifest_hdr_bytes": MANIFEST_HDR, "manifest_entry_bytes": MANIFEST_ENTRY, "index_entry_bytes": INDEX_ENTRY},
           "strategies": rows})
}

fn analyze(c: &Corpus, s: &Strat, rb: &Rabin) -> Value {
    let t0 = Instant::now();
    let mut z = Engine::new(Codec::Zstd(3));
    // per blob: chunk list (key, len); global chunk map key -> (raw len, comp len, class)
    let mut bchunks: Vec<Vec<(u128, u32)>> = Vec::with_capacity(c.blobs.len());
    let mut map: HashMap<u128, (u32, u32, Class)> = HashMap::new();
    for b in &c.blobs {
        let mut off = 0;
        let mut v = Vec::new();
        for l in cuts(s, &b.data, b.class, rb) {
            let ch = &b.data[off..off + l];
            let k = key(ch);
            map.entry(k).or_insert_with(|| (l as u32, z.compress(ch).len() as u32, b.class));
            v.push((k, l as u32));
            off += l;
        }
        bchunks.push(v);
    }
    // per revision
    let mut global: HashSet<u128> = HashSet::new();
    let mut zoned: HashSet<(String, u128)> = HashSet::new();
    let mut per_rev = Vec::new();
    let (mut sum_standalone_raw, mut sum_standalone_comp) = (0u64, 0u64);
    for rev in &c.revs {
        let mut seen: HashSet<u128> = HashSet::new();
        let (mut logical, mut new_raw, mut new_comp, mut st_raw, mut st_comp) = (0u64, 0u64, 0u64, 0u64, 0u64);
        for e in rev {
            let zone = Corpus::zone_of(&e.path);
            for &(k, l) in &bchunks[e.blob] {
                logical += l as u64;
                let comp = map[&k].1 as u64;
                if seen.insert(k) {
                    st_raw += l as u64;
                    st_comp += comp;
                }
                if global.insert(k) {
                    new_raw += l as u64;
                    new_comp += comp;
                }
                zoned.insert((zone.to_string(), k));
            }
        }
        sum_standalone_raw += st_raw;
        sum_standalone_comp += st_comp;
        per_rev.push(json!({"logical": logical, "new_raw": new_raw, "new_comp": new_comp, "standalone_unique_raw": st_raw, "standalone_unique_comp": st_comp}));
    }
    let uniq_raw: u64 = global.iter().map(|k| map[k].0 as u64).sum();
    let uniq_comp: u64 = global.iter().map(|k| map[k].1 as u64).sum();
    let zoned_raw: u64 = zoned.iter().map(|(_, k)| map[k].0 as u64).sum();
    let zoned_comp: u64 = zoned.iter().map(|(_, k)| map[k].1 as u64).sum();
    // chunk counts over distinct chunks referenced
    let nchunks = global.len() as u64;
    let nzoned = zoned.len() as u64;
    // manifests: one per distinct multi-chunk content
    let mut manifest = 0u64;
    let mut multi = 0u64;
    let mut referenced: HashSet<usize> = HashSet::new();
    for rev in &c.revs {
        for e in rev {
            referenced.insert(e.blob);
        }
    }
    for &bi in &referenced {
        let n = bchunks[bi].len() as u64;
        if n > 1 {
            manifest += MANIFEST_HDR + n * MANIFEST_ENTRY;
            multi += 1;
        }
    }
    let index = nchunks * INDEX_ENTRY;
    let mut by_class: HashMap<&str, (u64, u64, u64)> = HashMap::new();
    for k in &global {
        let (r, cm, cl) = map[k];
        let e = by_class.entry(cl.name()).or_default();
        e.0 += 1;
        e.1 += r as u64;
        e.2 += cm as u64;
    }
    let logical_total: u64 = per_rev.iter().map(|r| r["logical"].as_u64().unwrap()).sum();
    json!({
        "strategy": s.name(),
        "logical_bytes_all_revisions": logical_total,
        "unique_chunks": nchunks, "unique_raw_bytes": uniq_raw, "unique_comp_bytes_zstd3": uniq_comp,
        "avg_chunk_bytes": uniq_raw as f64 / nchunks.max(1) as f64,
        "manifest_bytes_model": manifest, "multi_chunk_files": multi, "index_bytes_model": index,
        "physical_total_model": uniq_comp + manifest + index,
        "physical_over_logical": (uniq_comp + manifest + index) as f64 / logical_total.max(1) as f64,
        "sum_of_per_revision_standalone_unique_raw": sum_standalone_raw, "sum_of_per_revision_standalone_unique_comp": sum_standalone_comp,
        "cross_revision_dedup_gain_comp_pct": 100.0 * (1.0 - uniq_comp as f64 / sum_standalone_comp.max(1) as f64),
        "zoned_stores": {"unique_chunks": nzoned, "unique_raw": zoned_raw, "unique_comp": zoned_comp,
            "extra_comp_vs_global_bytes": zoned_comp as i64 - uniq_comp as i64, "extra_index_bytes_model": (nzoned - nchunks) * INDEX_ENTRY},
        "by_class": by_class.iter().map(|(k, v)| (k.to_string(), json!({"chunks": v.0, "raw": v.1, "comp": v.2}))).collect::<serde_json::Map<_,_>>(),
        "per_revision": per_rev,
        "index_memory_bytes_model": nchunks * (16 + 4 + 16) + c.blobs.len() as u64 * 24,
        "analysis_secs": t0.elapsed().as_secs_f64(),
    })
}
