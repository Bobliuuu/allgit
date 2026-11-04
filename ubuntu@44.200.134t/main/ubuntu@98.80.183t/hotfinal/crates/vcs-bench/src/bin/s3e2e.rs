//! #242/#243: publish / fetch / clone of a real repository through `S3Backend` against a LOCAL S3-compatible
//! server (RustFS, MinIO, ...; never a cloud account), with request counts, plus the small-request numbers:
//! p50 latency of a pooled 1 KiB GET against a fresh connection per request (the connection-setup share),
//! 16-thread GET throughput, and what SigV4 costs on the client (sha256 of the body, HMAC chain).
//!
//! usage: s3e2e <endpoint> <bucket> [--trials N] [--create-bucket] [--files N] [--commits N]
//! credentials: AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY. Output: one line per measurement (median, min, max).
use std::time::{Duration, Instant};
use vcs_objects::Signature;
use vcs_remote::Backend;
use vcs_repo::Repo;
use vcs_store_s3::{S3Backend, S3Config};

fn sig() -> Signature {
    Signature { name: "b".into(), email: "b@x".into(), timestamp: 1 }
}
fn noise(seed: u64, n: usize) -> String {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (b'a' + (x % 26) as u8) as char
        })
        .collect()
}
fn stats(v: &mut [f64]) -> (f64, f64, f64) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (v[v.len() / 2], v[0], v[v.len() - 1])
}
fn report(label: &str, v: &mut [f64], unit: &str) {
    let (m, lo, hi) = stats(v);
    println!("{label:<44} median {m:>9.2} {unit}  min {lo:>9.2}  max {hi:>9.2}  (n={})", v.len());
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!("usage: s3e2e <endpoint> <bucket> [--trials N] [--create-bucket] [--files N] [--commits N]");
        std::process::exit(2);
    }
    let (endpoint, bucket) = (&a[1], &a[2]);
    let num = |name: &str, d: usize| a.iter().position(|x| x == name).and_then(|i| a.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(d);
    let (trials, files, commits) = (num("--trials", 5), num("--files", 300), num("--commits", 50));
    let base = S3Config::from_env(endpoint, bucket).expect("AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY not set");
    if a.iter().any(|x| x == "--create-bucket") {
        S3Backend::new_unchecked(base.clone()).unwrap().ensure_bucket().unwrap();
    }
    let t0 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    println!("endpoint {endpoint} trials {trials} files {files} commits {commits} (a local server, not a cloud)");

    // ---- repository workloads: publish, incremental publish, clone, fetch ----
    let (mut p_ms, mut p_rq, mut i_ms, mut i_rq, mut c_ms, mut c_rq, mut f_ms, mut f_rq) = (vec![], vec![], vec![], vec![], vec![], vec![], vec![], vec![]);
    for t in 0..trials {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = base.clone();
        cfg.prefix = format!("e2e-{t0}-{t}/");
        let remote = S3Backend::connect(cfg.clone()).expect("connect");
        let r = Repo::init(&dir.path().join("a")).unwrap();
        r.create_zone("src", "src").unwrap();
        let w = |i: usize, v: usize| {
            let p = r.worktree().join(format!("src/d{}/f{i}.rs", i % 10));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, format!("// {i} v{v}\n{}", noise((i * 7919 + v) as u64, 3000))).unwrap();
        };
        for i in 0..files {
            w(i, 0);
        }
        r.add(&["".into()]).unwrap();
        r.commit("c0", sig()).unwrap();
        for c in 1..commits {
            w(c % files, c);
            r.add(&["".into()]).unwrap();
            r.commit(&format!("c{c}"), sig()).unwrap();
        }
        let (n0, s) = (remote.requests(), Instant::now());
        r.publish(&remote, None, sig()).unwrap();
        p_ms.push(s.elapsed().as_secs_f64() * 1e3);
        p_rq.push((remote.requests() - n0) as f64);
        // one-file change published on top
        w(1, 999);
        r.add(&["".into()]).unwrap();
        r.commit("one file", sig()).unwrap();
        let (n0, s) = (remote.requests(), Instant::now());
        r.publish(&remote, None, sig()).unwrap();
        i_ms.push(s.elapsed().as_secs_f64() * 1e3);
        i_rq.push((remote.requests() - n0) as f64);
        // clone with a fresh client (fresh connection pool, as a new process would)
        let cl = S3Backend::connect(cfg.clone()).unwrap();
        let (n0, s) = (cl.requests(), Instant::now());
        let b = Repo::clone_from(&cl, "s3", &dir.path().join("b"), None).unwrap();
        c_ms.push(s.elapsed().as_secs_f64() * 1e3);
        c_rq.push((cl.requests() - n0) as f64);
        // fetch after one more remote commit
        w(2, 1000);
        r.add(&["".into()]).unwrap();
        r.commit("again", sig()).unwrap();
        r.publish(&remote, None, sig()).unwrap();
        let (n0, s) = (cl.requests(), Instant::now());
        b.fetch(&cl, None).unwrap();
        f_ms.push(s.elapsed().as_secs_f64() * 1e3);
        f_rq.push((cl.requests() - n0) as f64);
    }
    report("publish (initial, all commits)", &mut p_ms, "ms");
    report("  requests", &mut p_rq, "req");
    report("publish (one file on top)", &mut i_ms, "ms");
    report("  requests", &mut i_rq, "req");
    report("clone (fresh client)", &mut c_ms, "ms");
    report("  requests", &mut c_rq, "req");
    report("fetch (one new commit)", &mut f_ms, "ms");
    report("  requests", &mut f_rq, "req");

    // ---- small requests: pooled vs a fresh connection each time ----
    let mut cfg = base.clone();
    cfg.prefix = format!("e2e-{t0}-small/");
    let pooled = S3Backend::connect(cfg.clone()).unwrap();
    let body = vec![7u8; 1024];
    for i in 0..50 {
        pooled.put_if_absent(&format!("k/{i}"), &body).unwrap();
    }
    let time = |f: &mut dyn FnMut(usize)| -> Vec<f64> {
        (0..200)
            .map(|i| {
                let s = Instant::now();
                f(i);
                s.elapsed().as_secs_f64() * 1e3
            })
            .collect()
    };
    let _ = time(&mut |i| drop(pooled.get(&format!("k/{}", i % 50)))); // warm
    let mut g_pooled = time(&mut |i| drop(pooled.get(&format!("k/{}", i % 50)).unwrap()));
    let mut g_fresh = time(&mut |i| {
        let c = S3Backend::new_unchecked(cfg.clone()).unwrap(); // new agent: new TCP connection
        drop(c.get(&format!("k/{}", i % 50)).unwrap())
    });
    let mut p_put = time(&mut |i| drop(pooled.put_if_absent(&format!("p/{i}"), &body).unwrap()));
    report("GET 1 KiB, pooled connection", &mut g_pooled, "ms");
    report("GET 1 KiB, new connection each time", &mut g_fresh, "ms");
    let (mp, mf) = (stats(&mut g_pooled).0, stats(&mut g_fresh).0);
    println!("connection setup share of a fresh-connection GET: {:.0}% (p50 {mf:.2} ms vs {mp:.2} ms pooled)", (mf - mp) / mf * 100.0);
    report("PUT 1 KiB create-only, pooled", &mut p_put, "ms");

    // ---- 16-thread GET throughput ----
    let shared = std::sync::Arc::new(pooled);
    for threads in [1usize, 16] {
        let per = 400;
        let s = Instant::now();
        let hs: Vec<_> = (0..threads)
            .map(|t| {
                let b = shared.clone();
                std::thread::spawn(move || {
                    for i in 0..per {
                        b.get(&format!("k/{}", (i + t) % 50)).unwrap();
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        let el = s.elapsed().as_secs_f64();
        println!("GET 1 KiB x{} threads: {:.0} req/s ({} requests in {:.2} s)", threads, (threads * per) as f64 / el, threads * per, el);
    }

    // ---- what SigV4 costs on the client ----
    use sha2::{Digest, Sha256};
    for (label, n) in [("1 KiB", 1024usize), ("1 MiB", 1 << 20), ("16 MiB", 16 << 20)] {
        let data = vec![0x5au8; n];
        let reps = (64 << 20) / n.max(1) + 1;
        let s = Instant::now();
        let mut acc = 0u8;
        for _ in 0..reps {
            acc ^= Sha256::digest(&data)[0];
        }
        let per = s.elapsed().as_secs_f64() / reps as f64;
        println!("sha256 of {label} body: {:.1} us ({:.0} MB/s) [{acc}]", per * 1e6, n as f64 / per / 1e6);
    }
    let big = vec![9u8; 1 << 20];
    let mut put_big = time(&mut |i| {
        if i < 40 {
            drop(shared.put_if_absent(&format!("big/{i}"), &big).unwrap())
        }
    });
    put_big.truncate(40);
    report("PUT 1 MiB create-only, pooled (includes body sha256)", &mut put_big, "ms");
    let _ = Duration::from_secs(0);
}
