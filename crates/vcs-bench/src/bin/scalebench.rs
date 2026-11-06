//! Scaling benchmark suite (scripts/scaling/, docs/research/scaling-to-trillions.md).
//!
//! Every subcommand builds a SYNTHETIC structure of N elements directly (an id table, a commit graph, an
//! AGI5 index, ...) and times the REAL reader code from the product crates on it. These are structure
//! benchmarks, not repositories. Output: one JSON object per line on stdout.
//!
//!   scalebench idx      N DIR [--packs P] [--lookups K]   pack index (VCSPIDX2) lookups, 256-bit ids
//!   scalebench cgraph   N DIR                             commit-graph decode + ancestry queries (synthetic bytes)
//!   scalebench cgbuild  N                                 commit-graph build through CommitGraphFile::extend
//!   scalebench agi5     N DIR                             AGI5 index open/find/scan/verify
//!   scalebench reach    N DIR                             gc-reach bitmap cache load/lookup
//!   scalebench pfilter  N DIR                             changed-path filter file load/lookup
//!   scalebench bloom                                      changed-path Bloom filter size/FP/cost vs changed paths
//!   scalebench manifest N DIR                             single manifest vs sharded at N zones
//!   scalebench locks    N                                 lock table at N holders (locks::apply, encode, decode)

use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::Instant;
use vcs_objects::ObjectId;
use vcs_repo::scaling_probe as probe;

// ---------------------------------------------------------------- helpers

fn status_kb(key: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1).and_then(|v| v.parse().ok()))
        })
        .unwrap_or(0)
}
fn rss() -> u64 {
    status_kb("VmRSS:")
}
fn anon() -> u64 {
    status_kb("RssAnon:")
}
fn hwm() -> u64 {
    status_kb("VmHWM:")
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        ((self.next() as u128 * n as u128) >> 64) as u64
    }
}

fn pct(v: &mut [u128], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_unstable();
    v[(((v.len() - 1) as f64) * p).round() as usize] as f64 / 1000.0
}

/// Emit one record. `ns` = per-op samples (may be empty); `extra` = already-formatted `"k":v,...`.
fn emit(comp: &str, op: &str, n: u64, ns: &mut [u128], extra: &str) {
    let (p50, p95, cnt) = (pct(ns, 0.5), pct(ns, 0.95), ns.len());
    let f = |x: f64| if x.is_nan() { "null".to_string() } else { format!("{x:.3}") };
    println!(
        "{{\"comp\":\"{comp}\",\"op\":\"{op}\",\"n\":{n},\"samples\":{cnt},\"p50_us\":{},\"p95_us\":{},\"rss_kb\":{},\"anon_kb\":{},\"hwm_kb\":{}{}{extra}}}",
        f(p50),
        f(p95),
        rss(),
        anon(),
        hwm(),
        if extra.is_empty() { "" } else { "," }
    );
    let _ = std::io::stdout().flush();
}

/// One-shot measurement (build, open, full scan): the value is the single wall time.
fn emit1(comp: &str, op: &str, n: u64, dur: std::time::Duration, extra: &str) {
    let mut v = [dur.as_nanos()];
    emit(comp, op, n, &mut v, extra);
}

fn time_each<T>(k: usize, mut f: impl FnMut(usize) -> T) -> Vec<u128> {
    (0..k)
        .map(|i| {
            let t = Instant::now();
            std::hint::black_box(f(i));
            t.elapsed().as_nanos()
        })
        .collect()
}

fn arg(args: &[String], name: &str, default: u64) -> u64 {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8)
}

fn id_of(n: u64) -> ObjectId {
    ObjectId::from_bytes(*blake3::hash(&n.to_le_bytes()).as_bytes())
}

fn absent_id(rng: &mut Rng) -> [u8; 32] {
    let mut b = [0u8; 32];
    for k in 0..4 {
        b[k * 8..k * 8 + 8].copy_from_slice(&rng.next().to_be_bytes());
    }
    b
}

