//! kill -9 of a process holding / mid-way through FsBackend CAS writes must not wedge the store.
#![cfg(unix)]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use vcs_remote::{Backend, FsBackend};

const ENV: &str = "CAS_CHILD_ROOT";

/// Child: hammer manifest CAS forever (so SIGKILL lands inside lock / write / rename at random).
#[test]
fn child_writer() {
    let Ok(root) = std::env::var(ENV) else { return };
    let b = FsBackend::open(root).unwrap();
    println!("READY");
    loop {
        let cur = b.get("manifest").unwrap().unwrap();
        let n: u64 = String::from_utf8_lossy(&cur.data).parse().unwrap();
        let _ = b.put_if_match("manifest", (n + 1).to_string().as_bytes(), &cur.tag);
    }
}

fn spawn(root: &std::path::Path) -> std::process::Child {
    let mut c = Command::new(std::env::current_exe().unwrap())
        .args(["child_writer", "--exact", "--nocapture", "--test-threads=1"])
        .env(ENV, root)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::{BufRead, BufReader};
    let mut r = BufReader::new(c.stdout.take().unwrap());
    let mut l = String::new();
    while r.read_line(&mut l).unwrap() > 0 {
        if l.contains("READY") {
            std::mem::forget(r);
            return c;
        }
        l.clear();
    }
    panic!("no ready");
}

#[test]
fn sigkill_mid_manifest_write_never_wedges_or_corrupts() {
    let d = tempfile::tempdir().unwrap();
    let b = FsBackend::open(d.path()).unwrap();
    assert!(b.put_if_absent("manifest", b"0").unwrap());
    let mut last = 0u64;
    for round in 0..15 {
        let mut c = spawn(d.path());
        std::thread::sleep(Duration::from_millis(30 + (round * 7) % 40));
        c.kill().unwrap();
        c.wait().unwrap();
        // a fresh writer proceeds promptly and the manifest is intact and monotone
        let t = Instant::now();
        let cur = b.get("manifest").unwrap().unwrap();
        let n: u64 = String::from_utf8(cur.data.clone())
            .expect("manifest intact")
            .parse()
            .expect("manifest intact");
        assert!(n >= last, "no data loss: {n} < {last}");
        let tag = b
            .put_if_match("manifest", (n + 1).to_string().as_bytes(), &cur.tag)
            .expect("write after kill -9")
            .expect("cas wins: no other writer");
        assert!(!tag.is_empty());
        assert!(t.elapsed() < Duration::from_secs(5), "bounded recovery");
        last = n + 1;
    }
    assert!(last > 0);
}

#[test]
fn concurrent_cas_writers_lose_no_increments() {
    let d = tempfile::tempdir().unwrap();
    let b = std::sync::Arc::new(FsBackend::open(d.path()).unwrap());
    b.put_if_absent("manifest", b"0").unwrap();
    let hs: Vec<_> = (0..6)
        .map(|_| {
            let b = b.clone();
            std::thread::spawn(move || {
                let mut won = 0;
                while won < 40 {
                    let cur = b.get("manifest").unwrap().unwrap();
                    let n: u64 = String::from_utf8_lossy(&cur.data).parse().unwrap();
                    if b.put_if_match("manifest", (n + 1).to_string().as_bytes(), &cur.tag)
                        .unwrap()
                        .is_some()
                    {
                        won += 1;
                    }
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    assert_eq!(b.get("manifest").unwrap().unwrap().data, b"240");
}

const GC_ENV: &str = "GC_CHILD_ROOT";

/// Child: take the gc flock and publish a 6 h gc/lock lease, then hang (it gets SIGKILLed).
#[test]
fn child_gc_holder() {
    let Ok(root) = std::env::var(GC_ENV) else { return };
    let b = FsBackend::open(root).unwrap();
    let vcs_remote::LocalFlock::Acquired(_l) = b.local_flock("gc").unwrap() else {
        panic!("flock")
    };
    let exp = b.now_ms().unwrap() + 6 * 3600 * 1000;
    b.put_if_absent("gc/lock", format!("dead-gc {exp}").as_bytes()).unwrap();
    println!("READY");
    std::thread::sleep(Duration::from_secs(600));
}

#[test]
fn sigkilled_gc_does_not_block_gc_for_the_lease_ttl() {
    use std::io::{BufRead, BufReader};
    let d = tempfile::tempdir().unwrap();
    let b = FsBackend::open(d.path()).unwrap();
    let mut c = Command::new(std::env::current_exe().unwrap())
        .args(["child_gc_holder", "--exact", "--nocapture", "--test-threads=1"])
        .env(GC_ENV, d.path())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut r = BufReader::new(c.stdout.take().unwrap());
    let mut l = String::new();
    while r.read_line(&mut l).unwrap() > 0 && !l.contains("READY") {
        l.clear();
    }
    let now = b.now_ms().unwrap();
    // live holder: refused
    assert!(vcs_remote::gc::acquire_for_test(&b, now).is_err());
    c.kill().unwrap();
    c.wait().unwrap();
    // dead holder, lease still "valid" for 6 h: taken over at once
    assert!(vcs_remote::gc::acquire_for_test(&b, now).is_ok());
}
