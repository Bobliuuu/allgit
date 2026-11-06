//! Client-side retries with jittered exponential backoff for any [`Backend`].
//!
//! Only [`BackendError::Unavailable`] (timeouts, connection resets, 5xx, 429) is retried.
//! Reads, listings, HEAD and DELETE are idempotent and are simply repeated. A conditional write is
//! **never repeated blind**: when its answer is lost the write may or may not have applied, and a
//! second attempt would then report a precondition failure for a write that in fact succeeded.
//! Instead the wrapper reads the key back and decides (lost-ack handling):
//!
//! * `put_if_absent`: key still absent -> it did not apply, repeat; key present -> `Ok(false)`
//!   ("already existed"), the conservative answer (callers then run their extra checks for
//!   objects that already exist).
//! * `put_if_match(tag)`: key absent -> `Ok(None)`; tag unchanged -> did not apply, repeat; content
//!   equal to ours with a new tag -> it applied, `Ok(Some(new_tag))`; anything else -> another
//!   writer won, `Ok(None)`. A request still in flight on the server when we read back can apply
//!   later; our repeat then fails its precondition and the caller sees `Ok(None)`, which the
//!   publish protocol already resolves by reloading the manifest and finding its own txid.

use crate::backend::{Backend, BackendError, Conditional, Versioned};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Shared adaptive throttle for one [`RetryBackend`] (all threads that use it). Per-call jittered backoff
/// only spaces out one caller's retries; when 503 SlowDown means the whole prefix is over budget, every
/// other thread keeps sending at full rate and burns its own retry budget. This is AIMD, as in TCP:
///
/// * at most `limit` requests are in flight; `limit` starts at `max` (default 64, `VCS_REMOTE_MAX_CONCURRENCY`,
///   0 = off) and **halves** (floor 1) when a request is answered with 503 / SlowDown / 429, at most once per
///   cool-down so a burst of simultaneous 503s counts once;
/// * after a throttle answer, new requests wait for a shared jittered cool-down (`d/2..d`, `d` = 4x the retry base);
/// * every success adds `1/limit` (about +1 per `limit` successes) back, up to `max`.
///
/// Waiting for a permit does not use a retry, so threads queued behind the throttle are not given up on.
struct Throttle {
    max: f64,
    st: Mutex<ThrottleState>,
    cv: Condvar,
}

struct ThrottleState {
    limit: f64,
    inflight: usize,
    cool_until: Option<Instant>,
    throttled: u64,
}

struct Permit<'a>(&'a Throttle);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut s = self.0.st.lock().unwrap_or_else(|e| e.into_inner());
        s.inflight -= 1;
        self.0.cv.notify_all();
    }
}

impl Throttle {
    fn new(max: usize) -> Option<Self> {
        (max > 0).then(|| Throttle {
            max: max as f64,
            st: Mutex::new(ThrottleState {
                limit: max as f64,
                inflight: 0,
                cool_until: None,
                throttled: 0,
            }),
            cv: Condvar::new(),
        })
    }

    fn acquire(&self) -> Permit<'_> {
        let mut s = self.st.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let now = Instant::now();
            let cooling = s.cool_until.filter(|t| *t > now);
            if cooling.is_none() && (s.inflight as f64) < s.limit.floor().max(1.0) {
                s.inflight += 1;
                return Permit(self);
            }
            s = match cooling {
                Some(t) => self.cv.wait_timeout(s, t - now).unwrap_or_else(|e| e.into_inner()).0,
                None => self.cv.wait(s).unwrap_or_else(|e| e.into_inner()),
            };
        }
    }

    fn on_success(&self) {
        let mut s = self.st.lock().unwrap_or_else(|e| e.into_inner());
        if s.limit < self.max {
            s.limit = (s.limit + 1.0 / s.limit).min(self.max);
            self.cv.notify_all();
        }
    }

    /// `wait` is the jittered cool-down chosen by the caller.
    fn on_throttle(&self, wait: Duration) {
        let mut s = self.st.lock().unwrap_or_else(|e| e.into_inner());
        s.throttled += 1;
        let now = Instant::now();
        // One decrease per cool-down: the 503s of one burst arrive together.
        if s.cool_until.is_none_or(|t| t <= now) {
            s.limit = (s.limit / 2.0).max(1.0);
            s.cool_until = Some(now + wait);
        }
    }
}

/// An error that means "the store is shedding load; nothing was applied".
fn is_throttle(e: &BackendError) -> bool {
    matches!(e, BackendError::Unavailable(m)
        if m.contains("SlowDown") || m.contains("503") || m.contains("429") || m.contains("Throttl"))
}

#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// Extra attempts after the first (0 = never retry).
    pub retries: u32,
    pub base: Duration,
    pub cap: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        let retries = std::env::var("VCS_REMOTE_RETRIES")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(3)
            .min(10);
        Self {
            retries,
            base: Duration::from_millis(100),
            cap: Duration::from_secs(2),
        }
    }
}

pub struct RetryBackend {
    inner: Box<dyn Backend>,
    policy: RetryPolicy,
    rng: AtomicU64,
    retried: AtomicU64,
    throttle: Option<Throttle>,
}

