//! Backend resilience: bounded retries with jittered backoff for transient errors, a per-request
//! deadline, and a circuit breaker. Only [`BackendError::Unavailable`] is retried (docs/server.md:
//! protocol steps are idempotent, so an ambiguous failed write is safe to repeat; the worst case is a
//! spurious 412 that clients already handle by re-reading).
//!
//! The deadline bounds the retry loop (no attempt or backoff sleep starts past it). One attempt that
//! blocks inside the backend is bounded only by that backend's own timeouts (ureq/S3 timeouts, I/O).

use crate::metrics::Metrics;
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vcs_remote::Versioned;
use vcs_remote::{Backend, BackendError, Conditional};

#[derive(Clone, Debug)]
pub struct Resilience {
    /// Extra attempts after the first for a transient error (0 = no retry).
    pub retries: u32,
    pub base_backoff: Duration,
    pub max_backoff: Duration,
    /// Per-request budget for all backend work of one request; `None` = unbounded.
    pub request_deadline: Option<Duration>,
    /// Consecutive failed operations that open the breaker; 0 disables the breaker.
    pub breaker_threshold: u32,
    /// How long the breaker stays open before one probe call is let through.
    pub breaker_cooldown: Duration,
}

impl Default for Resilience {
    fn default() -> Self {
        Self {
            retries: 2,
            base_backoff: Duration::from_millis(50),
            max_backoff: Duration::from_secs(1),
            request_deadline: Some(Duration::from_secs(60)),
            breaker_threshold: 5,
            breaker_cooldown: Duration::from_secs(5),
        }
    }
}

thread_local! {
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Set (or clear) the backend deadline of the request being served on this thread.
pub fn set_request_deadline(d: Option<Instant>) {
    DEADLINE.with(|c| c.set(d));
}

#[derive(Default)]
struct Breaker {
    fails: u32,
    open_until: Option<Instant>,
    probing: bool,
}

pub struct ResilientBackend {
    inner: Arc<dyn Backend>,
    cfg: Resilience,
    breaker: Mutex<Breaker>,
    rng: AtomicU64,
    metrics: Arc<Metrics>,
}

impl ResilientBackend {
    pub fn new(inner: Arc<dyn Backend>, cfg: Resilience, metrics: Arc<Metrics>) -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        Self {
            inner,
            cfg,
            breaker: Mutex::default(),
            rng: AtomicU64::new(seed | 1),
            metrics,
        }
    }

    /// Seconds a client should wait while the breaker is open (`None` = closed).
    pub fn retry_after_secs(&self) -> Option<u64> {
        let b = self.breaker.lock().unwrap_or_else(|e| e.into_inner());
        let t = b.open_until?;
        let left = t.saturating_duration_since(Instant::now());
        Some(left.as_millis().div_ceil(1000).max(1) as u64)
    }

    fn jitter(&self, max_ms: u64) -> u64 {
        // splitmix64 step over an atomic counter: cheap, thread-safe, good enough for jitter
        let mut z = self.rng.fetch_add(0x9E37_79B9_7F4A_7C15, Relaxed).wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) % (max_ms + 1)
    }

    /// Backoff before retry number `n` (0-based): half fixed, half random, capped.
    fn backoff(&self, n: u32) -> Duration {
        let cap = self.cfg.max_backoff.as_millis() as u64;
        let d = (self.cfg.base_backoff.as_millis() as u64)
            .saturating_mul(1u64 << n.min(20))
            .min(cap);
        Duration::from_millis(d / 2 + self.jitter(d / 2))
    }

    fn admit(&self) -> Result<(), ()> {
        if self.cfg.breaker_threshold == 0 {
            return Ok(());
        }
        let mut b = self.breaker.lock().unwrap_or_else(|e| e.into_inner());
        match b.open_until {
            None => Ok(()),
            Some(t) if Instant::now() < t => Err(()),
            Some(_) if b.probing => Err(()),
            Some(_) => {
                b.probing = true;
                Ok(())
            }
        }
    }

    fn record(&self, ok: bool) {
        if self.cfg.breaker_threshold == 0 {
            return;
        }
        let mut b = self.breaker.lock().unwrap_or_else(|e| e.into_inner());
        b.probing = false;
        if ok {
            b.fails = 0;
            b.open_until = None;
            self.metrics.breaker_open.store(0, Relaxed);
        } else {
            b.fails = b.fails.saturating_add(1);
            if b.fails >= self.cfg.breaker_threshold {
                if b.open_until.map_or(true, |t| Instant::now() >= t) {
                    self.metrics.breaker_trips.fetch_add(1, Relaxed);
                }
                b.open_until = Some(Instant::now() + self.cfg.breaker_cooldown);
                self.metrics.breaker_open.store(1, Relaxed);
            }
        }
    }

    fn run<T>(&self, retries: u32, f: impl Fn(&dyn Backend) -> Result<T, BackendError>) -> Result<T, BackendError> {
        if self.admit().is_err() {
            self.metrics.backend_error("circuit_open");
            return Err(BackendError::Unavailable("backend circuit breaker is open".into()));
        }
        let deadline = DEADLINE.with(Cell::get);
        let mut attempt = 0;
        loop {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                self.metrics.backend_error("timeout");
                self.record(false);
                return Err(BackendError::Unavailable("request deadline exceeded".into()));
            }
            match f(self.inner.as_ref()) {
                Ok(v) => {
                    self.record(true);
                    return Ok(v);
                }
                // The backend answered: it is up, the request is just wrong.
                Err(e @ BackendError::Other(_)) => {
                    self.metrics.backend_error("other");
                    self.record(true);
                    return Err(e);
                }
                Err(e) => {
                    let wait = self.backoff(attempt);
                    let fits = deadline.map_or(true, |d| Instant::now() + wait < d);
                    if attempt >= retries || !fits {
                        self.metrics.backend_error("unavailable");
                        self.record(false);
                        return Err(e);
                    }
                    self.metrics.backend_retries.fetch_add(1, Relaxed);
                    std::thread::sleep(wait);
                    attempt += 1;
                }
            }
        }
    }

    /// Readiness probe: one HEAD of `key`, no retries, subject to (and feeding) the breaker.
    pub fn probe(&self, key: &str) -> bool {
        self.run(0, |b| b.exists(key)).is_ok()
    }
}

impl Backend for ResilientBackend {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        self.run(self.cfg.retries, |b| b.get(key))
    }
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        self.run(self.cfg.retries, |b| b.get_conditional(key, have))
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.run(self.cfg.retries, |b| b.get_range(key, offset, len))
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        self.run(self.cfg.retries, |b| b.exists(key))
    }
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        self.run(self.cfg.retries, |b| b.put_if_absent(key, data))
    }
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        self.run(self.cfg.retries, |b| b.put_if_match(key, data, tag))
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        self.run(self.cfg.retries, |b| b.list(prefix))
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        self.run(self.cfg.retries, |b| b.delete(key))
    }
    fn supports_gc(&self) -> bool {
        self.inner.supports_gc()
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        self.run(self.cfg.retries, |b| b.now_ms())
    }
    fn modified_ms(&self, key: &str) -> Result<Option<u64>, BackendError> {
        self.run(self.cfg.retries, |b| b.modified_ms(key))
    }
    fn touch(&self, key: &str) -> Result<bool, BackendError> {
        self.run(self.cfg.retries, |b| b.touch(key))
    }
}