// ---------------------------------------------------------------- idx

const IDX_HDR: usize = 8 + 4 + 8 + 32 + 1024;
const ENTRY: usize = 44;

/// Write one VCSPIDX2 file holding global indexes `p, p+P, p+2P, ...` of `total` sorted ids.
fn write_idx(path: &Path, total: u64, packs: u64, p: u64) -> u64 {
    let cnt = (total - p).div_ceil(packs);
    let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path).unwrap();
    let len = IDX_HDR as u64 + cnt * ENTRY as u64 + 32;
    f.set_len(len).unwrap();
    let nt = threads() as u64;
    let per = cnt.div_ceil(nt);
    let hists: Vec<[u32; 256]> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..nt)
            .map(|t| {
                let f = &f;
                s.spawn(move || {
                    let mut h = [0u32; 256];
                    let (a, b) = (t * per, ((t + 1) * per).min(cnt));
                    let mut buf = Vec::with_capacity(ENTRY * 65536);
                    let mut j = a;
                    while j < b {
                        buf.clear();
                        let e = (j + 65536).min(b);
                        for jj in j..e {
                            let id = probe::sorted_id(jj * packs + p, total);
                            h[id[0] as usize] += 1;
                            buf.extend_from_slice(&id);
                            buf.extend_from_slice(&(64 + jj * 100).to_le_bytes());
                            buf.extend_from_slice(&100u32.to_le_bytes());
                        }
                        f.write_all_at(&buf, IDX_HDR as u64 + j * ENTRY as u64).unwrap();
                        j = e;
                    }
                    h
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut counts = [0u32; 256];
    for h in &hists {
        for i in 0..256 {
            counts[i] += h[i];
        }
    }
    let mut head = Vec::with_capacity(IDX_HDR);
    head.extend_from_slice(b"VCSPIDX2");
    head.extend_from_slice(&(cnt as u32).to_le_bytes());
    head.extend_from_slice(&(64 + cnt * 100 + 32 + 1).to_le_bytes()); // pack_len
    head.extend_from_slice(&[7u8; 32]); // pack checksum (unused by lookups)
    let mut run = 0u32;
    for c in counts {
        run += c;
        head.extend_from_slice(&run.to_le_bytes());
    }
    assert_eq!(head.len(), IDX_HDR);
    f.write_all_at(&head, 0).unwrap();
    // BLAKE3 trailer over everything before it, streamed
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 64 << 20];
    let mut off = 0u64;
    while off < len - 32 {
        let n = ((len - 32 - off) as usize).min(buf.len());
        f.read_exact_at(&mut buf[..n], off).unwrap();
        h.update(&buf[..n]);
        off += n as u64;
    }
    f.write_all_at(h.finalize().as_bytes(), len - 32).unwrap();
    len
}

fn cmd_idx(args: &[String]) {
    let n: u64 = args[0].parse().unwrap();
    let dir = PathBuf::from(&args[1]);
    let packs = arg(args, "--packs", 1);
    let k = arg(args, "--lookups", 20000) as usize;
    let verify = arg(args, "--verify", 0) == 1;
    std::fs::create_dir_all(&dir).unwrap();
    let t = Instant::now();
    let mut total_bytes = 0;
    let paths: Vec<PathBuf> = (0..packs).map(|p| dir.join(format!("p{p}.idx"))).collect();
    for (p, path) in paths.iter().enumerate() {
        if !path.exists() {
            total_bytes += write_idx(path, n, packs, p as u64);
        } else {
            total_bytes += std::fs::metadata(path).unwrap().len();
        }
    }
    let build = t.elapsed();
    emit1(
        "pack_index",
        "build",
        n,
        build,
        &format!("\"bytes\":{total_bytes},\"bytes_per_object\":{:.3},\"packs\":{packs}", total_bytes as f64 / n as f64),
    );
    // drop page cache effects are left to the OS: file is hot (just written). Open lazily like the store does.
    let t = Instant::now();
    let idxs: Vec<vcs_store::pack::PackIndex> = paths.iter().map(|p| vcs_store::pack::PackIndex::open_lazy(p).unwrap()).collect();
    emit1("pack_index", "open", n, t.elapsed(), &format!("\"packs\":{packs}"));
    let find = |id: &ObjectId| idxs.iter().find_map(|ix| ix.lookup(id).unwrap());
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let present = |rng: &mut Rng| ObjectId::from_bytes(probe::sorted_id(rng.below(n), n));
    // cold phase: the first lookups land in untouched index buckets
    let mut cold = time_each(512, |_| {
        let id = present(&mut rng);
        assert!(find(&id).is_some());
    });
    emit("pack_index", "lookup_hit_cold", n, &mut cold, &format!("\"packs\":{packs}"));
    let mut hit = time_each(k, |_| {
        let id = present(&mut rng);
        assert!(find(&id).is_some());
    });
    emit("pack_index", "lookup_hit", n, &mut hit, &format!("\"packs\":{packs}"));
    let mut miss = time_each(k, |_| {
        let id = ObjectId::from_bytes(absent_id(&mut rng));
        assert!(find(&id).is_none());
    });
    emit("pack_index", "lookup_miss_random", n, &mut miss, &format!("\"packs\":{packs}"));
    // 256-bit near collisions: same first 24 bytes as a stored id, last 8 bytes different
    let mut near = 0u64;
    let mut ncol = time_each(k, |_| {
        let mut b = probe::sorted_id(rng.below(n), n);
        b[24..].copy_from_slice(&rng.next().to_be_bytes());
        if find(&ObjectId::from_bytes(b)).is_some() {
            near += 1;
        }
    });
    emit("pack_index", "lookup_miss_near_collision", n, &mut ncol, &format!("\"false_hits\":{near},\"packs\":{packs}"));
    if verify {
        let t = Instant::now();
        let r = idxs[0].verify_full();
        emit1("pack_index", "verify_full", n / packs, t.elapsed(), &format!("\"ok\":{}", r.is_ok()));
    }
}

// ---------------------------------------------------------------- cgraph

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}
fn modinv(a: u64, m: u64) -> u64 {
    let (mut r0, mut r1, mut s0, mut s1) = (m as i128, a as i128, 0i128, 1i128);
    while r1 != 0 {
        let q = r0 / r1;
        (r0, r1) = (r1, r0 - q * r1);
        (s0, s1) = (s1, s0 - q * s1);
    }
    s0.rem_euclid(m as i128) as u64
}

const SIDE_LEN: u64 = 50;
const FORKS: [u64; 4] = [1, 100, 10_000, 1_000_000];

struct Shape {
    n: u64,
    m: u64, // main chain length
    a: u64,
    ainv: u64,
    b: u64,
}
impl Shape {
    fn new(n: u64) -> Shape {
        let m = n - FORKS.len() as u64 * SIDE_LEN;
        let mut a = 0x9E37_79B9_7F4A_7C15u64 % n | 1;
        while gcd(a, n) != 1 {
            a += 2;
        }
        Shape { n, m, a, ainv: modinv(a, n), b: n / 3 }
    }
    fn rank(&self, k: u64) -> u64 {
        ((k as u128 * self.a as u128 + self.b as u128) % self.n as u128) as u64
    }
    fn commit(&self, r: u64) -> u64 {
        (((r + self.n - self.b % self.n) as u128 * self.ainv as u128) % self.n as u128) as u64
    }
    fn fork_of(&self, c: usize) -> u64 {
        self.m - 1 - FORKS[c].min(self.m - 1)
    }
    /// (parents as commit indexes, generation) of commit `k`
    fn node(&self, k: u64) -> ([u64; 2], usize, u32) {
        if k < self.m {
            if k == 0 {
                ([0, 0], 0, 1)
            } else if k >= 10 && k % 10 == 0 {
                ([k - 1, k - 5], 2, (k + 1) as u32)
            } else {
                ([k - 1, 0], 1, (k + 1) as u32)
            }
        } else {
            let (c, t) = (((k - self.m) / SIDE_LEN) as usize, (k - self.m) % SIDE_LEN);
            let fork = self.fork_of(c);
            if t == 0 {
                ([fork, 0], 1, (fork + 2) as u32)
            } else {
                ([k - 1, 0], 1, (fork + 2 + t) as u32)
            }
        }
    }
}

fn cg_bytes(sh: &Shape) -> Vec<u8> {
    let n = sh.n;
    let mut v = Vec::with_capacity(41 + n as usize * 53);
    v.extend_from_slice(b"AGCG");
    v.push(1);
    v.extend_from_slice(&(n as u32).to_le_bytes());
    for r in 0..n {
        let k = sh.commit(r);
        let (ps, np, gen) = sh.node(k);
        v.extend_from_slice(&probe::sorted_id(r, n));
        v.extend_from_slice(&(1_700_000_000 + k * 60).to_le_bytes());
        v.extend_from_slice(&gen.to_le_bytes());
        v.push(np as u8);
        for p in &ps[..np] {
            v.extend_from_slice(&(sh.rank(*p) as u32).to_le_bytes());
        }
    }
    let sum = blake3::hash(&v);
    v.extend_from_slice(sum.as_bytes());
    v
}

fn cmd_cgraph(args: &[String]) {
    use vcs_merge::CommitGraphFile;
    let n: u64 = args[0].parse().unwrap();
    let dir = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(n > 1_000, "cgraph needs N > 1000");
    let sh = Shape::new(n);
    let path = dir.join(format!("cg{n}.agcg"));
    let t = Instant::now();
    if !path.exists() {
        std::fs::write(&path, cg_bytes(&sh)).unwrap();
    }
    let bytes_len = std::fs::metadata(&path).unwrap().len();
    emit1("commit_graph", "synth_build", n, t.elapsed(), &format!("\"bytes\":{bytes_len},\"bytes_per_commit\":{:.2}", bytes_len as f64 / n as f64));
    let rss0 = rss();
    let raw = std::fs::read(&path).unwrap();
    let t = Instant::now();
    let g = CommitGraphFile::decode(&raw).expect("decode");
    emit1("commit_graph", "decode_validate", n, t.elapsed(), &format!("\"mem_delta_kb\":{}", rss().saturating_sub(rss0)));
    drop(raw);
    emit1("commit_graph", "resident", n, std::time::Duration::ZERO, &format!("\"mem_after_decode_kb\":{},\"mem_bytes_per_commit\":{:.2}", rss(), rss().saturating_sub(rss0) as f64 * 1024.0 / n as f64));
    let id_at = |k: u64| ObjectId::from_bytes(probe::sorted_id(sh.rank(k), n));
    let mut rng = Rng(0xDEAD_BEEF_1234_5678);
    let mut lk = time_each(20000, |_| {
        let k = rng.below(sh.m);
        assert!(g.index_of(&id_at(k)).is_some());
    });
    emit("commit_graph", "index_of", n, &mut lk, "");
    let tip = id_at(sh.m - 1);
    let rep = if n <= 10_000_000 { 31 } else { 9 };
    for (c, d) in FORKS.iter().enumerate() {
        let side_tip = id_at(sh.m + (c as u64 + 1) * SIDE_LEN - 1);
        let want = id_at(sh.fork_of(c));
        let mut ok = true;
        let mut mb = time_each(rep, |_| {
            let r = g.merge_bases_indexed(&tip, &side_tip).expect("in graph");
            ok &= r == vec![want];
        });
        emit("commit_graph", &format!("merge_base_fork_distance_{d}"), n, &mut mb, &format!("\"correct\":{ok},\"fork_distance\":{d}"));
    }
    let hide = id_at(sh.m - 1 - 1000);
    let mut rg = time_each(rep, |_| g.range_ids(&[tip], &[hide]).map(|v| v.len()));
    emit("commit_graph", "range_ids_1000", n, &mut rg, "");
    let mut cr = time_each(rep, |_| g.count_reachable_not_from(&tip, &hide));
    emit("commit_graph", "count_reachable_not_from_1000", n, &mut cr, "");
    let t = Instant::now();
    let enc = g.encode();
    emit1("commit_graph", "encode", n, t.elapsed(), &format!("\"bytes\":{}", enc.len()));
}

fn cmd_cgbuild(args: &[String]) {
    use vcs_merge::CommitGraphFile;
    let n: u64 = args[0].parse().unwrap();
    let ids: Vec<ObjectId> = (0..n).map(id_of).collect();
    let mut by: std::collections::HashMap<ObjectId, u64> = std::collections::HashMap::with_capacity(n as usize);
    for (i, id) in ids.iter().enumerate() {
        by.insert(*id, i as u64);
    }
    let t = Instant::now();
    let g = CommitGraphFile::default()
        .extend(&[ids[n as usize - 1]], |id| {
            let i = by[id];
            Ok((if i == 0 { vec![] } else { vec![ids[i as usize - 1]] }, 1_700_000_000 + i))
        })
        .expect("extend");
    let d = t.elapsed();
    emit1("commit_graph", "build_extend", n, d, &format!("\"commits_per_s\":{:.0},\"len\":{}", n as f64 / d.as_secs_f64(), g.len()));
    // incremental: add 1 commit on top of a graph of n (what each commit/push does when no batching)
    let extra = id_of(n + 1);
    let tip = ids[n as usize - 1];
    let mut inc = time_each(5, |_| g.extend(&[extra], |id| if *id == extra { Ok((vec![tip], 2_000_000_000)) } else { unreachable!() }).unwrap().len());
    emit("commit_graph", "extend_one_commit", n, &mut inc, "");
}

// ---------------------------------------------------------------- agi5, reach, pfilter, bloom

fn cmd_agi5(args: &[String]) {
    let n: u64 = args[0].parse().unwrap();
    let dir = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("idx{n}.agi5"));
    let t = Instant::now();
    let size = if path.exists() { std::fs::metadata(&path).unwrap().len() } else { probe::agi5_write(&path, n as usize).unwrap() };
    emit1("agi5", "synth_build", n, t.elapsed(), &format!("\"bytes\":{size},\"bytes_per_entry\":{:.2}", size as f64 / n as f64));
    let r0 = rss();
    let t = Instant::now();
    let ix = probe::Agi5::open(&path).expect("open");
    emit1("agi5", "open", n, t.elapsed(), &format!("\"mem_delta_kb\":{}", rss().saturating_sub(r0)));
    let mut rng = Rng(77);
    let mut hit = time_each(20000, |_| assert!(ix.find(&probe::agi5_path(rng.below(n), n))));
    emit("agi5", "find_hit", n, &mut hit, "");
    let mut miss = time_each(20000, |_| assert!(!ix.find(&format!("{}x/nope", probe::agi5_path(rng.below(n), n)))));
    emit("agi5", "find_miss", n, &mut miss, "");
    drop(ix);
    let ix = probe::Agi5::open(&path).expect("open");
    let t = Instant::now();
    ix.scan().expect("scan");
    let d = t.elapsed();
    emit1("agi5", "scan_all_status_pass", n, d, &format!("\"ns_per_entry\":{:.1}", d.as_nanos() as f64 / n as f64));
    drop(ix);
    let ix = probe::Agi5::open(&path).expect("open");
    let t = Instant::now();
    ix.verify_all().expect("verify");
    let d = t.elapsed();
    emit1("agi5", "verify_all", n, d, &format!("\"ns_per_entry\":{:.1}", d.as_nanos() as f64 / n as f64));
}

