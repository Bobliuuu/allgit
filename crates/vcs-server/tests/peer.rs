//! Read-through peer: coalescing, manifest freshness, integrity, chains, origin outage.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;
use vcs_remote::*;
use vcs_server::peer::{PeerBackend, PeerConfig};

/// Wraps a backend: counts calls, can be switched off (Unavailable) and slowed down.
struct Probe {
    inner: Arc<dyn Backend>,
    down: AtomicBool,
    delay: Duration,
    ranges: AtomicU64,
    gets: AtomicU64,
    lists: AtomicU64,
    conds: AtomicU64,
}
impl Probe {
    fn new(inner: Arc<dyn Backend>, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            inner,
            down: AtomicBool::new(false),
            delay,
            ranges: 0.into(),
            gets: 0.into(),
            lists: 0.into(),
            conds: 0.into(),
        })
    }
    fn chk(&self) -> Result<(), BackendError> {
        if self.down.load(SeqCst) {
            return Err(BackendError::Unavailable("origin down".into()));
        }
        std::thread::sleep(self.delay);
        Ok(())
    }
}
impl Backend for Probe {
    fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
        self.chk()?;
        self.gets.fetch_add(1, SeqCst);
        self.inner.get(k)
    }
    fn get_conditional(&self, k: &str, h: &str) -> Result<Conditional, BackendError> {
        self.chk()?;
        self.conds.fetch_add(1, SeqCst);
        self.inner.get_conditional(k, h)
    }
    fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.chk()?;
        self.ranges.fetch_add(1, SeqCst);
        self.inner.get_range(k, o, l)
    }
    fn exists(&self, k: &str) -> Result<bool, BackendError> {
        self.chk()?;
        self.inner.exists(k)
    }
    fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
        self.chk()?;
        self.inner.put_if_absent(k, d)
    }
    fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
        self.chk()?;
        self.inner.put_if_match(k, d, t)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
        self.chk()?;
        self.lists.fetch_add(1, SeqCst);
        self.inner.list(p)
    }
}

fn obj_key(body: &[u8]) -> String {
    let h = vcs_objects::id_of_encoded(body).to_hex();
    format!("objects/{}/{}", &h[..2], &h[2..])
}

fn peer(up: Arc<dyn Backend>, ttl: u64, stale: u64) -> (Arc<PeerBackend>, tempfile::TempDir) {
    let d = tempfile::tempdir().unwrap();
    let mut c = PeerConfig::new(d.path());
    c.manifest_ttl = Duration::from_secs(ttl);
    c.stale_if_error = Duration::from_secs(stale);
    c.fail_fast = Duration::from_millis(50);
    (Arc::new(PeerBackend::new(up, c).unwrap()), d)
}

