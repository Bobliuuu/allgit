//! Experiment J2: cache backends (memory, local files, SQLite, redb, RocksDB) under an object-sized workload.
//! usage: backendbench [--n 100000] [--threads 1,8,30] [--out f.jsonl]; LAB_DIR selects where files go.
use std::collections::HashMap;
use std::hint::black_box;
use std::sync::{Arc, RwLock};
use vcs_storage_lab::ondisk::*;
use vcs_storage_lab::util::*;

#[global_allocator]
static A: Counting = Counting;

struct Mem(Vec<RwLock<HashMap<Vec<u8>, Vec<u8>>>>);
impl Mem {
    fn new() -> Mem {
        Mem((0..64).map(|_| RwLock::new(HashMap::new())).collect())
    }
    fn shard(&self, k: &[u8]) -> &RwLock<HashMap<Vec<u8>, Vec<u8>>> {
        &self.0[k[0] as usize % 64]
    }
}
impl Kv for Mem {
    fn name(&self) -> &'static str {
        "memory_sharded_hashmap"
    }
    fn put_batch(&self, items: &[(Vec<u8>, Vec<u8>)]) {
        for (k, v) in items {
            self.shard(k).write().unwrap().insert(k.clone(), v.clone());
        }
    }
    fn get(&self, k: &[u8]) -> Option<Vec<u8>> {
        self.shard(k).read().unwrap().get(k).cloned()
    }
}

fn size_of(i: u64) -> usize {
    let h = (i.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 11) as f64;
    let u1 = ((h as u64 & 0xfffff) as f64 + 1.0) / 1048577.0;
    let u2 = (((h as u64) >> 20 & 0xfffff) as f64) / 1048576.0;
    let g = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
    (2000.0 * (1.2 * g).exp()).clamp(64.0, 512_000.0) as usize
}

fn main() {
    let n: usize = arg_or("--n", 100_000);
    let threads: Vec<usize> = arg("--threads").unwrap_or("1,8,30".into()).split(',').map(|s| s.parse().unwrap()).collect();
    let mut out = Out::new(arg("--out"));
    out.row(Row::new().s("exp", "env").s("env", &env_json()).i("n", n as u64));
    let dir = std::path::PathBuf::from(std::env::var("LAB_DIR").unwrap_or_else(|_| "/tmp".into()));
    let ids = random_ids(n, 21);
    let mut rng = Rng::new(5);
    let payload_bytes: u64 = (0..n as u64).map(|i| size_of(i) as u64).sum();
    let data: Vec<(Vec<u8>, usize)> = ids.iter().enumerate().map(|(i, id)| (id.to_vec(), size_of(i as u64))).collect();
    let mut bytes_buf = vec![0u8; 512_000];
    rng.fill(&mut bytes_buf);
    let mk_val = |i: usize, sz: usize| -> Vec<u8> {
        let mut v = bytes_buf[(i * 31) % 1000..(i * 31) % 1000 + sz].to_vec(); // incompressible, deterministic
        v[0] = i as u8;
        v
    };
    type Opener = Box<dyn Fn(&std::path::Path) -> Arc<dyn Kv>>;
    let mut backends: Vec<(&str, Opener)> = vec![
        ("mem", Box::new(|_| Arc::new(Mem::new()))),
        ("fs", Box::new(|p| Arc::new(FsLoose::open(p)))),
    ];
    #[cfg(feature = "sqlite")]
    backends.push(("sqlite", Box::new(|p| Arc::new(Sqlite::open(p)))));
    #[cfg(feature = "redb")]
    backends.push(("redb", Box::new(|p| Arc::new(Redb::open(p)))));
    #[cfg(feature = "rocks")]
    backends.push(("rocks", Box::new(|p| Arc::new(Rocks::open(p)))));
    let probe: Vec<usize> = {
        let mut r = Rng::new(77);
        (0..200_000.min(n * 2)).map(|_| (r.next_u64() % n as u64) as usize).collect()
    };
    let miss_ids = random_ids(50_000, 4242);
    for (label, open) in &backends {
        let d = dir.join(format!("be_{label}"));
        let _ = std::fs::remove_dir_all(&d);
        let kv = open(&d);
        let t = std::time::Instant::now();
        for ch in (0..n).collect::<Vec<_>>().chunks(2000) {
            let items: Vec<(Vec<u8>, Vec<u8>)> = ch.iter().map(|&i| (data[i].0.clone(), mk_val(i, data[i].1))).collect();
            kv.put_batch(&items);
        }
        kv.finish();
        let put_s = t.elapsed().as_secs_f64();
        let disk = if *label == "mem" { 0 } else { dir_size(&d) };
        // single-thread latency distribution (warm)
        let mut lat: Vec<u64> = Vec::with_capacity(probe.len());
        let mut wrong = 0;
        for &i in &probe {
            let t = std::time::Instant::now();
            let v = kv.get(&data[i].0);
            lat.push(t.elapsed().as_nanos() as u64);
            if v.as_ref().map(|v| v.len()) != Some(data[i].1) {
                wrong += 1;
            }
        }
        lat.sort_unstable();
        let mut miss_lat: Vec<u64> = vec![];
        for id in &miss_ids {
            let t = std::time::Instant::now();
            black_box(kv.contains(id));
            miss_lat.push(t.elapsed().as_nanos() as u64);
        }
        miss_lat.sort_unstable();
        let mut row = Row::new().s("exp", "backend").s("backend", kv.name()).i("n", n as u64).i("payload_bytes", payload_bytes)
            .n("put_s", put_s).n("put_objs_per_s", n as f64 / put_s).n("put_MBps", payload_bytes as f64 / 1e6 / put_s)
            .n("disk_bytes_over_payload", if disk == 0 { f64::NAN } else { disk as f64 / payload_bytes as f64 })
            .n("get_warm_p50_ns", lat[lat.len() / 2] as f64).n("get_warm_p99_ns", lat[lat.len() * 99 / 100] as f64)
            .n("absent_key_contains_p50_ns", miss_lat[miss_lat.len() / 2] as f64).i("wrong_values", wrong);
        for &th in &threads {
            let t = std::time::Instant::now();
            std::thread::scope(|sc| {
                for k in 0..th {
                    let kv = kv.clone();
                    let (probe, data) = (&probe, &data);
                    sc.spawn(move || {
                        let mut s = 0usize;
                        for (j, &i) in probe.iter().enumerate() {
                            if j % th == k {
                                s += kv.get(&data[i].0).map(|v| v.len()).unwrap_or(0);
                            }
                        }
                        black_box(s);
                    });
                }
            });
            row = row.n(&format!("get_warm_kops_per_s_{th}threads"), probe.len() as f64 / t.elapsed().as_secs_f64() / 1e3);
        }
        // cold: drop caches, reopen, first 3000 random gets (memory backend has no cold state)
        if *label != "mem" {
            drop(kv);
            let ok = drop_caches();
            let t0 = std::time::Instant::now();
            let kv2 = open(&d);
            let open_s = t0.elapsed().as_secs_f64();
            let t = std::time::Instant::now();
            for &i in probe.iter().take(3000) {
                black_box(kv2.get(&data[i].0));
            }
            row = row.n("reopen_cold_s", open_s).n("get_cold_mean_ns", t.elapsed().as_nanos() as f64 / 3000.0).s("cold_dropped_caches", if ok { "yes" } else { "no" });
        }
        out.row(row);
        let _ = std::fs::remove_dir_all(&d);
    }
}
