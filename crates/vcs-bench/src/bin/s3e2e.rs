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
    Signature {
        name: "b".into(),
        email: "b@x".into(),
        timestamp: 1,
    }
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
    let num = |name: &str, d: usize| {
        a.iter()
            .position(|x| x == name)
            .and_then(|i| a.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let (trials, files, commits) = (num("--trials", 5), num("--files", 300), num("--commits", 50));
    let base = S3Config::from_env(endpoint, bucket).expect("AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY not set");
    if a.iter().any(|x| x == "--create-bucket") {
        S3Backend::new_unchecked(base.clone()).unwrap().ensure_bucket().unwrap();
    }
    let t0 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    println!("endpoint {endpoint} trials {trials} files {files} commits {commits} (a local server, not a cloud)");

    // ---- bulk throughput: one big PUT, one big GET, parallel ranged GETs (decides parallel ranges / multipart) ----
    if a.iter().any(|x| x == "--throughput") {
        let mb = num("--mb", 64);
        let mut c2 = base.clone();
        c2.prefix = format!("{}e2e-{t0}-bulk/", std::env::var("S3E2E_PREFIX").unwrap_or_default());
        let bk = std::sync::Arc::new(S3Backend::connect(c2).unwrap());
        let big: Vec<u8> = noise(7, mb << 20).into_bytes();
        for rep in 0..3 {
            let s = Instant::now();
            bk.put_if_absent(&format!("big{rep}"), &big).unwrap();
            let el = s.elapsed().as_secs_f64();
            println!(
                "PUT {mb} MiB single request (rep {rep}): {:.0} ms, {:.0} MB/s",
                el * 1e3,
                (mb << 20) as f64 / el / 1e6
            );
        }
        assert!(
            !bk.put_if_absent("big0", &big[..1024]).unwrap(),
            "create-only must refuse an existing multipart object"
        );
        {
            use sha2::{Digest, Sha256};
            let v = bk.get("big0").unwrap().unwrap();
            assert_eq!(
                Sha256::digest(&v.data),
                Sha256::digest(&big),
                "uploaded object differs from the source bytes"
            );
            println!("create-only refusal and byte-for-byte readback of the {mb} MiB object: ok");
        }
        for rep in 0..3 {
            let s = Instant::now();
            let v = bk.get("big0").unwrap().unwrap();
            let el = s.elapsed().as_secs_f64();
            println!(
                "GET {mb} MiB single request (rep {rep}): {:.0} ms, {:.0} MB/s ({} B)",
                el * 1e3,
                v.data.len() as f64 / el / 1e6,
                v.data.len()
            );
        }
        for (threads, part) in [(1usize, 8usize), (4, 8), (8, 8), (16, 4), (16, 8), (32, 2)] {
            let total = mb << 20;
            let (part, parts) = (part << 20, total / (part << 20));
            let mut ms = vec![];
            for _ in 0..3 {
                let next = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let s = Instant::now();
                let hs: Vec<_> = (0..threads)
                    .map(|_| {
                        let (b, n) = (bk.clone(), next.clone());
                        std::thread::spawn(move || loop {
                            let i = n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if i >= parts {
                                break;
                            }
                            b.get_range("big0", (i * part) as u64, part as u64).unwrap().unwrap();
                        })
                    })
                    .collect();
                for h in hs {
                    h.join().unwrap();
                }
                ms.push(s.elapsed().as_secs_f64() * 1e3);
            }
            ms.sort_by(|x, y| x.partial_cmp(y).unwrap());
            println!(
                "GET {mb} MiB as {} ranges of {} MiB on {threads} threads: median {:.0} ms ({:.0} MB/s), min {:.0}",
                parts,
                part >> 20,
                ms[1],
                total as f64 / ms[1] / 1e3,
                ms[0]
            );
        }
        // parallel PUTs of distinct 8 MiB objects (what multipart parts would look like)
        for threads in [1usize, 4, 8, 16] {
            let part = &big[..8 << 20];
            let n = 16usize;
            let next = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let s = Instant::now();
            let hs: Vec<_> = (0..threads)
                .map(|_| {
                    let (b, nx, p) = (bk.clone(), next.clone(), part.to_vec());
                    std::thread::spawn(move || loop {
                        let i = nx.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= n {
                            break;
                        }
                        b.put_if_absent(&format!("par{threads}-{i}"), &p).unwrap();
                    })
                })
                .collect();
            for h in hs {
                h.join().unwrap();
            }
            let el = s.elapsed().as_secs_f64();
            println!(
                "PUT {n} x 8 MiB objects on {threads} threads: {:.0} ms, {:.0} MB/s",
                el * 1e3,
                (n * (8 << 20)) as f64 / el / 1e6
            );
        }
        return;
    }

    // ---- request storm on ONE prefix: does S3 answer 503 SlowDown, and how does the client back off? ----
    if a.iter().any(|x| x == "--storm") {
        let (threads, secs) = (num("--threads", 128), num("--secs", 15));
        let mut c2 = base.clone();
        c2.prefix = format!("{}e2e-{t0}-storm/", std::env::var("S3E2E_PREFIX").unwrap_or_default());
        let bk = std::sync::Arc::new(S3Backend::connect(c2).unwrap());
        let body = vec![1u8; 256];
        let (ok, bad) = (
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        );
        let msgs = std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<String, u64>::new()));
        let end = Instant::now() + Duration::from_secs(secs as u64);
        let s = Instant::now();
        let hs: Vec<_> = (0..threads)
            .map(|t| {
                let (b, ok, bad, msgs, body) = (bk.clone(), ok.clone(), bad.clone(), msgs.clone(), body.clone());
                std::thread::spawn(move || {
                    let mut i = 0u64;
                    while Instant::now() < end {
                        i += 1;
                        match b.put_if_absent(&format!("s/{t}/{i}"), &body) {
                            Ok(_) => ok.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                            Err(e) => {
                                *msgs
                                    .lock()
                                    .unwrap()
                                    .entry(format!("{e}").chars().skip_while(|c| *c != ':').take(40).collect())
                                    .or_default() += 1;
                                bad.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                            }
                        };
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        let el = s.elapsed().as_secs_f64();
        let (ok, bad) = (
            ok.load(std::sync::atomic::Ordering::Relaxed),
            bad.load(std::sync::atomic::Ordering::Relaxed),
        );
        println!(
            "storm: {threads} threads, {el:.1} s, one prefix: {ok} ok, {bad} failed (503/timeouts), {:.0} req/s",
            (ok + bad) as f64 / el
        );
        for (m, n) in msgs.lock().unwrap().iter() {
            println!("  failure kind {m:?}: {n}");
        }
        return;
    }

    // ---- repository workloads: publish, incremental publish, clone, fetch ----
    let (mut p_ms, mut p_rq, mut i_ms, mut i_rq, mut c_ms, mut c_rq, mut f_ms, mut f_rq) =
        (vec![], vec![], vec![], vec![], vec![], vec![], vec![], vec![]);
    for t in 0..trials {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = base.clone();
        cfg.prefix = format!("{}e2e-{t0}-{t}/", std::env::var("S3E2E_PREFIX").unwrap_or_default());
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
    cfg.prefix = format!("{}e2e-{t0}-small/", std::env::var("S3E2E_PREFIX").unwrap_or_default());
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
    let mut p_put = time(&mut |i| {
        let _ = pooled.put_if_absent(&format!("p/{i}"), &body).unwrap();
    });
    report("GET 1 KiB, pooled connection", &mut g_pooled, "ms");
    report("GET 1 KiB, new connection each time", &mut g_fresh, "ms");
    let (mp, mf) = (stats(&mut g_pooled).0, stats(&mut g_fresh).0);
    println!(
        "connection setup share of a fresh-connection GET: {:.0}% (p50 {mf:.2} ms vs {mp:.2} ms pooled)",
        (mf - mp) / mf * 100.0
    );
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
        println!(
            "GET 1 KiB x{} threads: {:.0} req/s ({} requests in {:.2} s)",
            threads,
            (threads * per) as f64 / el,
            threads * per,
            el
        );
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
        println!(
            "sha256 of {label} body: {:.1} us ({:.0} MB/s) [{acc}]",
            per * 1e6,
            n as f64 / per / 1e6
        );
    }
    let big = vec![9u8; 1 << 20];
    let mut put_big = time(&mut |i| {
        if i < 40 {
            let _ = shared.put_if_absent(&format!("big/{i}"), &big).unwrap();
        }
    });
    put_big.truncate(40);
    report("PUT 1 MiB create-only, pooled (includes body sha256)", &mut put_big, "ms");
    let _ = Duration::from_secs(0);
}
