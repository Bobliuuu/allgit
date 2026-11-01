//! Manifest scaling harness (issue #252): manifest bytes and encode/decode/validate CPU as zones,
//! branches, tags, locks and applied ids grow, plus publish latency, CAS conflicts and throughput
//! with N concurrent publishers on disjoint zones against Mem or an S3-compatible server.
//!
//! usage: manifestbench sizes
//!        manifestbench publish <mem | s3 ENDPOINT BUCKET> --zones Z --branches B --writers N --each K [--create-bucket]
//!        manifestbench fetch   <mem | s3 ENDPOINT BUCKET> --zones Z --branches B --reps R [--create-bucket]
//! S3 credentials come from AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY (never printed). Output: JSON lines.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Barrier;
use std::time::Instant;
use vcs_objects::*;
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore};
use vcs_store_s3::{S3Backend, S3Config};

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

fn id_of(n: u64) -> ObjectId {
    ObjectId::from_bytes(*blake3::hash(&n.to_le_bytes()).as_bytes())
}

/// Synthetic manifest: `z` zones x `b` branches, one tag per zone if `tags`, `locks` locks, `applied` ids.
fn synth(z: usize, b: usize, tags: bool, locks: usize, applied: usize) -> Manifest {
    let mut m = Manifest {
        generation: 1_000_000,
        ..Default::default()
    };
    let mut n = 0u64;
    for i in 0..z {
        let zn = format!("zone-{i:05}");
        for j in 0..b {
            n += 1;
            m.branches.insert(
                (zn.clone(), if j == 0 { "main".into() } else { format!("feature/topic-{j}") }),
                id_of(n),
            );
        }
        m.zones.insert(zn.clone(), format!("services/{zn}"));
        if tags {
            n += 1;
            m.tags.insert((zn.clone(), "v1.0".into()), id_of(n));
        }
    }
    if locks > 0 {
        m.next_token = locks as u64 + 1;
        m.time_hw = 1;
        for i in 0..locks {
            let zn = format!("zone-{:05}", i * z.max(1) / locks);
            m.locks.insert(
                (zn, format!("assets/model-{i}.bin")),
                Lock {
                    holder: "alice".into(),
                    token: i as u64 + 1,
                    acquired_at: 1,
                    expires_at: 2,
                    reason: "editing".into(),
                },
            );
        }
    }
    for i in 0..applied {
        m.applied.push(id_of(1 << 40 | i as u64));
    }
    m
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}
fn pct(v: &[f64], p: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[((s.len() as f64 - 1.0) * p).round() as usize]
}

fn time_us<T>(reps: usize, mut f: impl FnMut() -> T) -> f64 {
    let mut v = Vec::new();
    for _ in 0..reps {
        let t = Instant::now();
        std::hint::black_box(f());
        v.push(t.elapsed().as_secs_f64() * 1e6);
    }
    median(&mut v)
}

fn sizes() {
    for &z in &[10usize, 100, 1000, 10000] {
        for &b in &[1usize, 5] {
            for &(tags, locks, applied) in &[(false, 0usize, 0usize), (true, z / 10, 0), (true, z / 10, 4096)] {
                let m = synth(z, b, tags, locks, applied);
                let (l, c) = (m.encode_legacy(), m.encode_compact());
                let reps = if z >= 10000 { 5 } else { 21 };
                let el = time_us(reps, || m.encode_legacy());
                let ec = time_us(reps, || m.encode_compact());
                let dl = time_us(reps, || Manifest::decode(&l).unwrap());
                let dc = time_us(reps, || Manifest::decode(&c).unwrap());
                assert_eq!(Manifest::decode(&c).unwrap(), m);
                println!(
                    "{{\"kind\":\"sizes\",\"zones\":{z},\"branches_per_zone\":{b},\"tags\":{},\"locks\":{locks},\"applied\":{applied},\"legacy_bytes\":{},\"compact_bytes\":{},\"encode_legacy_us\":{el:.0},\"encode_compact_us\":{ec:.0},\"decode_validate_legacy_us\":{dl:.0},\"decode_validate_compact_us\":{dc:.0},\"reps\":{reps}}}",
                    if tags { z } else { 0 },
                    l.len(),
                    c.len()
                );
            }
        }
    }
}

