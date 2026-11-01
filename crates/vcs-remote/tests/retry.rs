//! Fault injection for the client retry layer and the parallel pack/upload paths.

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vcs_remote::{Backend, BackendError, Conditional, MemBackend, RetryBackend, RetryPolicy, Versioned};

type Hook = Box<dyn Fn(&MemBackend) + Send>;

/// What happens to the Nth call (1-based, counted over writes and reads alike).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Fault {
    /// Request never reaches the store.
    DropBefore,
    /// Store applies it, the answer is lost.
    DropAfter,
}

struct Faulty {
    inner: Arc<MemBackend>,
    plan: Mutex<Vec<(usize, Fault)>>,
    calls: AtomicUsize,
    /// Run right after a lost-ack write is applied: lets a test play a concurrent writer.
    intervene: Mutex<Option<Hook>>,
}

impl Faulty {
    fn new(plan: &[(usize, Fault)]) -> (Arc<Self>, Arc<MemBackend>) {
        let mem = Arc::new(MemBackend::new());
        (
            Arc::new(Self {
                inner: mem.clone(),
                plan: Mutex::new(plan.to_vec()),
                calls: AtomicUsize::new(0),
                intervene: Mutex::new(None),
            }),
            mem,
        )
    }
    fn fault(&self) -> Option<Fault> {
        let n = self.calls.fetch_add(1, SeqCst) + 1;
        self.plan.lock().unwrap().iter().find(|(i, _)| *i == n).map(|(_, f)| *f)
    }
    fn lost() -> BackendError {
        BackendError::Unavailable("injected".into())
    }
}

struct Shared(Arc<Faulty>);

impl Backend for Shared {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        match self.0.fault() {
            Some(_) => Err(Faulty::lost()),
            None => self.0.inner.get(key),
        }
    }
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        match self.0.fault() {
            Some(_) => Err(Faulty::lost()),
            None => self.0.inner.get_conditional(key, have),
        }
    }
    fn get_range(&self, key: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        match self.0.fault() {
            Some(_) => Err(Faulty::lost()),
            None => self.0.inner.get_range(key, o, l),
        }
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        match self.0.fault() {
            Some(_) => Err(Faulty::lost()),
            None => self.0.inner.exists(key),
        }
    }
    fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
        match self.0.fault() {
            Some(_) => Err(Faulty::lost()),
            None => self.0.inner.list(p),
        }
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        match self.0.fault() {
            Some(_) => Err(Faulty::lost()),
            None => self.0.inner.delete(key),
        }
    }
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        match self.0.fault() {
            Some(Fault::DropBefore) => Err(Faulty::lost()),
            Some(Fault::DropAfter) => {
                let _ = self.0.inner.put_if_absent(key, data);
                if let Some(f) = &*self.0.intervene.lock().unwrap() {
                    f(&self.0.inner);
                }
                Err(Faulty::lost())
            }
            None => self.0.inner.put_if_absent(key, data),
        }
    }
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        match self.0.fault() {
            Some(Fault::DropBefore) => Err(Faulty::lost()),
            Some(Fault::DropAfter) => {
                let _ = self.0.inner.put_if_match(key, data, tag);
                if let Some(f) = &*self.0.intervene.lock().unwrap() {
                    f(&self.0.inner);
                }
                Err(Faulty::lost())
            }
            None => self.0.inner.put_if_match(key, data, tag),
        }
    }
}

fn retrying(f: &Arc<Faulty>, retries: u32) -> RetryBackend {
    RetryBackend::with_policy(
        Box::new(Shared(f.clone())),
        RetryPolicy {
            retries,
            base: Duration::from_millis(1),
            cap: Duration::from_millis(4),
        },
    )
}

#[test]
fn reads_and_lists_retry_through_transient_failures() {
    let (f, mem) = Faulty::new(&[(1, Fault::DropBefore), (2, Fault::DropBefore), (4, Fault::DropBefore)]);
    mem.put_if_absent("a/k", b"v").unwrap();
    let b = retrying(&f, 3);
    assert_eq!(b.get("a/k").unwrap().unwrap().data, b"v"); // calls 1,2 fail, 3 ok
    assert_eq!(b.list("a/").unwrap(), vec!["a/k"]); // call 4 fails, 5 ok
    assert_eq!(b.retried(), 3);
}

#[test]
fn gives_up_after_the_retry_budget_and_surfaces_unavailable() {
    let plan: Vec<_> = (1..=10).map(|i| (i, Fault::DropBefore)).collect();
    let (f, _) = Faulty::new(&plan);
    let b = retrying(&f, 2);
    assert!(matches!(b.get("k"), Err(BackendError::Unavailable(_))));
    assert_eq!(f.calls.load(SeqCst), 3, "1 try + 2 retries");
    let (f, _) = Faulty::new(&plan);
    let b = retrying(&f, 0);
    assert!(b.get("k").is_err());
    assert_eq!(f.calls.load(SeqCst), 1, "retries = 0 never repeats");
}

