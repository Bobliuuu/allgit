//! Experiment H1: object-location indexes and negative-lookup filters.
//! usage: idxbench [--n 1000000] [--packs 1,16,256] [--ondisk-max 1000000] [--out file.jsonl] [--trials 5]
use std::hint::black_box;
use vcs_storage_lab::filters::*;
use vcs_storage_lab::index::*;
use vcs_storage_lab::ondisk::*;
use vcs_storage_lab::util::*;

#[global_allocator]
static A: Counting = Counting;

fn probes(ids: &[Id], m: usize, seed: u64) -> (Vec<Id>, Vec<Id>) {
    let mut r = Rng::new(seed);
    let hit = (0..m).map(|_| ids[(r.next_u64() % ids.len() as u64) as usize]).collect();
    let miss = random_ids(m, seed ^ 0xdead_beef_1234);
    (hit, miss)
}

fn measure_index(out: &mut Out, n: usize, p: usize, build: impl FnOnce() -> Box<dyn LocIndex>, hit: &[Id], miss: &[Id], truth: &std::collections::HashMap<Id, (u32, u64)>, nt: usize, bsecs: f64, base_live: usize) {
    let _ = base_live;
    let ix = build();
    let m = hit.len();
    let th = trials(nt, m, || {
        let mut s = 0u64;
        for id in hit {
            s += black_box(ix.get(black_box(id))).map(|l| l.off).unwrap_or(0);
        }
        black_box(s);
    });
    let tm = trials(nt, m, || {
        let mut s = 0u64;
        for id in miss {
            s += black_box(ix.get(black_box(id))).is_some() as u64;
        }
        black_box(s);
    });
    // correctness: every hit resolves to its true (pack, off); count absent ids that wrongly resolve
    let wrong = hit.iter().take(200_000).filter(|id| ix.get(id).map(|l| (l.pack, l.off)) != truth.get(*id).copied()).count();
    let fp = miss.iter().filter(|id| ix.get(id).is_some()).count();
    out.row(
        Row::new().s("exp", "loc_index").s("name", &ix.name()).i("n", n as u64).i("packs", p as u64)
            .n("build_s", bsecs).n("bytes_per_obj", ix.bytes() as f64 / n as f64)
            .n("hit_ns_med", th.median).n("hit_ns_min", th.min).n("hit_ns_max", th.max)
            .n("miss_ns_med", tm.median).n("miss_ns_min", tm.min).n("miss_ns_max", tm.max)
            .i("wrong_hits", wrong as u64).n("miss_false_pos_rate", fp as f64 / miss.len() as f64).s("state", "warm,in-memory,single-thread"),
    );
}