fn cmd_reach(args: &[String]) {
    let n: u64 = args[0].parse().unwrap();
    let dir = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("reach{n}"));
    let t = Instant::now();
    let size = if path.exists() { std::fs::metadata(&path).unwrap().len() } else { probe::reach_write(&path, n as usize, 64).unwrap() };
    emit1("reach_bitmap", "synth_build", n, t.elapsed(), &format!("\"bytes\":{size},\"bytes_per_object\":{:.3}", size as f64 / n as f64));
    let r0 = rss();
    let t = Instant::now();
    let rc = probe::Reach::load(&path).expect("load");
    emit1("reach_bitmap", "load_verify", n, t.elapsed(), &format!("\"mem_delta_kb\":{}", rss().saturating_sub(r0)));
    let mut rng = Rng(5);
    let mut q = time_each(20000, |_| assert!(rc.scanned(&probe::sorted_id(rng.below(n), n))));
    emit("reach_bitmap", "member_lookup", n, &mut q, "");
    let mut miss = time_each(20000, |_| assert!(!rc.scanned(&absent_id(&mut rng))));
    emit("reach_bitmap", "member_miss", n, &mut miss, "");
    let _ = rc.marked(&probe::sorted_id(0, n));
    let roots: Vec<[u8; 32]> = (0..256u64).map(|i| probe::sorted_id(i, n)).collect();
    let mut rw = time_each(9, |_| rc.roots_within(&roots));
    emit("reach_bitmap", "roots_within_256", n, &mut rw, "");
}

