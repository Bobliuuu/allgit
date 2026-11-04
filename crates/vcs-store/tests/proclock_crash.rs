//! Crash tests for the local process lock: a child takes the lock and is SIGKILLed.
#![cfg(unix)]
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use vcs_store::proclock::{lock_timeout, read_owner, try_lock};

const ENV: &str = "PROCLOCK_CHILD_PATH";

/// Re-exec this test binary as a child that takes the lock, prints READY and sleeps forever.
#[test]
fn child_holder() {
    let Ok(p) = std::env::var(ENV) else { return };
    let _l = try_lock(std::path::Path::new(&p)).unwrap().expect("child lock");
    println!("READY");
    std::thread::sleep(Duration::from_secs(600));
}

fn spawn_holder(path: &std::path::Path) -> Child {
    let mut c = Command::new(std::env::current_exe().unwrap())
        .args(["child_holder", "--exact", "--nocapture", "--test-threads=1"])
        .env(ENV, path)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut r = BufReader::new(c.stdout.take().unwrap());
    let mut l = String::new();
    while r.read_line(&mut l).unwrap() > 0 {
        if l.contains("READY") {
            return c;
        }
        l.clear();
    }
    panic!("child never became ready");
}

#[test]
fn sigkilled_holder_does_not_wedge() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("lock");
    let mut c = spawn_holder(&p);
    assert!(try_lock(&p).unwrap().is_none(), "live holder excludes us");
    let (pid, host, _) = read_owner(&p).unwrap();
    assert_eq!(pid, c.id());
    assert!(!host.is_empty());
    c.kill().unwrap(); // SIGKILL
    c.wait().unwrap();
    let t = Instant::now();
    let l = lock_timeout(&p, Duration::from_secs(5)).unwrap();
    assert!(l.is_some(), "lock must be free after SIGKILL");
    assert!(t.elapsed() < Duration::from_secs(2));
}

/// PID reuse: a lock file that names a live, unrelated pid (ours) is NOT held; ownership is the
/// kernel lock, not the recorded pid.
#[test]
fn recorded_pid_that_is_alive_does_not_hold_lock() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("lock");
    std::fs::write(&p, format!("pid {}\nhost other\nstart 1\n", std::process::id())).unwrap();
    assert!(try_lock(&p).unwrap().is_some());
}

#[test]
fn concurrent_contenders_are_mutually_exclusive() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("lock");
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicI32::new(0));
    let hs: Vec<_> = (0..8)
        .map(|_| {
            let (p, c) = (p.clone(), counter.clone());
            std::thread::spawn(move || {
                for _ in 0..50 {
                    let _l = lock_timeout(&p, Duration::from_secs(20)).unwrap().expect("lock");
                    assert_eq!(c.fetch_add(1, std::sync::atomic::Ordering::SeqCst), 0, "two holders");
                    std::thread::yield_now();
                    c.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
}
