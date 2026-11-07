//! Read-through peer (`vcs-server --upstream <url>`): a [`Backend`] that serves from a local disk
//! cache and forwards everything authoritative to an upstream server (docs/peering.md).
//!
//! * Immutable keys (`objects/*`, `packs/*.idx`, ranges of `packs/*.pack`) are content addressed:
//!   cached on disk forever (bounded by a size cap), fetched once per miss no matter how many
//!   requests ask at the same time (single flight), and hash-checked on receipt from upstream
//!   (objects against their id, indexes against their own checksum and the pack name) before they
//!   are cached or served. Pack ranges cannot be checked alone; clients verify every object they
//!   decode, as against any other store.
//! * The manifest and the pack listing are live state. They are revalidated with a conditional
//!   GET (`If-None-Match`, a 304 carries no body) on every read unless `manifest_ttl` allows
//!   serving a recent copy. A write through this peer replaces the cached manifest atomically with
//!   the answer it got from upstream (read-your-writes) and invalidates the listing; a rejected or
//!   failed write drops the cache. Every compare-and-swap is decided by upstream, never here.
//! * If upstream is unreachable, immutable reads still work from disk; the manifest and listing are
//!   served stale only within `stale_if_error` (default 0 = never). Writes fail with
//!   `Unavailable` and are not accepted locally, so nothing is acknowledged that upstream lacks.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use vcs_remote::manifest::KEY as MANIFEST;
use vcs_remote::{Backend, BackendError, Conditional, DiskCacheBackend, IdxCacheBackend, SharedBytes, Versioned};

#[derive(Clone, Debug)]
pub struct PeerConfig {
    pub cache_dir: PathBuf,
    pub cache_size: u64,
    /// Serve the manifest/listing without asking upstream for this long after the last check.
    pub manifest_ttl: Duration,
    /// If upstream fails, serve a manifest/listing last confirmed at most this long ago.
    pub stale_if_error: Duration,
    /// After an upstream failure, uncached upstream calls fail at once for this long.
    pub fail_fast: Duration,
}

impl PeerConfig {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            cache_size: 8 << 30,
            manifest_ttl: Duration::ZERO,
            stale_if_error: Duration::ZERO,
            fail_fast: Duration::from_secs(2),
        }
    }
}

struct Flight<T> {
    slot: Mutex<Option<T>>,
    cv: Condvar,
}

struct Flights<T>(Mutex<HashMap<String, Arc<Flight<T>>>>);

impl<T: Clone> Flights<T> {
    fn new() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
    /// Run `f` unless an identical call is in flight; then wait for its result instead.
    fn run(&self, key: String, shared: &AtomicU64, fail: impl Fn() -> T, f: impl FnOnce() -> T) -> T {
        let (fl, leader) = {
            let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
            match m.get(&key) {
                Some(x) => (x.clone(), false),
                None => {
                    let x = Arc::new(Flight {
                        slot: Mutex::new(None),
                        cv: Condvar::new(),
                    });
                    m.insert(key.clone(), x.clone());
                    (x, true)
                }
            }
        };
        if !leader {
            shared.fetch_add(1, SeqCst);
            let mut g = fl.slot.lock().unwrap_or_else(|e| e.into_inner());
            while g.is_none() {
                g = fl.cv.wait(g).unwrap_or_else(|e| e.into_inner());
            }
            return g.clone().expect("set");
        }
        struct Done<'a, T: Clone, G: Fn() -> T> {
            owner: &'a Flights<T>,
            key: String,
            fl: Arc<Flight<T>>,
            fail: G,
        }
        impl<T: Clone, G: Fn() -> T> Drop for Done<'_, T, G> {
            fn drop(&mut self) {
                self.owner.0.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.key);
                let mut s = self.fl.slot.lock().unwrap_or_else(|e| e.into_inner());
                if s.is_none() {
                    *s = Some((self.fail)()); // leader panicked
                }
                self.fl.cv.notify_all();
            }
        }
        let done = Done {
            owner: self,
            key,
            fl: fl.clone(),
            fail,
        };
        let r = f();
        *fl.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(r.clone());
        drop(done);
        r
    }
}

/// After an `Unavailable` from upstream, further upstream calls fail at once for `cooldown`
/// instead of each waiting out a connect/read timeout; cache hits never reach this layer.
struct FastFail {
    inner: Arc<dyn Backend>,
    down_until: Mutex<Option<Instant>>,
    cooldown: Duration,
}