#[test]
fn permanent_errors_are_not_retried() {
    let (f, _) = Faulty::new(&[]);
    let b = retrying(&f, 5);
    assert!(matches!(b.get("../bad"), Err(BackendError::Other(_))));
    assert_eq!(f.calls.load(SeqCst), 1);
}

#[test]
fn put_if_absent_dropped_before_is_repeated_and_creates_once() {
    let (f, mem) = Faulty::new(&[(1, Fault::DropBefore)]);
    let b = retrying(&f, 3);
    assert!(b.put_if_absent("k", b"v").unwrap(), "create succeeds after the retry");
    assert_eq!(mem.get("k").unwrap().unwrap().data, b"v");
}

#[test]
fn put_if_absent_lost_ack_reports_existing_not_an_error() {
    let (f, mem) = Faulty::new(&[(1, Fault::DropAfter)]);
    let b = retrying(&f, 3);
    // applied but unacknowledged: the safe answer is "already existed", and the data is ours
    assert!(!b.put_if_absent("k", b"v").unwrap());
    assert_eq!(mem.get("k").unwrap().unwrap().data, b"v");
    assert_eq!(f.calls.load(SeqCst), 2, "one put, one read-back, no blind second put");
}

#[test]
fn cas_lost_ack_is_recognised_as_applied() {
    let (f, mem) = Faulty::new(&[(1, Fault::DropAfter)]);
    mem.put_if_absent("m", b"v1").unwrap();
    let t1 = mem.get("m").unwrap().unwrap().tag;
    let b = retrying(&f, 3);
    let t2 = b.put_if_match("m", b"v2", &t1).unwrap().expect("applied");
    let cur = mem.get("m").unwrap().unwrap();
    assert_eq!((cur.data.as_slice(), cur.tag.as_str()), (&b"v2"[..], t2.as_str()));
    assert_eq!(f.calls.load(SeqCst), 2, "CAS sent once, then read back; never repeated");
}

#[test]
fn cas_dropped_before_is_repeated_and_applies() {
    let (f, mem) = Faulty::new(&[(1, Fault::DropBefore)]);
    mem.put_if_absent("m", b"v1").unwrap();
    let t1 = mem.get("m").unwrap().unwrap().tag;
    let b = retrying(&f, 3);
    assert!(b.put_if_match("m", b"v2", &t1).unwrap().is_some());
    assert_eq!(mem.get("m").unwrap().unwrap().data, b"v2");
}

#[test]
fn cas_lost_ack_when_another_writer_won_reports_lost_race() {
    // The CAS is dropped before applying and a rival commits in between: we must report None, never
    // overwrite the rival, and never claim success.
    let (f, mem) = Faulty::new(&[(1, Fault::DropBefore)]);
    mem.put_if_absent("m", b"v1").unwrap();
    let t1 = mem.get("m").unwrap().unwrap().tag;
    mem.put_if_match("m", b"rival", &t1).unwrap().unwrap();
    let b = retrying(&f, 3);
    assert_eq!(b.put_if_match("m", b"v2", &t1).unwrap(), None);
    assert_eq!(mem.get("m").unwrap().unwrap().data, b"rival");
}

#[test]
fn cas_lost_ack_then_rival_overwrites_is_a_lost_race_not_a_false_success() {
    // Applied and the ack lost, then a rival replaced our value before the read-back: foreign
    // content under a new tag -> None (the caller reloads and sees the truth); never Some.
    let (f, mem) = Faulty::new(&[(1, Fault::DropAfter)]);
    mem.put_if_absent("m", b"v1").unwrap();
    let t1 = mem.get("m").unwrap().unwrap().tag;
    *f.intervene.lock().unwrap() = Some(Box::new(|m: &MemBackend| {
        let t = m.get("m").unwrap().unwrap().tag;
        m.put_if_match("m", b"rival", &t).unwrap().unwrap();
    }));
    let b = retrying(&f, 3);
    assert_eq!(b.put_if_match("m", b"v2", &t1).unwrap(), None);
    assert_eq!(mem.get("m").unwrap().unwrap().data, b"rival");
}

