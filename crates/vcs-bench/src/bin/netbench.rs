//! Remote I/O benchmark against a LOCAL emulator or `vcs-server` (never a real cloud account).
//!
//! usage: netbench s3 <endpoint> <bucket> [--create-bucket] [--objs N] [--rounds R]
//!        netbench http <url> <token> [--objs N] [--rounds R]
//! S3 credentials come from AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY (the emulator's own, never printed).
//!
//! Phases (each reports median wall ms over R rounds and requests per round):
//!   small_rw    200 x (create-only PUT of 1 KB, GET)      -> connection reuse for latency-bound ops
//!   cas_chain   100 sequential If-Match PUTs on one key   -> the publish hot path
//!   fetch_many  N objects spread over one pack, far apart -> one ranged GET each; VCS_REMOTE_PARALLEL
//!                                                            sets the concurrency (ignored by older builds)
use std::time::{Duration, Instant};
use vcs_objects::{id_of_encoded, Object};
use vcs_remote::{fetch_many, Backend, CountingBackend};
use vcs_store::pack::write_pack;

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: netbench s3 <endpoint> <bucket> | http <url> <token>  [--objs N] [--rounds R] [--create-bucket]");
        std::process::exit(2);
    }
    let flag = |n: &str, d: usize| a.iter().position(|x| x == n).and_then(|i| a.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(d);
    let (objs_n, rounds) = (flag("--objs", 64), flag("--rounds", 5));
    let uniq = format!("nb{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
    let b: Box<dyn Backend> = match a[1].as_str() {
        "s3" => {
            let mut cfg = vcs_store_s3::S3Config::from_env(&a[2], &a[3]).expect("AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY not set");
            cfg.prefix = format!("{uniq}/");
            cfg.timeout = Duration::from_secs(60);
            if a.iter().any(|x| x == "--create-bucket") {
                vcs_store_s3::S3Backend::new_unchecked(cfg.clone()).unwrap().ensure_bucket().unwrap();
            }
            Box::new(vcs_store_s3::S3Backend::connect(cfg).expect("connect"))
        }
        "http" => Box::new(vcs_server::HttpBackend::new(&a[2], &a[3]).expect("client")),
        _ => panic!("s3 or http"),
    };
    let cb = CountingBackend::new(b.as_ref());
    let mut report = |name: &str, f: &dyn Fn(usize)| {
        let (mut ms, mut reqs) = (vec![], vec![]);
        for r in 0..rounds {
            let r0 = cb.stats().requests;
            let t = Instant::now();
            f(r);
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            reqs.push((cb.stats().requests - r0) as f64);
        }
        println!("{name:<12} median_ms={:9.1} requests/round={:6.0} rounds={rounds}", median(&mut ms), median(&mut reqs));
    };
    report("small_rw", &|r| {
        for i in 0..200 {
            let k = format!("sm/{uniq}/{r}/{i}");
            assert!(cb.put_if_absent(&k, &noise(i as u64, 1024)).unwrap());
            assert!(cb.get(&k).unwrap().is_some());
        }
    });
    report("cas_chain", &|r| {
        let k = format!("cas/{uniq}/{r}");
        cb.put_if_absent(&k, b"0").unwrap();
        let mut tag = cb.get(&k).unwrap().unwrap().tag;
        for i in 1..=100 {
            tag = cb.put_if_match(&k, i.to_string().as_bytes(), &tag).unwrap().expect("no contention");
        }
    });
    // one pack of 4*N 40 KB objects; the wanted ones are every 4th, so each needs its own ranged GET
    let objs: Vec<(vcs_objects::ObjectId, Vec<u8>)> = (0..(objs_n * 4) as u64)
        .map(|i| {
            let e = Object::Blob(noise(i + 7, 40_000)).encode();
            (id_of_encoded(&e), e)
        })
        .collect();
    let ids: Vec<_> = objs.iter().step_by(4).map(|o| o.0).collect();
    let dir = tempfile::tempdir().unwrap();
    let pk = write_pack(&dir.path().join("pack"), dir.path(), objs).unwrap();
    cb.put_if_absent(&format!("packs/{uniq}.pack"), &std::fs::read(&pk.pack_path).unwrap()).unwrap();
    cb.put_if_absent(&format!("packs/{uniq}.idx"), &std::fs::read(pk.pack_path.with_extension("idx")).unwrap()).unwrap();
    report("fetch_many", &|_| {
        let got = fetch_many(&cb, &ids).unwrap();
        assert_eq!(got.len(), ids.len());
    });
}
