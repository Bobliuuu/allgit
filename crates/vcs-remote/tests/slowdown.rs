//! S3-style request-rate shaping: a backend that answers 503 SlowDown (applying nothing) once a key
//! prefix exceeds its request budget for the current 100 ms window. Many publishers through the client
//! retry layer must all finish, with every update present exactly once.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};
use vcs_objects::*;
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore};

struct SlowDown {
    inner: MemBackend,
    t0: Instant,
    per_window: u64,
    seen: Mutex<HashMap<(String, u64), u64>>,
    total: AtomicU64,
    shed: AtomicU64,
}

impl SlowDown {
    /// Prefix as S3 sees it: `objects/ab/...` is one prefix per two hex digits; everything else by first segment.
    fn prefix(key: &str) -> String {
        let mut it = key.split('/');
        match (it.next(), it.next()) {
            (Some("objects"), Some(h)) => format!("objects/{h}"),
            (Some(a), _) => a.to_owned(),
            _ => String::new(),
        }
    }
    fn gate(&self, key: &str) -> Result<(), BackendError> {
        self.total.fetch_add(1, Relaxed);
        let win = (self.t0.elapsed().as_millis() / 100) as u64;
        let mut m = self.seen.lock().unwrap();
        m.retain(|(_, w), _| *w + 1 >= win);
        let n = m.entry((Self::prefix(key), win)).or_insert(0);
        *n += 1;
        if *n > self.per_window {
            self.shed.fetch_add(1, Relaxed);
            return Err(BackendError::Unavailable("503 SlowDown".into()));
        }
        Ok(())
    }
}

impl Backend for SlowDown {
    fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
        self.gate(k)?;
        self.inner.get(k)
    }
    fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.gate(k)?;
        self.inner.get_range(k, o, l)
    }
    fn exists(&self, k: &str) -> Result<bool, BackendError> {
        self.gate(k)?;
        self.inner.exists(k)
    }
    fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
        self.gate(k)?;
        self.inner.put_if_absent(k, d)
    }
    fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
        self.gate(k)?;
        self.inner.put_if_match(k, d, t)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
        self.gate(p)?;
        self.inner.list(p)
    }
}

fn sig() -> Signature {
    Signature {
        name: "a".into(),
        email: "a".into(),
        timestamp: 1,
    }
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

/// (ok, failed, requests, shed, backend)
type RunOut = (usize, usize, u64, u64, Arc<SlowDown>);

/// `None`: one `RetryBackend` per writer thread (no shared state, the old shape). `Some(n)`: ONE `RetryBackend`
/// shared by all writers with the adaptive concurrency throttle capped at `n` (0 = shared object, throttle off).
fn run(writers: usize, each: usize, per_window: u64) -> RunOut {
    run_mode(writers, each, per_window, None).0
}

fn run_mode(writers: usize, each: usize, per_window: u64, shared: Option<usize>) -> (RunOut, f64, Option<usize>) {
    let t_start = Instant::now();
    let sd = Arc::new(SlowDown {
        inner: MemBackend::new(),
        t0: Instant::now(),
        per_window,
        seen: Mutex::new(HashMap::new()),
        total: AtomicU64::new(0),
        shed: AtomicU64::new(0),
    });
    struct Wrap(Arc<SlowDown>);
    impl Backend for Wrap {
        fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
            self.0.get(k)
        }
        fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
            self.0.get_range(k, o, l)
        }
        fn exists(&self, k: &str) -> Result<bool, BackendError> {
            self.0.exists(k)
        }
        fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
            self.0.put_if_absent(k, d)
        }
        fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
            self.0.put_if_match(k, d, t)
        }
        fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
            self.0.list(p)
        }
    }
    let policy = || RetryPolicy {
        retries: 3,
        base: Duration::from_millis(20),
        cap: Duration::from_millis(400),
    };
    let shared_b: Option<Arc<RetryBackend>> =
        shared.map(|n| Arc::new(RetryBackend::with_policy(Box::new(Wrap(sd.clone())), policy()).with_max_concurrency(n)));
    struct Sh(Arc<RetryBackend>);
    impl Backend for Sh {
        fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
            self.0.get(k)
        }
        fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
            self.0.get_range(k, o, l)
        }
        fn exists(&self, k: &str) -> Result<bool, BackendError> {
            self.0.exists(k)
        }
        fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
            self.0.put_if_absent(k, d)
        }
        fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
            self.0.put_if_match(k, d, t)
        }
        fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
            self.0.list(p)
        }
    }
    let bar = Barrier::new(writers);
    let (ok, failed): (Vec<_>, Vec<_>) = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..writers)
            .map(|w| {
                let (sd, bar, shared_b) = (sd.clone(), &bar, shared_b.clone());
                sc.spawn(move || {
                    let b: Box<dyn Backend> = match shared_b {
                        Some(sb) => Box::new(Sh(sb)),
                        None => Box::new(RetryBackend::with_policy(Box::new(Wrap(sd)), policy())),
                    };
                    let b = &*b;
                    let store = MemoryStore::new();
                    let zone = format!("w{w:03}");
                    let mut head = None;
                    let mut good = 0usize;
                    bar.wait();
                    for i in 0..each {
                        let new = commit(&store, &zone, head, i);
                        let tx = Transaction {
                            updates: vec![RefUpdate {
                                zone: zone.clone(),
                                branch: "main".into(),
                                old: head,
                                new,
                            }],
                            author: sig(),
                            message: "m".into(),
                            lock_ops: vec![],
                        };
                        match Publisher::new(b, &store, tx).run() {
                            Ok(Outcome::Applied { .. }) => {
                                good += 1;
                                head = Some(new);
                            }
                            other => {
                                eprintln!("{zone} publish {i}: {other:?}");
                                break;
                            }
                        }
                    }
                    (good, each - good)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).unzip()
    });
    let (ok, failed) = (ok.iter().sum::<usize>(), failed.iter().sum::<usize>());
    let secs = t_start.elapsed().as_secs_f64();
    let lim = shared_b.as_ref().and_then(|b| b.concurrency_limit());
    ((ok, failed, sd.total.load(Relaxed), sd.shed.load(Relaxed), sd), secs, lim)
}