#[test]
fn cas_never_double_applies_under_repeated_lost_acks() {
    // Every first attempt of each CAS round loses its ack; 20 rounds must produce 20 distinct
    // versions in order, none skipped or applied twice.
    let plan: Vec<_> = (0..20).map(|r| (1 + r * 2, Fault::DropAfter)).collect();
    let (f, mem) = Faulty::new(&plan);
    mem.put_if_absent("ctr", b"0").unwrap();
    let b = retrying(&f, 3);
    let mut tag = mem.get("ctr").unwrap().unwrap().tag;
    for i in 1..=20u32 {
        tag = b
            .put_if_match("ctr", i.to_string().as_bytes(), &tag)
            .unwrap()
            .unwrap_or_else(|| panic!("round {i} lost"));
    }
    assert_eq!(mem.get("ctr").unwrap().unwrap().data, b"20");
}

// ---------- parallel fetch / upload ----------

mod parallel {
    use super::*;
    use vcs_objects::{id_of_encoded, Object};
    use vcs_remote::fetch_many;
    use vcs_store::pack::write_pack;

    /// Counts peak concurrent ranged GETs and optionally fails the Nth one.
    struct Probe {
        inner: MemBackend,
        live: AtomicUsize,
        peak: AtomicUsize,
        fail_range: Mutex<Option<usize>>,
        ranges: AtomicUsize,
    }
    impl Backend for Probe {
        fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
            self.inner.get(k)
        }
        fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
            if l > 33 {
                let n = self.ranges.fetch_add(1, SeqCst) + 1;
                if *self.fail_range.lock().unwrap() == Some(n) {
                    return Err(BackendError::Unavailable("injected".into()));
                }
            }
            let now = self.live.fetch_add(1, SeqCst) + 1;
            self.peak.fetch_max(now, SeqCst);
            std::thread::sleep(Duration::from_millis(5));
            let r = self.inner.get_range(k, o, l);
            self.live.fetch_sub(1, SeqCst);
            r
        }
        fn exists(&self, k: &str) -> Result<bool, BackendError> {
            self.inner.exists(k)
        }
        fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
            self.inner.put_if_absent(k, d)
        }
        fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
            self.inner.put_if_match(k, d, t)
        }
        fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
            self.inner.list(p)
        }
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

    /// Many blobs far apart (gaps larger than the coalescing window) so each needs its own range.
    fn setup(n: usize) -> (Probe, Vec<vcs_objects::ObjectId>) {
        let p = Probe {
            inner: MemBackend::new(),
            live: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            fail_range: Mutex::new(None),
            ranges: AtomicUsize::new(0),
        };
        let objs: Vec<(vcs_objects::ObjectId, Vec<u8>)> = (0..(n * 4) as u64)
            .map(|i| {
                let e = Object::Blob(noise(i + 1, 40_000)).encode();
                (id_of_encoded(&e), e)
            })
            .collect();
        let ids = objs.iter().step_by(4).map(|o| o.0).collect();
        let dir = tempfile::tempdir().unwrap();
        let pk = write_pack(&dir.path().join("pack"), dir.path(), objs).unwrap();
        p.inner
            .put_if_absent("packs/p1.pack", &std::fs::read(&pk.pack_path).unwrap())
            .unwrap();
        p.inner
            .put_if_absent("packs/p1.idx", &std::fs::read(pk.pack_path.with_extension("idx")).unwrap())
            .unwrap();
        (p, ids)
    }

    #[test]
    fn multi_range_fetch_runs_concurrently_and_returns_everything() {
        let (p, ids) = setup(12);
        let got = fetch_many(&p, &ids).unwrap();
        assert_eq!(got.len(), ids.len());
        let want: std::collections::HashSet<_> = ids.iter().collect();
        assert!(got.iter().all(|(id, _)| want.contains(id)));
        let peak = p.peak.load(SeqCst);
        assert!(p.ranges.load(SeqCst) >= 6, "several separate ranges, got {}", p.ranges.load(SeqCst));
        assert!(peak > 1, "ranged GETs overlapped (peak {peak})");
        assert!(peak <= 8, "bounded by the default concurrency (peak {peak})");
    }

    #[test]
    fn a_failed_range_fails_the_fetch_instead_of_returning_partial_data() {
        let (p, ids) = setup(12);
        *p.fail_range.lock().unwrap() = Some(5);
        let e = fetch_many(&p, &ids).unwrap_err();
        assert!(matches!(e, BackendError::Unavailable(_)), "{e:?}");
        // and through the retry wrapper a one-off failure is absorbed (the whole fetch is not
        // repeated by RetryBackend, only the failed range, because it wraps get_range)
        *p.fail_range.lock().unwrap() = Some(5);
        p.ranges.store(0, SeqCst);
        let r = RetryBackend::with_policy(
            Box::new(p),
            RetryPolicy {
                retries: 2,
                base: Duration::from_millis(1),
                cap: Duration::from_millis(2),
            },
        );
        assert_eq!(fetch_many(&r, &ids).unwrap().len(), ids.len());
        assert_eq!(r.retried(), 1);
    }
}
