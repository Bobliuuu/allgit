//! Provider contract + latency benchmark for an S3-compatible bucket (storage research, issue #187).
//!
//! Cost-guarded: aborts when the request or byte budget is exceeded. Credentials come from the
//! standard AWS_* environment variables and are never printed.
//!
//! usage: s3bench <endpoint> <bucket> <region> <out.json> [--max-requests N] [--max-mb N] [--ca-pem FILE] [--create-bucket] [--no-probe]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vcs_remote::Backend;
use vcs_store_s3::{S3Backend, S3Config};

fn pct(v: &mut Vec<f64>, p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() {
        return 0.0;
    }
    v[(((v.len() - 1) as f64) * p).round() as usize]
}

fn stats(label: &str, mut ms: Vec<f64>) -> String {
    let n = ms.len();
    let p50 = pct(&mut ms, 0.5);
    let p95 = pct(&mut ms, 0.95);
    let p99 = pct(&mut ms, 0.99);
    let max = ms.last().copied().unwrap_or(0.0);
    eprintln!("{label:<34} n={n:<4} p50={p50:8.1} p95={p95:8.1} p99={p99:8.1} max={max:8.1} ms");
    format!(r#"{{"op":"{label}","n":{n},"p50_ms":{p50:.2},"p95_ms":{p95:.2},"p99_ms":{p99:.2},"max_ms":{max:.2}}}"#)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        eprintln!("usage: s3bench <endpoint> <bucket> <region> <out.json> [--max-requests N] [--max-mb N] [--ca-pem FILE] [--create-bucket] [--no-probe]");
        std::process::exit(2);
    }
    let (endpoint, bucket, region, out) = (&a[1], &a[2], &a[3], &a[4]);
    let flag = |name: &str, d: u64| {
        a.iter()
            .position(|x| x == name)
            .and_then(|i| a.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let max_req = flag("--max-requests", 4000);
    let max_bytes = flag("--max-mb", 300) * 1_000_000;
    let mut cfg = S3Config::from_env(endpoint, bucket).expect("AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY not set");
    cfg.region = region.clone();
    cfg.prefix = format!("bench-{}/", std::process::id());
    cfg.timeout = Duration::from_secs(60);
    let strarg = |name: &str| a.iter().position(|x| x == name).and_then(|i| a.get(i + 1)).cloned();
    if let Some(path) = strarg("--ca-pem") {
        cfg.ca_pem = Some(std::fs::read(&path).expect("read --ca-pem file"));
    }
    if a.iter().any(|x| x == "--create-bucket") {
        // Local emulators only: create the bucket first (real runs use a pre-created private bucket).
        S3Backend::new_unchecked(cfg.clone())
            .expect("client")
            .ensure_bucket()
            .expect("create bucket");
    }
    // `--no-probe` skips the product's conditional-write probe so a provider that would be refused
    // can still be characterised (research only; the product never does this).
    let b = Arc::new(if a.iter().any(|x| x == "--no-probe") {
        S3Backend::new_unchecked(cfg).expect("client")
    } else {
        S3Backend::connect(cfg).expect("connect (bucket must exist; provider must pass the conditional-write probe)")
    });
    let uploaded = Arc::new(AtomicU64::new(0));
    let budget = |b: &S3Backend, up: &AtomicU64| {
        if b.requests() > max_req || b.bytes_read() + up.load(Ordering::Relaxed) > max_bytes {
            eprintln!(
                "BUDGET EXCEEDED: {} requests, {} bytes read, {} uploaded; aborting",
                b.requests(),
                b.bytes_read(),
                up.load(Ordering::Relaxed)
            );
            std::process::exit(3);
        }
    };
    let mut rows: Vec<String> = Vec::new();
    let mut checks: Vec<String> = Vec::new();
    let mut check = |name: &str, ok: bool, note: &str| {
        eprintln!("[{}] {name}: {note}", if ok { "ok" } else { "FAIL" });
        checks.push(format!(r#"{{"check":"{name}","ok":{ok},"note":"{}"}}"#, note.replace('"', "'")));
    };

    // ---- contract checks (the questions in provider-comparison.md) ----
    let k = "contract/create-only";
    check(
        "create-only first put",
        b.put_if_absent(k, b"v1").unwrap(),
        "put_if_absent on a new key returns true",
    );
    check(
        "create-only second put refused",
        !b.put_if_absent(k, b"v2").unwrap(),
        "If-None-Match: * on existing key returns false",
    );
    let v = b.get(k).unwrap().unwrap();
    check("create-only left value intact", v.data == b"v1", "value still v1");
    let t1 = v.tag.clone();
    let t2 = b.put_if_match(k, b"v2", &t1).unwrap();
    check(
        "CAS success with current tag",
        t2.is_some(),
        "If-Match with the current ETag replaces the object",
    );
    check(
        "CAS fail with stale tag",
        b.put_if_match(k, b"v3", &t1).unwrap().is_none(),
        "If-Match with a stale ETag is refused",
    );
    check(
        "CAS on missing key fails",
        b.put_if_match("contract/missing", b"x", "\"abc\"").unwrap().is_none(),
        "If-Match on a missing key is refused",
    );
    // ABA hazard: ETag is the content MD5 for simple PUTs, so writing v1 again reuses the old tag.
    let k2 = "contract/aba";
    b.put_if_absent(k2, b"AAA").unwrap();
    let ea = b.get(k2).unwrap().unwrap().tag;
    let eb = b.put_if_match(k2, b"BBB", &ea).unwrap().expect("cas");
    let ec = b.put_if_match(k2, b"AAA", &eb).unwrap().expect("cas back");
    let aba = ec == ea;
    check(
        "ETag equals content MD5 (ABA hazard)",
        aba,
        &format!("tag after A->B->A {} the first tag", if aba { "equals" } else { "differs from" }),
    );
    if aba {
        let stale_ok = b.put_if_match(k2, b"CCC", &ea).unwrap().is_some();
        check(
            "stale-tag CAS succeeds after A-B-A",
            stale_ok,
            "a writer that read A before B still wins: manifest must embed an epoch",
        );
    }
    let big: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    b.put_if_absent("contract/range", &big).unwrap();
    let r = b.get_range("contract/range", 100, 50).unwrap().unwrap();
    check("range read exact", r == big[100..150], "bytes 100..150 returned");
    let past = b.get_range("contract/range", 5000, 10).unwrap();
    check(
        "range past end",
        past.as_ref().map(|v| v.is_empty()).unwrap_or(true),
        "offset past end returns empty or none",
    );
    let lst = b.list("contract/").unwrap();
    check("list under prefix", lst.len() >= 3, &format!("{} keys listed", lst.len()));
    budget(&b, &uploaded);

    // ---- sequential latency by object size ----
    for (label, size, n) in [("1KiB", 1024usize, 40usize), ("64KiB", 64 * 1024, 40), ("1MiB", 1024 * 1024, 20)] {
        let payload: Vec<u8> = (0..size).map(|i| (i * 7 % 256) as u8).collect();
        let (mut put, mut get, mut head, mut rng, mut cas_fail) = (vec![], vec![], vec![], vec![], vec![]);
        for i in 0..n {
            let key = format!("lat/{label}/{i}");
            let t = Instant::now();
            b.put_if_absent(&key, &payload).unwrap();
            put.push(t.elapsed().as_secs_f64() * 1e3);
            uploaded.fetch_add(size as u64, Ordering::Relaxed);
            let t = Instant::now();
            let g = b.get(&key).unwrap().unwrap();
            get.push(t.elapsed().as_secs_f64() * 1e3);
            assert_eq!(g.data.len(), size);
            let t = Instant::now();
            b.exists(&key).unwrap();
            head.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            b.get_range(&key, 0, 512.min(size as u64)).unwrap().unwrap();
            rng.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            let _ = b.put_if_absent(&key, b"x").unwrap(); // refused create-only (no data change)
            cas_fail.push(t.elapsed().as_secs_f64() * 1e3);
            budget(&b, &uploaded);
        }
        rows.push(stats(&format!("put_if_absent {label}"), put));
        rows.push(stats(&format!("get {label}"), get));
        rows.push(stats(&format!("head {label}"), head));
        rows.push(stats(&format!("get_range 512B {label}"), rng));
        rows.push(stats(&format!("put_if_absent refused {label}"), cas_fail));
    }
    // manifest-style CAS loop on one key (single-writer commit rate)
    {
        let k = "lat/manifest";
        b.put_if_absent(k, &vec![0u8; 8192]).unwrap();
        let mut tag = b.get(k).unwrap().unwrap().tag;
        let mut ms = vec![];
        for i in 0..30u8 {
            let t = Instant::now();
            tag = b.put_if_match(k, &vec![i; 8192], &tag).unwrap().expect("cas");
            ms.push(t.elapsed().as_secs_f64() * 1e3);
        }
        rows.push(stats("manifest CAS 8KiB (sequential)", ms));
        budget(&b, &uploaded);
    }
    // ---- GET throughput by concurrency on one 64 KiB object ----
    let mut conc = Vec::new();
    for threads in [1usize, 4, 16, 32] {
        let per = 40usize;
        let t0 = Instant::now();
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                let b = b.clone();
                std::thread::spawn(move || {
                    let mut v = vec![];
                    for _ in 0..per {
                        let t = Instant::now();
                        b.get("lat/64KiB/0").unwrap().unwrap();
                        v.push(t.elapsed().as_secs_f64() * 1e3);
                    }
                    v
                })
            })
            .collect();
        let mut all: Vec<f64> = hs.into_iter().flat_map(|h| h.join().unwrap()).collect();
        let secs = t0.elapsed().as_secs_f64();
        let rps = (threads * per) as f64 / secs;
        eprintln!("concurrent GET 64KiB x{threads:<3} {rps:8.1} req/s");
        conc.push(format!(
            r#"{{"threads":{threads},"requests":{},"seconds":{secs:.3},"req_per_s":{rps:.1},"p50_ms":{:.1},"p99_ms":{:.1}}}"#,
            threads * per,
            pct(&mut all, 0.5),
            pct(&mut all, 0.99)
        ));
        budget(&b, &uploaded);
    }
    // ---- parallel CAS contention: how many of N concurrent writers win a round ----
    let kc = "lat/contend";
    b.put_if_absent(kc, b"0").unwrap();
    let mut wins = vec![];
    for round in 0..5 {
        let t0 = b.get(kc).unwrap().unwrap().tag;
        let hs: Vec<_> = (0..8)
            .map(|i| {
                let (b, t0) = (b.clone(), t0.clone());
                std::thread::spawn(move || b.put_if_match(kc, format!("{round}-{i}").as_bytes(), &t0).unwrap().is_some())
            })
            .collect();
        let w = hs.into_iter().map(|h| h.join().unwrap()).filter(|x| *x).count();
        wins.push(w);
    }
    check(
        "CAS contention: exactly one winner per round",
        wins.iter().all(|&w| w == 1),
        &format!("winners per round of 8 writers: {wins:?}"),
    );
    budget(&b, &uploaded);

    let json = format!(
        r#"{{"endpoint":"{endpoint}","bucket_region":"{region}","requests":{},"bytes_down":{},"bytes_up":{},"checks":[{}],"latency":[{}],"concurrency":[{}]}}"#,
        b.requests(),
        b.bytes_read(),
        uploaded.load(Ordering::Relaxed),
        checks.join(","),
        rows.join(","),
        conc.join(",")
    );
    std::fs::write(out, json).expect("write json");
    eprintln!(
        "done: {} requests, {} bytes down, {} bytes up -> {out}",
        b.requests(),
        b.bytes_read(),
        uploaded.load(Ordering::Relaxed)
    );
}