/// Counts CAS attempts and losses and requests; everything else delegates.
struct Counting<'a> {
    inner: &'a dyn Backend,
    cas: AtomicU64,
    lost: AtomicU64,
    reqs: AtomicU64,
}
impl Backend for Counting<'_> {
    fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
        self.reqs.fetch_add(1, Ordering::Relaxed);
        self.inner.get(k)
    }
    fn get_conditional(&self, k: &str, h: &str) -> Result<Conditional, BackendError> {
        self.reqs.fetch_add(1, Ordering::Relaxed);
        self.inner.get_conditional(k, h)
    }
    fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.reqs.fetch_add(1, Ordering::Relaxed);
        self.inner.get_range(k, o, l)
    }
    fn exists(&self, k: &str) -> Result<bool, BackendError> {
        self.reqs.fetch_add(1, Ordering::Relaxed);
        self.inner.exists(k)
    }
    fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
        self.reqs.fetch_add(1, Ordering::Relaxed);
        self.inner.put_if_absent(k, d)
    }
    fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
        self.reqs.fetch_add(1, Ordering::Relaxed);
        self.cas.fetch_add(1, Ordering::Relaxed);
        let r = self.inner.put_if_match(k, d, t)?;
        if r.is_none() {
            self.lost.fetch_add(1, Ordering::Relaxed);
        }
        Ok(r)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
        self.reqs.fetch_add(1, Ordering::Relaxed);
        self.inner.list(p)
    }
}

fn open_backend(a: &[String]) -> (Box<dyn Backend>, String) {
    let flag = |n: &str| a.iter().position(|x| x == n).and_then(|i| a.get(i + 1)).cloned();
    match a.get(2).map(String::as_str) {
        Some("mem") => (Box::new(MemBackend::new()), "mem".into()),
        Some("s3") => {
            let (ep, bucket) = (&a[3], &a[4]);
            let mut cfg = S3Config::from_env(ep, bucket).expect("AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY not set");
            cfg.prefix = format!("mbench-{}-{}/", std::process::id(), flag("--tag").unwrap_or_default());
            if a.iter().any(|x| x == "--create-bucket") {
                S3Backend::new_unchecked(cfg.clone())
                    .expect("client")
                    .ensure_bucket()
                    .expect("bucket");
            }
            (Box::new(S3Backend::connect(cfg).expect("connect")), format!("s3 {ep}"))
        }
        _ => {
            eprintln!("backend must be `mem` or `s3 ENDPOINT BUCKET`");
            std::process::exit(2)
        }
    }
}