impl RetryBackend {
    pub fn new(inner: Box<dyn Backend>) -> Self {
        Self::with_policy(inner, RetryPolicy::default())
    }
    pub fn with_policy(inner: Box<dyn Backend>, policy: RetryPolicy) -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        Self {
            inner,
            policy,
            rng: AtomicU64::new(seed | 1),
            retried: AtomicU64::new(0),
            throttle: Throttle::new(
                std::env::var("VCS_REMOTE_MAX_CONCURRENCY")
                    .ok()
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(64),
            ),
        }
    }
    /// Set the cap of concurrent requests through this backend (0 = no shared throttle). The cap shrinks when the
    /// store answers 503 SlowDown and grows back on success; see the module notes on AIMD.
    pub fn with_max_concurrency(mut self, max: usize) -> Self {
        self.throttle = Throttle::new(max);
        self
    }
    /// Requests answered with a throttle error (503 / SlowDown / 429) while the shared throttle was on.
    pub fn throttled(&self) -> u64 {
        self.throttle
            .as_ref()
            .map_or(0, |t| t.st.lock().unwrap_or_else(|e| e.into_inner()).throttled)
    }
    /// Current concurrency limit of the shared throttle, if enabled.
    pub fn concurrency_limit(&self) -> Option<usize> {
        self.throttle
            .as_ref()
            .map(|t| t.st.lock().unwrap_or_else(|e| e.into_inner()).limit.floor() as usize)
    }

    /// One request to the inner backend, under the shared throttle.
    fn call<T>(&self, f: impl FnOnce() -> Result<T, BackendError>) -> Result<T, BackendError> {
        let Some(t) = &self.throttle else { return f() };
        let _p = t.acquire();
        let r = f();
        match &r {
            Err(e) if is_throttle(e) => {
                let d = (self.policy.base.as_millis() as u64)
                    .saturating_mul(4)
                    .min(self.policy.cap.as_millis() as u64)
                    .max(2);
                t.on_throttle(Duration::from_millis(d / 2 + self.jitter(d / 2)));
            }
            Ok(_) => t.on_success(),
            _ => {}
        }
        r
    }
    /// How many times an operation was repeated after a transient failure.
    pub fn retried(&self) -> u64 {
        self.retried.load(Ordering::Relaxed)
    }

    fn jitter(&self, max_ms: u64) -> u64 {
        let mut z = self
            .rng
            .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
            .wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) % (max_ms + 1)
    }

    /// Sleep before retry `n` (0-based): half fixed, half random ("equal jitter"), capped.
    fn pause(&self, n: u32) {
        self.retried.fetch_add(1, Ordering::Relaxed);
        let d = (self.policy.base.as_millis() as u64)
            .saturating_mul(1u64 << n.min(20))
            .min(self.policy.cap.as_millis() as u64);
        if d > 0 {
            std::thread::sleep(Duration::from_millis(d / 2 + self.jitter(d / 2)));
        }
    }

    /// Repeat an idempotent operation on `Unavailable`.
    fn idem<T>(&self, f: impl Fn() -> Result<T, BackendError>) -> Result<T, BackendError> {
        let mut n = 0;
        loop {
            match f() {
                Err(BackendError::Unavailable(_)) if n < self.policy.retries => {
                    self.pause(n);
                    n += 1;
                }
                r => return r,
            }
        }
    }
}

impl Backend for RetryBackend {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        self.idem(|| self.call(|| self.inner.get(key)))
    }
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        self.idem(|| self.call(|| self.inner.get_conditional(key, have)))
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.idem(|| self.call(|| self.inner.get_range(key, offset, len)))
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        self.idem(|| self.call(|| self.inner.exists(key)))
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        self.idem(|| self.call(|| self.inner.list(prefix)))
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        self.idem(|| self.call(|| self.inner.delete(key)))
    }

    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        let mut n = 0;
        loop {
            match self.call(|| self.inner.put_if_absent(key, data)) {
                Err(BackendError::Unavailable(why)) if n < self.policy.retries => {
                    self.pause(n);
                    n += 1;
                    // The write may have applied. Look before repeating.
                    match self.idem(|| self.call(|| self.inner.get(key))) {
                        Ok(None) => {}                   // did not apply: repeat
                        Ok(Some(_)) => return Ok(false), // exists (ours or not): conservative answer
                        Err(_) => return Err(BackendError::Unavailable(why)),
                    }
                }
                r => return r,
            }
        }
    }

    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        let mut n = 0;
        loop {
            match self.call(|| self.inner.put_if_match(key, data, tag)) {
                Err(BackendError::Unavailable(why)) if n < self.policy.retries => {
                    self.pause(n);
                    n += 1;
                    match self.idem(|| self.call(|| self.inner.get(key))) {
                        Ok(None) => return Ok(None),
                        Ok(Some(v)) if v.tag == tag => {} // unchanged: did not apply, repeat
                        Ok(Some(v)) if v.data == data => return Ok(Some(v.tag)),
                        Ok(Some(_)) => return Ok(None),
                        Err(_) => return Err(BackendError::Unavailable(why)),
                    }
                }
                r => return r,
            }
        }
    }

    fn supports_gc(&self) -> bool {
        self.inner.supports_gc()
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        self.idem(|| self.call(|| self.inner.now_ms()))
    }
    fn modified_ms(&self, key: &str) -> Result<Option<u64>, BackendError> {
        self.idem(|| self.call(|| self.inner.modified_ms(key)))
    }
    fn touch(&self, key: &str) -> Result<bool, BackendError> {
        self.idem(|| self.call(|| self.inner.touch(key)))
    }
}