fn cmd_pfilter(args: &[String]) {
    let n: u64 = args[0].parse().unwrap();
    let dir = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("pf{n}"));
    let t = Instant::now();
    let size = if path.exists() { std::fs::metadata(&path).unwrap().len() } else { probe::pathfilter_write(&path, n as usize, 24).unwrap() };
    emit1("path_filter", "synth_build", n, t.elapsed(), &format!("\"bytes\":{size},\"bytes_per_commit\":{:.2}", size as f64 / n as f64));
    let r0 = rss();
    let t = Instant::now();
    let pf = probe::PathFilters::load(&path);
    emit1("path_filter", "load_parse", n, t.elapsed(), &format!("\"loaded\":{},\"mem_delta_kb\":{}", pf.loaded(), rss().saturating_sub(r0)));
    let mut rng = Rng(9);
    let mut q = time_each(20000, |_| assert!(pf.get(&probe::sorted_id(rng.below(n), n))));
    emit("path_filter", "lookup", n, &mut q, "");
}

fn cmd_bloom() {
    let mut rng = Rng(31337);
    for &k in &[1usize, 4, 16, 64, 128, 256, probe::BLOOM_MAX_PATHS] {
        let paths: Vec<String> = (0..k).map(|i| format!("svc{:03}/pkg{:03}/src/file{:05}.rs", i % 50, i % 17, i)).collect();
        let mut sizes = 0usize;
        let mut b = time_each(200, |_| {
            let f = probe::bloom_build(&paths);
            sizes = f.as_ref().map_or(0, |f| f.len());
            f
        });
        let f = probe::bloom_build(&paths);
        match f {
            None => emit("bloom", &format!("build_changed_{k}"), k as u64, &mut b, "\"unknown\":true"),
            Some(f) => {
                let trials = 400_000u64;
                let mut fp = 0u64;
                let mut q = time_each(trials as usize, |_| {
                    let p = format!("svc{:03}/pkg{:03}/src/other{:06}.rs", rng.below(900), rng.below(900), rng.below(1_000_000));
                    if probe::bloom_may_contain(&f, &p) {
                        fp += 1;
                    }
                });
                let mut tp = 0;
                for p in &paths {
                    if probe::bloom_may_contain(&f, p) {
                        tp += 1;
                    }
                }
                emit("bloom", &format!("build_changed_{k}"), k as u64, &mut b, &format!("\"filter_bytes\":{sizes}"));
                emit(
                    "bloom",
                    &format!("query_changed_{k}"),
                    k as u64,
                    &mut q,
                    &format!("\"false_positive_rate\":{:.6},\"false_negatives\":{},\"trials\":{trials}", fp as f64 / trials as f64, k - tp),
                );
            }
        }
    }
}