fn num(a: &[String], n: &str, d: usize) -> usize {
    a.iter()
        .position(|x| x == n)
        .and_then(|i| a.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

fn commit(s: &MemoryStore, zone: &str, parent: Option<ObjectId>, i: usize) -> ObjectId {
    let blob = s.put_blob(format!("{zone}-{i}").as_bytes()).unwrap();
    let t = s
        .put(&Object::Tree(
            Tree::new(vec![TreeEntry {
                name: "f".into(),
                mode: EntryMode::File,
                id: blob,
            }])
            .unwrap(),
        ))
        .unwrap();
    s.put(&Object::Commit(Commit {
        zone: zone.into(),
        parents: parent.into_iter().collect(),
        root: t,
        change_id: ChangeId::from_bytes([i as u8; 16]),
        author: sig(),
        message: format!("{zone} {i}"),
    }))
    .unwrap()
}

fn seed(b: &dyn Backend, m: &Manifest) {
    assert!(
        b.put_if_absent(vcs_remote::manifest::KEY, &m.encode()).unwrap(),
        "manifest already present"
    );
}

fn publish(a: &[String]) {
    let (zones, branches, writers, each) = (
        num(a, "--zones", 100),
        num(a, "--branches", 5),
        num(a, "--writers", 4),
        num(a, "--each", 20),
    );
    let (backend, name) = open_backend(a);
    let base = synth(zones, branches, true, zones / 10, 0);
    seed(&*backend, &base);
    let manifest_bytes = base.encode().len();
    let counting = Counting {
        inner: &*backend,
        cas: 0.into(),
        lost: 0.into(),
        reqs: 0.into(),
    };
    let bar = Barrier::new(writers);
    let wall = Instant::now();
    let lat: Vec<Vec<f64>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..writers)
            .map(|w| {
                let (counting, bar) = (&counting, &bar);
                sc.spawn(move || {
                    let store = MemoryStore::new();
                    let zone = format!("writer-{w:02}");
                    let nobackoff = a.iter().any(|x| x == "--no-backoff");
                    let (mut head, mut lats) = (None, Vec::new());
                    bar.wait();
                    for i in 0..each {
                        let new = commit(&store, &zone, head, i);
                        let t = Instant::now();
                        let p = Publisher::new(
                            counting,
                            &store,
                            Transaction {
                                updates: vec![RefUpdate {
                                    zone: zone.clone(),
                                    branch: "main".into(),
                                    old: head,
                                    new,
                                }],
                                author: sig(),
                                message: "m".into(),
                                lock_ops: vec![],
                            },
                        );
                        let out = if nobackoff { p.without_backoff() } else { p }.run().expect("publish");
                        assert!(matches!(out, Outcome::Applied { .. }));
                        lats.push(t.elapsed().as_secs_f64() * 1e3);
                        head = Some(new);
                    }
                    lats
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let secs = wall.elapsed().as_secs_f64();
    let all: Vec<f64> = lat.into_iter().flatten().collect();
    let n = all.len() as f64;
    let (cas, lost, reqs) = (
        counting.cas.load(Ordering::Relaxed),
        counting.lost.load(Ordering::Relaxed),
        counting.reqs.load(Ordering::Relaxed),
    );
    println!(
        "{{\"kind\":\"publish\",\"backend\":\"{name}\",\"zones\":{zones},\"branches_per_zone\":{branches},\"manifest_bytes\":{manifest_bytes},\"backoff\":{},\"writers\":{writers},\"publishes\":{},\"wall_s\":{secs:.3},\"throughput_per_s\":{:.1},\"lat_ms_p50\":{:.2},\"lat_ms_p95\":{:.2},\"lat_ms_max\":{:.2},\"cas_attempts\":{cas},\"cas_lost\":{lost},\"cas_lost_pct\":{:.1},\"retries_per_publish\":{:.3},\"requests_per_publish\":{:.2}}}",
        !a.iter().any(|x| x == "--no-backoff"),
        all.len(),
        n / secs,
        pct(&all, 0.5),
        pct(&all, 0.95),
        pct(&all, 1.0),
        100.0 * lost as f64 / cas.max(1) as f64,
        lost as f64 / n,
        reqs as f64 / n
    );
}

/// Sharded prototype: same workload as `publish` (disjoint zones, one commit per publish), plus
/// the four object PUTs (blob, tree, commit, tx record) a real publish makes, so request counts
/// are comparable. Single-zone transactions still pay the full two-phase protocol.
#[cfg(feature = "sharded")]
fn shardpublish(a: &[String]) {
    use vcs_remote::shard::*;
    let (zones, writers, each) = (num(a, "--zones", 100), num(a, "--writers", 4), num(a, "--each", 20));
    let (backend, name) = open_backend(a);
    let counting = Counting {
        inner: &*backend,
        cas: 0.into(),
        lost: 0.into(),
        reqs: 0.into(),
    };
    let bar = Barrier::new(writers);
    let wall = Instant::now();
    let res: Vec<(Vec<f64>, u64, u64)> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..writers)
            .map(|w| {
                let (counting, bar) = (&counting, &bar);
                sc.spawn(move || {
                    let zone = format!("writer-{w:02}");
                    let mut p = ShardedPublisher::new(counting);
                    let (mut head, mut lats) = (None, Vec::new());
                    bar.wait();
                    for i in 0..each {
                        let new = id_of((w as u64) << 32 | i as u64 | 1 << 50);
                        let t = Instant::now();
                        for k in 0..4u8 {
                            counting.put_if_absent(&format!("objects/{}/{k}", new.to_hex()), &[k; 120]).unwrap();
                        }
                        p.publish_retrying(
                            &new,
                            &[ShardUpdate {
                                zone: zone.clone(),
                                branch: "main".into(),
                                old: head,
                                new,
                            }],
                            50,
                        )
                        .expect("sharded publish");
                        lats.push(t.elapsed().as_secs_f64() * 1e3);
                        head = Some(new);
                    }
                    (lats, p.cas_attempts, p.cas_lost)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let secs = wall.elapsed().as_secs_f64();
    let all: Vec<f64> = res.iter().flat_map(|r| r.0.clone()).collect();
    let (cas, lost) = res.iter().fold((0, 0), |a, r| (a.0 + r.1, a.1 + r.2));
    let n = all.len() as f64;
    println!(
        "{{\"kind\":\"shardpublish\",\"backend\":\"{name}\",\"zones\":{zones},\"writers\":{writers},\"publishes\":{},\"wall_s\":{secs:.3},\"throughput_per_s\":{:.1},\"lat_ms_p50\":{:.2},\"lat_ms_p95\":{:.2},\"lat_ms_max\":{:.2},\"cas_attempts\":{cas},\"cas_lost\":{lost},\"cas_lost_pct\":{:.1},\"requests_per_publish\":{:.2}}}",
        all.len(),
        n / secs,
        pct(&all, 0.5),
        pct(&all, 0.95),
        pct(&all, 1.0),
        100.0 * lost as f64 / cas.max(1) as f64,
        counting.reqs.load(Ordering::Relaxed) as f64 / n
    );
}

fn fetch(a: &[String]) {
    let (zones, branches, reps) = (num(a, "--zones", 100), num(a, "--branches", 5), num(a, "--reps", 15));
    let (backend, name) = open_backend(a);
    let base = synth(zones, branches, true, zones / 10, 4096);
    seed(&*backend, &base);
    let mut full = Vec::new();
    let mut cond = Vec::new();
    let mut tag = String::new();
    for _ in 0..reps {
        let t = Instant::now();
        let (m, tg) = load_manifest(&*backend).unwrap();
        full.push(t.elapsed().as_secs_f64() * 1e3);
        assert_eq!(m.generation, base.generation);
        tag = tg.unwrap();
        let t = Instant::now();
        let (m2, _) = refresh_manifest(&*backend, Some((m, tag.clone()))).unwrap();
        cond.push(t.elapsed().as_secs_f64() * 1e3);
        assert_eq!(m2.generation, base.generation);
    }
    println!(
        "{{\"kind\":\"fetch\",\"backend\":\"{name}\",\"zones\":{zones},\"branches_per_zone\":{branches},\"manifest_bytes\":{},\"reps\":{reps},\"get_full_ms_p50\":{:.2},\"get_full_ms_p95\":{:.2},\"conditional_ms_p50\":{:.2},\"conditional_ms_p95\":{:.2},\"etag\":\"{}\"}}",
        base.encode().len(),
        pct(&full, 0.5),
        pct(&full, 0.95),
        pct(&cond, 0.5),
        pct(&cond, 0.95),
        tag.len()
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("sizes") => sizes(),
        Some("publish") => publish(&a),
        Some("fetch") => fetch(&a),
        #[cfg(feature = "sharded")]
        Some("shardpublish") => shardpublish(&a),
        _ => {
            eprintln!("usage: manifestbench sizes | publish <backend> ... | fetch <backend> ...");
            std::process::exit(2)
        }
    }
}