#[test]
fn concurrent_misses_make_one_upstream_fetch() {
    let mem = Arc::new(MemBackend::new());
    mem.put_if_absent("packs/p.pack", &vec![7u8; 4096]).unwrap();
    mem.put_if_absent("manifest", b"m").unwrap();
    let up = Probe::new(mem, Duration::from_millis(150));
    let (p, _d) = peer(up.clone(), 0, 0);
    let hs: Vec<_> = (0..64)
        .map(|_| {
            let p = p.clone();
            std::thread::spawn(move || {
                assert_eq!(p.get_range("packs/p.pack", 10, 100).unwrap().unwrap(), vec![7u8; 100]);
                assert_eq!(p.list("packs/").unwrap(), vec!["packs/p.pack"]);
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    assert_eq!(up.ranges.load(SeqCst), 1, "64 concurrent misses, one upstream range read");
    assert_eq!(up.lists.load(SeqCst), 1, "listings coalesce too");
    assert!(p.coalesced() >= 100, "{}", p.coalesced());
    // now warm: no upstream range read at all
    p.get_range("packs/p.pack", 10, 100).unwrap();
    assert_eq!(up.ranges.load(SeqCst), 1);
}

#[test]
fn never_stale_after_a_push_through_the_peer_even_with_a_long_ttl() {
    let mem = Arc::new(MemBackend::new());
    mem.put_if_absent("manifest", b"m1").unwrap();
    let up = Probe::new(mem.clone(), Duration::ZERO);
    let (p, _d) = peer(up.clone(), 3600, 0);
    let m1 = p.get("manifest").unwrap().unwrap();
    assert_eq!(m1.data, b"m1");
    p.list("packs/").unwrap();
    // cached within ttl: no further upstream traffic
    let before = (up.gets.load(SeqCst), up.conds.load(SeqCst), up.lists.load(SeqCst));
    p.get("manifest").unwrap();
    p.list("packs/").unwrap();
    assert_eq!(before, (up.gets.load(SeqCst), up.conds.load(SeqCst), up.lists.load(SeqCst)));
    // push through the peer
    let t2 = p.put_if_match("manifest", b"m2", &m1.tag).unwrap().unwrap();
    p.put_if_absent("packs/new.pack", b"x").unwrap();
    let again = p.get("manifest").unwrap().unwrap();
    assert_eq!((again.data.as_slice(), again.tag.as_str()), (&b"m2"[..], t2.as_str()));
    assert!(
        p.list("packs/").unwrap().contains(&"packs/new.pack".to_string()),
        "listing invalidated by the write"
    );
    // a stale CAS is decided upstream and drops the cache
    assert_eq!(p.put_if_match("manifest", b"m3", &m1.tag).unwrap(), None);
    assert_eq!(p.get("manifest").unwrap().unwrap().data, b"m2");
    // a write that bypasses the peer is seen only after the ttl...
    let cur = mem.get("manifest").unwrap().unwrap();
    mem.put_if_match("manifest", b"m-origin", &cur.tag).unwrap().unwrap();
    assert_eq!(p.get("manifest").unwrap().unwrap().data, b"m2", "ttl window (documented)");
    // ...and immediately with ttl 0 (conditional GET each read)
    let (p0, _d0) = peer(up.clone(), 0, 0);
    assert_eq!(p0.get("manifest").unwrap().unwrap().data, b"m-origin");
    let cur = mem.get("manifest").unwrap().unwrap();
    mem.put_if_match("manifest", b"m-origin2", &cur.tag).unwrap().unwrap();
    assert_eq!(p0.get("manifest").unwrap().unwrap().data, b"m-origin2");
    let c = up.conds.load(SeqCst);
    p0.get("manifest").unwrap();
    assert_eq!(
        up.conds.load(SeqCst),
        c + 1,
        "unchanged manifest costs one conditional GET, no body"
    );
}

#[test]
fn racing_readers_and_a_push_never_leave_a_stale_manifest() {
    let mem = Arc::new(MemBackend::new());
    mem.put_if_absent("manifest", b"v0").unwrap();
    let up = Probe::new(mem, Duration::from_millis(2));
    let (p, _d) = peer(up, 3600, 0);
    for round in 0..30 {
        let stop = Arc::new(AtomicBool::new(false));
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let (p, s) = (p.clone(), stop.clone());
                std::thread::spawn(move || {
                    while !s.load(SeqCst) {
                        p.get("manifest").unwrap();
                    }
                })
            })
            .collect();
        let cur = p.get("manifest").unwrap().unwrap();
        let new = format!("v{}", round + 1);
        p.put_if_match("manifest", new.as_bytes(), &cur.tag).unwrap().unwrap();
        // from here on (the push returned) every read must show it
        for _ in 0..20 {
            assert_eq!(p.get("manifest").unwrap().unwrap().data, new.as_bytes());
        }
        stop.store(true, SeqCst);
        for r in readers {
            r.join().unwrap();
        }
    }
}

#[test]
fn bad_bytes_from_upstream_are_refused_and_never_cached() {
    let mem = Arc::new(MemBackend::new());
    let body = b"some object body".to_vec();
    let good = obj_key(&body);
    mem.put_if_absent(&good, &body).unwrap();
    let evil = obj_key(b"the real content");
    mem.put_if_absent(&evil, b"tampered content").unwrap();
    mem.put_if_absent(
        "packs/pack-0000000000000000000000000000000000000000000000000000000000000000.idx",
        b"garbage-garbage-garbage-garbage-garbage-garbage-garbage",
    )
    .unwrap();
    let up = Probe::new(mem.clone(), Duration::ZERO);
    let (p, d) = peer(up, 0, 0);
    assert_eq!(p.get(&good).unwrap().unwrap().data, body);
    let e = p.get(&evil).unwrap_err().to_string();
    assert!(e.contains("integrity"), "{e}");
    let e = p
        .get("packs/pack-0000000000000000000000000000000000000000000000000000000000000000.idx")
        .unwrap_err()
        .to_string();
    assert!(e.contains("integrity"), "{e}");
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1, "only the good object was cached");
}

#[test]
fn origin_down_reads_from_cache_work_and_writes_fail_without_loss() {
    let mem = Arc::new(MemBackend::new());
    let body = b"warm object".to_vec();
    let k = obj_key(&body);
    mem.put_if_absent(&k, &body).unwrap();
    mem.put_if_absent("packs/p.pack", &[1u8; 64]).unwrap();
    mem.put_if_absent("manifest", b"m1").unwrap();
    let up = Probe::new(mem.clone(), Duration::ZERO);
    let (strict, _d1) = peer(up.clone(), 0, 0);
    let (lenient, _d2) = peer(up.clone(), 0, 300);
    for p in [&strict, &lenient] {
        p.get(&k).unwrap();
        p.get_range("packs/p.pack", 0, 16).unwrap();
        let m = p.get("manifest").unwrap().unwrap();
        assert_eq!(m.data, b"m1");
        p.list("packs/").unwrap();
    }
    up.down.store(true, SeqCst);
    for p in [&strict, &lenient] {
        assert_eq!(p.get(&k).unwrap().unwrap().data, body, "immutable reads survive an outage");
        assert_eq!(p.get_range("packs/p.pack", 0, 16).unwrap().unwrap(), vec![1u8; 16]);
        assert!(p.get("packs/other.idx").is_err(), "an uncached key is an error, not a lie");
    }
    assert!(strict.get("manifest").is_err(), "default: no stale manifest");
    assert_eq!(lenient.get("manifest").unwrap().unwrap().data, b"m1", "opt-in stale-if-error");
    assert_eq!(lenient.list("packs/").unwrap().len(), 1);
    // writes fail cleanly and nothing is acknowledged or kept locally
    let newbody = b"new object".to_vec();
    let nk = obj_key(&newbody);
    assert!(matches!(lenient.put_if_absent(&nk, &newbody), Err(BackendError::Unavailable(_))));
    let tag = lenient.get("manifest").unwrap().unwrap().tag;
    assert!(matches!(
        lenient.put_if_match("manifest", b"m2", &tag),
        Err(BackendError::Unavailable(_))
    ));
    up.down.store(false, SeqCst);
    std::thread::sleep(Duration::from_millis(80));
    assert!(!mem.exists(&nk).unwrap());
    assert_eq!(mem.get("manifest").unwrap().unwrap().data, b"m1");
    // and the retry after recovery works
    assert!(lenient.put_if_absent(&nk, &newbody).unwrap());
    let tag = lenient.get("manifest").unwrap().unwrap().tag;
    lenient.put_if_match("manifest", b"m2", &tag).unwrap().unwrap();
    assert_eq!(mem.get("manifest").unwrap().unwrap().data, b"m2");
}

#[test]
fn edge_regional_origin_chain() {
    let mem = Arc::new(MemBackend::new());
    mem.put_if_absent("packs/p.pack", &vec![9u8; 1024]).unwrap();
    mem.put_if_absent("manifest", b"m1").unwrap();
    let origin = Probe::new(mem.clone(), Duration::ZERO);
    let (regional, _a) = peer(origin.clone(), 0, 0);
    let mid = Probe::new(regional.clone(), Duration::ZERO);
    let (edge, _b) = peer(mid.clone(), 0, 0);
    for _ in 0..3 {
        assert_eq!(edge.get_range("packs/p.pack", 0, 32).unwrap().unwrap(), vec![9u8; 32]);
    }
    assert_eq!((origin.ranges.load(SeqCst), mid.ranges.load(SeqCst)), (1, 1), "one fetch per tier");
    // push through the edge reaches origin and the edge reads it back
    let m = edge.get("manifest").unwrap().unwrap();
    let t = edge.put_if_match("manifest", b"m2", &m.tag).unwrap().unwrap();
    assert_eq!(mem.get("manifest").unwrap().unwrap().data, b"m2");
    assert_eq!(edge.get("manifest").unwrap().unwrap().tag, t);
    // another edge on the same regional sees it at once (ttl 0)
    let (edge2, _c) = peer(mid.clone(), 0, 0);
    assert_eq!(edge2.get("manifest").unwrap().unwrap().data, b"m2");
    // a push at the origin shows through both tiers
    let cur = mem.get("manifest").unwrap().unwrap();
    mem.put_if_match("manifest", b"m3", &cur.tag).unwrap().unwrap();
    assert_eq!(edge.get("manifest").unwrap().unwrap().data, b"m3");
    // origin down: warm range still served through the chain
    origin.down.store(true, SeqCst);
    assert_eq!(edge2.get_range("packs/p.pack", 0, 32).unwrap().unwrap().len(), 32);
}