// ---------------------------------------------------------------- manifest, locks

fn synth_manifest(z: usize) -> vcs_remote::Manifest {
    let mut m = vcs_remote::Manifest { generation: 1_000_000, ..Default::default() };
    let mut n = 0u64;
    for i in 0..z {
        let zn = format!("zone-{i:07}");
        n += 1;
        m.branches.insert((zn.clone(), "main".into()), id_of(n));
        n += 1;
        m.tags.insert((zn.clone(), "v1.0".into()), id_of(n));
        m.zones.insert(zn.clone(), format!("services/{zn}"));
    }
    m
}

fn cmd_manifest(args: &[String]) {
    use vcs_remote::{Backend, FsBackend, Manifest, MemBackend};
    let z: usize = args[0].parse().unwrap();
    let dir = PathBuf::from(&args[1]);
    let reps = if z <= 10_000 { 21 } else if z <= 100_000 { 9 } else { 3 };
    let t = Instant::now();
    let m = synth_manifest(z);
    emit1("manifest", "synth_build", z as u64, t.elapsed(), "");
    let mut enc_bytes = 0;
    let mut e = time_each(reps, |_| {
        let b = m.encode();
        enc_bytes = b.len();
        b
    });
    emit("manifest", "encode", z as u64, &mut e, &format!("\"bytes\":{enc_bytes},\"bytes_per_zone\":{:.2}", enc_bytes as f64 / z as f64));
    let bytes = m.encode();
    let mut d = time_each(reps, |_| Manifest::decode(&bytes).unwrap());
    emit("manifest", "decode", z as u64, &mut d, "");
    // single-manifest commit: get, decode, advance one branch, encode, compare-and-swap
    let fs_dir = dir.join(format!("fs{z}"));
    let _ = std::fs::remove_dir_all(&fs_dir);
    std::fs::create_dir_all(&fs_dir).unwrap();
    let backends: Vec<(&str, Box<dyn Backend>)> = vec![("mem", Box::new(MemBackend::new())), ("fs", Box::new(FsBackend::open(&fs_dir).unwrap()))];
    for (name, b) in &backends {
        b.put_if_absent(vcs_remote::manifest::KEY, &bytes).unwrap();
        let mut seq = 0u64;
        let mut c = time_each(reps, |_| {
            let v = b.get(vcs_remote::manifest::KEY).unwrap().unwrap();
            let mut cur = Manifest::decode(&v.data).unwrap();
            seq += 1;
            cur.branches.insert((format!("zone-{:07}", seq as usize % z), "main".into()), id_of(10_000_000 + seq));
            cur.generation += 1;
            let out = cur.encode();
            assert!(b.put_if_match(vcs_remote::manifest::KEY, &out, &v.tag).unwrap().is_some());
        });
        emit("manifest", &format!("single_commit_cas_{name}"), z as u64, &mut c, "");
    }
    // sharded: migrate (one record per zone), then single-zone commit and full snapshot
    if z <= 300_000 {
        let sdir = dir.join(format!("sh{z}"));
        let _ = std::fs::remove_dir_all(&sdir);
        std::fs::create_dir_all(&sdir).unwrap();
        let fsb = FsBackend::open(&sdir).unwrap();
        fsb.put_if_absent(vcs_remote::manifest::KEY, &bytes).unwrap();
        let t = Instant::now();
        vcs_remote::sharded::migrate_to_sharded(&fsb).expect("migrate");
        emit1("manifest", "sharded_migrate", z as u64, t.elapsed(), "");
        let eng = vcs_remote::sharded::Engine::new(&fsb);
        let mut seq = 0u64;
        let mut cur: std::collections::BTreeMap<usize, ObjectId> = Default::default();
        let mut c = time_each(reps.max(9), |_| {
            seq += 1;
            let zi = (seq as usize * 7919) % z;
            let zn = format!("zone-{zi:07}");
            let old = cur.get(&zi).copied().unwrap_or_else(|| m.branches[&(zn.clone(), "main".to_string())]);
            let new = id_of(20_000_000 + seq);
            cur.insert(zi, new);
            let d = vcs_remote::sharded::PartDelta { heads: vec![("main".into(), Some(old), Some(new))], ..Default::default() };
            let parts = std::collections::BTreeMap::from([(zn, d)]);
            eng.commit(id_of(30_000_000 + seq), &parts).expect("commit");
        });
        emit("manifest", "sharded_commit_one_zone", z as u64, &mut c, "");
        let sreps = if z <= 10_000 { 5 } else { 2 };
        let mut s = time_each(sreps, |_| eng.snapshot().expect("snapshot").0.branches.len());
        emit("manifest", "sharded_snapshot_all_zones", z as u64, &mut s, "");
    }
}