impl FastFail {
    fn call<T>(&self, op: &str, key: &str, f: impl FnOnce(&dyn Backend) -> Result<T, BackendError>) -> Result<T, BackendError> {
        if let Some(t) = *self.down_until.lock().unwrap_or_else(|e| e.into_inner()) {
            if Instant::now() < t {
                return Err(BackendError::Unavailable(format!(
                    "upstream marked down (fast-fail window): {op} {key}"
                )));
            }
        }
        let r = f(self.inner.as_ref());
        *self.down_until.lock().unwrap_or_else(|e| e.into_inner()) = match &r {
            Err(BackendError::Unavailable(_)) => Some(Instant::now() + self.cooldown),
            _ => None,
        };
        r
    }
}

impl Backend for FastFail {
    fn get(&self, k: &str) -> Got {
        self.call("get", k, |b| b.get(k))
    }
    fn get_conditional(&self, k: &str, h: &str) -> Result<Conditional, BackendError> {
        self.call("get_conditional", k, |b| b.get_conditional(k, h))
    }
    fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.call("get_range", k, |b| b.get_range(k, o, l))
    }
    fn exists(&self, k: &str) -> Result<bool, BackendError> {
        self.call("exists", k, |b| b.exists(k))
    }
    fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
        self.call("put_if_absent", k, |b| b.put_if_absent(k, d))
    }
    fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
        self.call("put_if_match", k, |b| b.put_if_match(k, d, t))
    }
    fn list(&self, p: &str) -> Lst {
        self.call("list", p, |b| b.list(p))
    }
    fn delete(&self, k: &str) -> Result<(), BackendError> {
        self.call("delete", k, |b| b.delete(k))
    }
    fn supports_gc(&self) -> bool {
        self.inner.supports_gc()
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        self.call("now_ms", "", |b| b.now_ms())
    }
    fn modified_ms(&self, k: &str) -> Result<Option<u64>, BackendError> {
        self.call("modified_ms", k, |b| b.modified_ms(k))
    }
    fn touch(&self, k: &str) -> Result<bool, BackendError> {
        self.call("touch", k, |b| b.touch(k))
    }
}

/// Hash-checks what upstream sends for content-addressed keys before anything caches it.
struct VerifyBackend(Arc<dyn Backend>);

fn integrity(key: &str, why: &str) -> BackendError {
    BackendError::Other(format!("integrity check failed for {key} from upstream: {why}"))
}

impl Backend for VerifyBackend {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        let got = self.0.get(key)?;
        if let Some(v) = &got {
            if let Some(rest) = key.strip_prefix("objects/") {
                let hex = rest.replace('/', "");
                let want = vcs_objects::ObjectId::from_hex(&hex).map_err(|e| integrity(key, &e.to_string()))?;
                if vcs_objects::id_of_encoded(&v.data) != want {
                    return Err(integrity(key, "content does not hash to its id"));
                }
            } else if let Some(h) = key
                .strip_prefix("packs/pack-")
                .and_then(|h| h.strip_suffix(".pack"))
                .filter(|h| h.len() == 64 && h.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
            {
                // a whole pack: trailer == blake3(body) == the name
                let n = v.data.len();
                let ok = n >= 32 && {
                    let (body, trailer) = v.data.split_at(n - 32);
                    blake3::hash(body).as_bytes() == trailer
                        && vcs_objects::ObjectId::from_bytes(trailer.try_into().expect("32")).to_hex() == h
                };
                if !ok {
                    return Err(integrity(key, "pack checksum does not match its name"));
                }
            } else if let Some(h) = IdxCacheBackend::stem_hex(key) {
                if !IdxCacheBackend::valid(&v.data, h) {
                    return Err(integrity(key, "index checksum or pack name mismatch"));
                }
            }
        }
        Ok(got)
    }
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        self.0.get_conditional(key, have)
    }
    fn get_range(&self, key: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.0.get_range(key, o, l)
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
}

struct ManifestCopy {
    v: Versioned,
    checked: Instant,
}

#[derive(Default)]
struct Live {
    /// Bumped (under this lock) by every write through this peer.
    epoch: u64,
    manifest: Option<ManifestCopy>,
    lists: HashMap<String, (Vec<String>, Instant)>,
    /// Last answer for the format marker (`None` inside = no marker), kept only to serve within
    /// `stale_if_error` when upstream is down: every client reads the marker before the manifest.
    format: Option<(Option<Versioned>, Instant)>,
}

