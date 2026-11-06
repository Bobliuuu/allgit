//! The gc lease on a backend with no same-host kernel lock (an HTTP server or S3, here a directory backend that hides
//! its flock): a live holder keeps the lock across many leases by heartbeat, and a holder that is SIGKILLed frees the
//! lock after one short lease instead of after hours (monorepo stress run, item 4 of "What broke").
#![cfg(unix)]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use vcs_remote::backend::{BackendError, Versioned};
use vcs_remote::gc::{acquire_for_test, hold_lock_for_test, RemoteGcOptions};
use vcs_remote::{Backend, FsBackend};

/// A directory backend that, like HTTP and S3, cannot tell whether the holder of a lease is alive.
struct NoFlock(FsBackend);

impl Backend for NoFlock {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        self.0.get(key)
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.0.get_range(key, offset, len)
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        self.0.exists(key)
    }
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        self.0.put_if_absent(key, data)
    }
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        self.0.put_if_match(key, data, tag)
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        self.0.list(prefix)
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        self.0.delete(key)
    }
    fn supports_gc(&self) -> bool {
        self.0.supports_gc()
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        self.0.now_ms()
    }
    fn modified_ms(&self, key: &str) -> Result<Option<u64>, BackendError> {
        self.0.modified_ms(key)
    }
    fn touch(&self, key: &str) -> Result<bool, BackendError> {
        self.0.touch(key)
    }
    // local_flock: the default, Unsupported
}

fn open(root: &std::path::Path) -> NoFlock {
    NoFlock(FsBackend::open(root).unwrap())
}

#[test]
fn the_default_lease_is_minutes_not_hours() {
    let ttl = RemoteGcOptions::default().lock_ttl_ms;
    assert!(ttl <= 15 * 60 * 1000, "gc lease {ttl} ms");
    assert!(vcs_remote::compact::CompactOptions::default().lock_ttl_ms <= 15 * 60 * 1000);
}

#[test]
fn a_live_holder_keeps_the_lock_across_many_leases() {
    let d = tempfile::tempdir().unwrap();
    let b = open(d.path());
    const TTL: u64 = 400;
    hold_lock_for_test(&b, "alive", TTL, || {
        // five leases pass; without the heartbeat the lock would have expired after the first
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(200));
            let now = b.now_ms().unwrap();
            assert!(acquire_for_test(&b, now).is_err(), "a rival took a lock whose holder is alive");
        }
    })
    .unwrap();
    // released on drop
    assert!(acquire_for_test(&b, b.now_ms().unwrap()).is_ok());
}

const ENV: &str = "LEASE_CHILD_ROOT";

/// Child: take the lock with a 1.5 s lease, heartbeat it, announce, then hang until SIGKILLed.
#[test]
fn child_lease_holder() {
    let Ok(root) = std::env::var(ENV) else { return };
    let b = open(std::path::Path::new(&root));
    hold_lock_for_test(&b, "doomed-gc", 1500, || {
        println!("READY");
        std::thread::sleep(Duration::from_secs(600));
    })
    .unwrap();
}

#[test]
fn a_killed_holder_frees_the_lock_after_one_lease() {
    use std::io::{BufRead, BufReader};
    let d = tempfile::tempdir().unwrap();
    let b = open(d.path());
    let mut c = Command::new(std::env::current_exe().unwrap())
        .args(["child_lease_holder", "--exact", "--nocapture", "--test-threads=1"])
        .env(ENV, d.path())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut r = BufReader::new(c.stdout.take().unwrap());
    let mut l = String::new();
    while r.read_line(&mut l).unwrap() > 0 && !l.contains("READY") {
        l.clear();
    }
    // alive and beating: refused, even a few leases later
    std::thread::sleep(Duration::from_millis(2000));
    assert!(acquire_for_test(&b, b.now_ms().unwrap()).is_err(), "live holder lost its lock");
    c.kill().unwrap();
    c.wait().unwrap();
    // killed: no kernel lock on this backend to say so, so the lock stays until the lease runs out ...
    let t0 = Instant::now();
    assert!(
        acquire_for_test(&b, b.now_ms().unwrap()).is_err(),
        "the lease cannot be stale the moment the holder dies"
    );
    // ... which is one short lease, not hours
    loop {
        if acquire_for_test(&b, b.now_ms().unwrap()).is_ok() {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "lock still held {:?} after the holder was killed",
            t0.elapsed()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        t0.elapsed() < Duration::from_millis(3500),
        "recovery took {:?}, a lease is 1.5 s",
        t0.elapsed()
    );
}