fn cmd_locks(args: &[String]) {
    use vcs_objects::{LockOp, RefUpdate, Signature, Transaction};
    let n: usize = args[0].parse().unwrap();
    let mut m = vcs_remote::Manifest { generation: 1, ..Default::default() };
    for i in 0..n {
        m.locks.insert(
            (format!("zone-{:05}", i % 1000), format!("assets/file-{i:09}.bin")),
            vcs_remote::Lock { holder: format!("user{}", i % 5000), token: i as u64 + 1, acquired_at: 1_000, expires_at: 10_000_000_000, reason: "editing".into() },
        );
    }
    m.next_token = n as u64 + 1;
    m.time_hw = 2_000;
    let reps = if n <= 100_000 { 21 } else { 5 };
    let mut bytes = 0;
    let mut e = time_each(reps, |_| {
        let b = m.encode();
        bytes = b.len();
    });
    emit("locks", "manifest_encode", n as u64, &mut e, &format!("\"bytes\":{bytes},\"bytes_per_lock\":{:.2}", bytes as f64 / n as f64));
    let b = m.encode();
    let mut d = time_each(reps, |_| vcs_remote::Manifest::decode(&b).unwrap());
    emit("locks", "manifest_decode", n as u64, &mut d, "");
    let sig = Signature { name: "t".into(), email: "t@x".into(), timestamp: 1 };
    let mut k = 0u64;
    let mut a = time_each(reps, |_| {
        k += 1;
        let tx = Transaction {
            updates: vec![RefUpdate { zone: "zone-00001".into(), branch: "main".into(), old: None, new: id_of(1) }],
            author: sig.clone(),
            message: "m".into(),
            lock_ops: vec![LockOp::Acquire { zone: "zone-00001".into(), path: format!("assets/new-{k}.bin"), lease_ms: 60_000, reason: "x".into() }],
        };
        let mut newm = m.clone();
        vcs_remote::locks::apply(&m, &mut newm, &tx, "alice", 3_000, 100, None, &mut |_| Ok(Default::default())).expect("apply");
        newm.locks.len()
    });
    emit("locks", "acquire_validate_apply", n as u64, &mut a, "");
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let rest = &a[1.min(a.len())..];
    match a.first().map(String::as_str) {
        Some("idx") => cmd_idx(rest),
        Some("cgraph") => cmd_cgraph(rest),
        Some("cgbuild") => cmd_cgbuild(rest),
        Some("agi5") => cmd_agi5(rest),
        Some("reach") => cmd_reach(rest),
        Some("pfilter") => cmd_pfilter(rest),
        Some("bloom") => cmd_bloom(),
        Some("manifest") => cmd_manifest(rest),
        Some("locks") => cmd_locks(rest),
        _ => {
            eprintln!("usage: scalebench idx|cgraph|cgbuild|agi5|reach|pfilter|bloom|manifest|locks ...");
            std::process::exit(2);
        }
    }
}