#[test]
fn publishers_survive_slowdown_storms() {
    let (writers, each) = (24, 4);
    let (ok, failed, total, shed, sd) = run(writers, each, 40);
    println!("slowdown: {ok} publishes ok, {failed} failed, {shed} of {total} requests shed with 503");
    assert!(shed > 0, "the test did not exercise SlowDown");
    assert_eq!(failed, 0, "publishes gave up under SlowDown");
    let m = Manifest::decode(&sd.inner.get(manifest::KEY).unwrap().unwrap().data).unwrap();
    assert_eq!(m.generation as usize, writers * each);
}

/// Probe, not a gate: how hard a budget can the default-shaped retry policy absorb? Prints the table.
#[test]
fn slowdown_budget_sweep() {
    for per_window in [100u64, 40, 20, 10, 5] {
        let (ok, failed, total, shed, _) = run(24, 4, per_window);
        println!(
            "budget {per_window}/100ms/prefix: {ok} ok, {failed} failed, {shed}/{total} shed ({:.0}%)",
            100.0 * shed as f64 / total as f64
        );
    }
}

/// Same storm, one shared client object: throttle off (cap 0) vs on (AIMD, cap 64 and 16). Prints the table; the
/// gate is only that the throttled run loses no publish and sends fewer shed requests than the unthrottled run.
#[test]
fn shared_throttle_vs_none() {
    for (writers, each, per_window) in [(24usize, 4usize, 20u64), (48, 4, 10)] {
        let mut rows = vec![];
        for cap in [0usize, 64, 16] {
            let ((ok, failed, total, shed, _), secs, lim) = run_mode(writers, each, per_window, Some(cap));
            println!(
                "{writers}w x{each}, budget {per_window}/100ms/prefix, shared client cap {cap:>2}: {ok} ok {failed} failed, {shed}/{total} shed ({:.0}%), {secs:.2}s, final limit {lim:?}",
                100.0 * shed as f64 / total as f64
            );
            rows.push((cap, failed, shed, total));
        }
        let off = rows[0];
        let on = rows[1];
        assert_eq!(on.1, 0, "throttled run gave up on publishes");
        assert!(on.2 <= off.2, "throttle should not shed more: off {} on {}", off.2, on.2);
    }
}