type Got = Result<Option<Versioned>, BackendError>;
type Rng = Result<Option<SharedBytes>, BackendError>;
type Lst = Result<Vec<String>, BackendError>;

pub struct PeerBackend {
    upstream: Arc<dyn Backend>,
    cache: DiskCacheBackend,
    cfg: PeerConfig,
    live: Mutex<Live>,
    f_get: Flights<Got>,
    f_rng: Flights<Rng>,
    f_list: Flights<Lst>,
    coalesced: AtomicU64,
}

fn immutable_whole(key: &str) -> bool {
    (key.starts_with("packs/") && (key.ends_with(".idx") || key.ends_with(".pack"))) || key.starts_with("objects/")
}
fn immutable_range(key: &str) -> bool {
    key.starts_with("packs/") && key.ends_with(".pack")
}

impl PeerBackend {
    pub fn new(upstream: Arc<dyn Backend>, cfg: PeerConfig) -> std::io::Result<Self> {
        let upstream: Arc<dyn Backend> = Arc::new(FastFail {
            inner: upstream,
            down_until: Mutex::new(None),
            cooldown: cfg.fail_fast,
        });
        let cache = DiskCacheBackend::new(Arc::new(VerifyBackend(upstream.clone())), &cfg.cache_dir, cfg.cache_size)?;
        Ok(Self {
            upstream,
            cache,
            cfg,
            live: Mutex::new(Live::default()),
            f_get: Flights::new(),
            f_rng: Flights::new(),
            f_list: Flights::new(),
            coalesced: AtomicU64::new(0),
        })
    }
    /// Requests that waited on another in-flight identical fetch instead of going upstream.
    pub fn coalesced(&self) -> u64 {
        self.coalesced.load(SeqCst)
    }
    /// Reads answered from the disk cache since start.
    pub fn disk_hits(&self) -> u64 {
        self.cache.hits()
    }
    fn live(&self) -> std::sync::MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn epoch(&self) -> u64 {
        self.live().epoch
    }
    /// A write through this peer finished: drop what it may have changed.
    /// `manifest`: `None` leaves the cached manifest alone, `Some(x)` replaces it (`Some(None)` drops it).
    fn wrote(&self, manifest: Option<Option<Versioned>>) {
        let mut l = self.live();
        l.epoch += 1;
        l.lists.clear();
        l.format = None;
        if let Some(m) = manifest {
            l.manifest = m.map(|v| ManifestCopy {
                v,
                checked: Instant::now(),
            });
        }
    }