fn main() {
    let n: usize = arg_or("--n", 1_000_000);
    let packs: Vec<usize> = arg("--packs").unwrap_or("1,16,256".into()).split(',').map(|s| s.parse().unwrap()).collect();
    let nt: usize = arg_or("--trials", 5);
    let ondisk_max: usize = arg_or("--ondisk-max", 1_000_000);
    let mut out = Out::new(arg("--out"));
    out.row(Row::new().s("exp", "env").s("env", &env_json()).i("n", n as u64));
    let ids = random_ids(n, 7);
    let m = n.min(2_000_000);
    let (hit, miss) = probes(&ids, m, 99);

    for &p in &packs {
        let pk = Packs::split(&ids, p, true);
        let truth: std::collections::HashMap<Id, (u32, u64)> = pk.packs.iter().flatten().map(|(i, l)| (*i, (l.pack, l.off))).collect();
        macro_rules! cand {
            ($e:expr) => {{
                let before = live_bytes();
                let t = std::time::Instant::now();
                let b: Box<dyn LocIndex> = Box::new($e);
                let bs = t.elapsed().as_secs_f64();
                let _ = before;
                measure_index(&mut out, n, p, || b, &hit, &miss, &truth, nt, bs, 0);
            }};
        }
        cand!(PerPackFlat::build(&pk));
        cand!(PerPackFanout::build(&pk));
        cand!(GlobalSorted::build(&pk));
        cand!(GlobalHash::build(&pk));
        cand!(Mph::build(&pk, 1.7));
        cand!(Mph::build(&pk, 4.0));
        if n <= 4_000_000 {
            cand!(BTreeMem::build(&pk));
        }
    }

    // Filters over all n ids (one filter per pack would be the same bits/key; size scales with the pack's keys).
    macro_rules! filt {
        ($label:expr, $e:expr) => {{
            reset_peak();
            let b0 = live_bytes();
            let t = std::time::Instant::now();
            let f = $e;
            let bs = t.elapsed().as_secs_f64();
            let held = live_bytes() - b0;
            let tq_hit = trials(nt, m, || {
                let mut s = 0u64;
                for id in &hit {
                    s += black_box(f.contains(black_box(id))) as u64;
                }
                black_box(s);
            });
            let tq_miss = trials(nt, m, || {
                let mut s = 0u64;
                for id in &miss {
                    s += black_box(f.contains(black_box(id))) as u64;
                }
                black_box(s);
            });
            let fn_ = hit.iter().filter(|id| !f.contains(id)).count();
            let fp = miss.iter().filter(|id| f.contains(id)).count();
            out.row(
                Row::new().s("exp", "filter").s("name", &f.name()).i("n", n as u64).n("build_s", bs)
                    .n("bits_per_key", f.bytes() as f64 * 8.0 / n as f64).n("measured_heap_bits_per_key", held as f64 * 8.0 / n as f64)
                    .n("query_hit_ns_med", tq_hit.median).n("query_miss_ns_med", tq_miss.median).n("query_miss_ns_max", tq_miss.max)
                    .n("fpr", fp as f64 / miss.len() as f64).i("false_negatives", fn_ as u64).s("label", $label),
            );
        }};
    }
    filt!("", Bloom::build(&ids, 8.0, 5));
    filt!("", Bloom::build(&ids, 10.0, 7));
    filt!("", Bloom::build(&ids, 16.0, 11));
    filt!("", Cuckoo::build(&ids));
    filt!("", Fuse8::build(&ids));

    // Real existing format: open cost of the product's pack index at this size.
    {
        let mut sorted: Vec<&Id> = ids.iter().collect();
        sorted.sort();
        let pack_len = 100 + 40 * n as u64 + 36 + 32 + 1000;
        let mut idx = Vec::with_capacity(60 + 44 * n + 32);
        idx.extend_from_slice(b"VCSPIDX1");
        idx.extend_from_slice(&(n as u32).to_le_bytes());
        idx.extend_from_slice(&pack_len.to_le_bytes());
        idx.extend_from_slice(&[7u8; 32]);
        for (k, id) in sorted.iter().enumerate() {
            idx.extend_from_slice(*id);
            idx.extend_from_slice(&(100u64 + 40 * k as u64).to_le_bytes());
            idx.extend_from_slice(&36u32.to_le_bytes());
        }
        let h = *blake3::hash(&idx).as_bytes();
        idx.extend_from_slice(&h);
        let mut parse = vec![];
        let mut pi = None;
        for _ in 0..nt {
            let t = std::time::Instant::now();
            let x = vcs_store::pack::PackIndex::parse(&idx, std::path::Path::new("x.idx")).unwrap();
            parse.push(t.elapsed().as_secs_f64());
            pi = Some(x);
        }
        parse.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pi = pi.unwrap();
        let th = trials(nt, m, || {
            let mut s = 0u64;
            for id in &hit {
                s += black_box(pi.find(black_box(&vcs_objects::ObjectId::from_bytes(*id)))).map(|x| x.0).unwrap_or(0);
            }
            black_box(s);
        });
        let hashonly = secs(|| {
            black_box(blake3::hash(&idx));
        });
        out.row(
            Row::new().s("exp", "product_pack_index").s("name", "vcs_store::pack::PackIndex").i("n", n as u64).i("idx_file_bytes", idx.len() as u64)
                .n("parse_s_med", parse[parse.len() / 2]).n("parse_s_max", parse[parse.len() - 1]).n("blake3_only_s", hashonly)
                .n("find_hit_ns_med", th.median).s("state", "bytes already in RAM (open cost excludes file read)"),
        );
    }

    // On-disk candidates, as location indexes (value = 16 bytes).
    if n <= ondisk_max {
        let dir = std::env::var("LAB_DIR").unwrap_or_else(|_| "/tmp".into());
        let mk: Vec<(&str, Box<dyn Fn(&std::path::Path) -> Box<dyn Kv>>)> = vec![
            #[cfg(feature = "redb")]
            ("redb", Box::new(|p| Box::new(Redb::open(p)))),
            #[cfg(feature = "sqlite")]
            ("sqlite", Box::new(|p| Box::new(Sqlite::open(p)))),
            #[cfg(feature = "rocks")]
            ("rocks", Box::new(|p| Box::new(Rocks::open(p)))),
        ];
        for (label, open) in &mk {
            for order in ["random", "sorted"] {
                let d = std::path::PathBuf::from(&dir).join(format!("idx_{label}_{order}_{n}"));
                let _ = std::fs::remove_dir_all(&d);
                let kv = open(&d);
                let mut ord: Vec<&Id> = ids.iter().collect();
                if order == "sorted" {
                    ord.sort();
                }
                let bs = secs(|| {
                    for ch in ord.chunks(50_000) {
                        let items: Vec<(Vec<u8>, Vec<u8>)> = ch.iter().map(|id| (id.to_vec(), vec![1u8; 16])).collect();
                        kv.put_batch(&items);
                    }
                    kv.finish();
                });
                let disk = dir_size(&d);
                let mm = m.min(300_000);
                let th = trials(3, mm, || {
                    let mut s = 0usize;
                    for id in &hit[..mm] {
                        s += kv.get(id).map(|v| v.len()).unwrap_or(0);
                    }
                    black_box(s);
                });
                let tm = trials(3, mm, || {
                    let mut s = 0usize;
                    for id in &miss[..mm] {
                        s += kv.get(id).map(|v| v.len()).unwrap_or(0);
                    }
                    black_box(s);
                });
                drop(kv);
                // cold: drop the page cache, reopen, time the first 2000 hits
                let cold_ok = drop_caches();
                let kv2 = open(&d);
                let t = std::time::Instant::now();
                for id in &hit[..2000.min(mm)] {
                    black_box(kv2.get(id));
                }
                let cold = t.elapsed().as_nanos() as f64 / 2000.0;
                out.row(
                    Row::new().s("exp", "loc_index_ondisk").s("name", kv2.name()).s("insert_order", order).i("n", n as u64).n("build_s", bs)
                        .n("disk_bytes_per_obj", disk as f64 / n as f64).n("hit_ns_med", th.median).n("miss_ns_med", tm.median)
                        .n("cold_first2000_hit_ns", cold).s("cold_dropped_caches", if cold_ok { "yes" } else { "no" }),
                );
                drop(kv2);
                let _ = std::fs::remove_dir_all(&d);
            }
        }
    }
}