    fn manifest_get(&self) -> Got {
        let epoch = self.epoch();
        self.f_get.run(
            format!("{epoch}\0{MANIFEST}"),
            &self.coalesced,
            || Err(BackendError::Other("peer: fetch aborted".into())),
            || self.manifest_fetch(epoch),
        )
    }
    fn manifest_fetch(&self, epoch: u64) -> Got {
        let have = {
            let l = self.live();
            if let Some(m) = &l.manifest {
                if !self.cfg.manifest_ttl.is_zero() && m.checked.elapsed() < self.cfg.manifest_ttl {
                    return Ok(Some(m.v.clone()));
                }
            }
            l.manifest.as_ref().map(|m| (m.v.clone(), ()))
        };
        let res = match &have {
            Some((v, _)) => match self.upstream.get_conditional(MANIFEST, &v.tag) {
                Ok(Conditional::NotModified) => Ok(Some(v.clone())),
                Ok(Conditional::Modified(n)) => Ok(Some(n)),
                Ok(Conditional::Missing) => Ok(None),
                Err(e) => Err(e),
            },
            None => self.upstream.get(MANIFEST),
        };
        match res {
            Ok(v) => {
                let mut l = self.live();
                if l.epoch == epoch {
                    l.manifest = v.clone().map(|v| ManifestCopy {
                        v,
                        checked: Instant::now(),
                    });
                }
                Ok(v)
            }
            Err(e @ BackendError::Unavailable(_)) => {
                let l = self.live();
                match &l.manifest {
                    Some(m) if !self.cfg.stale_if_error.is_zero() && m.checked.elapsed() < self.cfg.stale_if_error => Ok(Some(m.v.clone())),
                    _ => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// The format marker: always asked of upstream, but when upstream is unavailable the last answer
    /// is served within `stale_if_error` (like the manifest), so a warm peer keeps serving reads.
    fn format_get(&self) -> Got {
        let epoch = self.epoch();
        self.f_get.run(
            format!("{epoch}\0{}", vcs_remote::format::KEY),
            &self.coalesced,
            || Err(BackendError::Other("peer: fetch aborted".into())),
            || match self.upstream.get(vcs_remote::format::KEY) {
                Ok(v) => {
                    let mut l = self.live();
                    if l.epoch == epoch {
                        l.format = Some((v.clone(), Instant::now()));
                    }
                    Ok(v)
                }
                Err(e @ BackendError::Unavailable(_)) => {
                    let l = self.live();
                    match &l.format {
                        Some((v, at)) if !self.cfg.stale_if_error.is_zero() && at.elapsed() < self.cfg.stale_if_error => Ok(v.clone()),
                        _ => Err(e),
                    }
                }
                Err(e) => Err(e),
            },
        )
    }

    fn list_fetch(&self, prefix: &str, epoch: u64) -> Lst {
        {
            let l = self.live();
            if let Some((v, at)) = l.lists.get(prefix) {
                if !self.cfg.manifest_ttl.is_zero() && at.elapsed() < self.cfg.manifest_ttl {
                    return Ok(v.clone());
                }
            }
        }
        match self.upstream.list(prefix) {
            Ok(v) => {
                let mut l = self.live();
                if l.epoch == epoch {
                    l.lists.insert(prefix.to_owned(), (v.clone(), Instant::now()));
                }
                Ok(v)
            }
            Err(e @ BackendError::Unavailable(_)) => {
                let l = self.live();
                match l.lists.get(prefix) {
                    Some((v, at)) if !self.cfg.stale_if_error.is_zero() && at.elapsed() < self.cfg.stale_if_error => Ok(v.clone()),
                    _ => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }
}

impl Backend for PeerBackend {
    fn get(&self, key: &str) -> Got {
        if key == MANIFEST {
            return self.manifest_get();
        }
        if key == vcs_remote::format::KEY {
            return self.format_get();
        }
        let immut = immutable_whole(key);
        let ep = if immut { 0 } else { self.epoch() };
        self.f_get.run(
            format!("{ep}\0{key}"),
            &self.coalesced,
            || Err(BackendError::Other("peer: fetch aborted".into())),
            || if immut { self.cache.get(key) } else { self.upstream.get(key) },
        )
    }
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        if key != MANIFEST {
            return self.upstream.get_conditional(key, have);
        }
        Ok(match self.manifest_get()? {
            None => Conditional::Missing,
            Some(v) if v.tag == have => Conditional::NotModified,
            Some(v) => Conditional::Modified(v),
        })
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        Ok(self.get_range_shared(key, offset, len)?.map(SharedBytes::into_vec))
    }
    fn get_range_shared(&self, key: &str, offset: u64, len: u64) -> Rng {
        if !immutable_range(key) || len == 0 {
            return Ok(self.upstream.get_range(key, offset, len)?.map(SharedBytes::from_vec));
        }
        self.f_rng.run(
            format!("{key}\0{offset}\0{len}"),
            &self.coalesced,
            || Err(BackendError::Other("peer: fetch aborted".into())),
            || Ok(self.cache.get_range(key, offset, len)?.map(SharedBytes::from_vec)),
        )
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        self.upstream.exists(key)
    }
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        let r = if immutable_whole(key) {
            self.cache.put_if_absent(key, data)
        } else {
            self.upstream.put_if_absent(key, data)
        };
        self.wrote((key == MANIFEST).then_some(None));
        r
    }
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        let r = self.upstream.put_if_match(key, data, tag);
        let keep = match (&r, key == MANIFEST) {
            (Ok(Some(t)), true) => Some(Some(Versioned {
                data: data.to_vec(),
                tag: t.clone(),
            })),
            (_, true) => Some(None),
            _ => None,
        };
        self.wrote(keep);
        r
    }
    fn list(&self, prefix: &str) -> Lst {
        let epoch = self.epoch();
        self.f_list.run(
            format!("{epoch}\0{prefix}"),
            &self.coalesced,
            || Err(BackendError::Other("peer: fetch aborted".into())),
            || self.list_fetch(prefix, epoch),
        )
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        let r = self.upstream.delete(key);
        self.wrote((key == MANIFEST).then_some(None));
        r
    }
    fn supports_gc(&self) -> bool {
        self.upstream.supports_gc()
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        self.upstream.now_ms()
    }
    fn modified_ms(&self, key: &str) -> Result<Option<u64>, BackendError> {
        self.upstream.modified_ms(key)
    }
    fn touch(&self, key: &str) -> Result<bool, BackendError> {
        self.upstream.touch(key)
    }
}
