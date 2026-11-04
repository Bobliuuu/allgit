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
use std::time::Duration;

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
        }
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
        self.idem(|| self.inner.get(key))
    }
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        self.idem(|| self.inner.get_conditional(key, have))
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.idem(|| self.inner.get_range(key, offset, len))
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        self.idem(|| self.inner.exists(key))
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        self.idem(|| self.inner.list(prefix))
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        self.idem(|| self.inner.delete(key))
    }

    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        let mut n = 0;
        loop {
            match self.inner.put_if_absent(key, data) {
                Err(BackendError::Unavailable(why)) if n < self.policy.retries => {
                    self.pause(n);
                    n += 1;
                    // The write may have applied. Look before repeating.
                    match self.idem(|| self.inner.get(key)) {
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
            match self.inner.put_if_match(key, data, tag) {
                Err(BackendError::Unavailable(why)) if n < self.policy.retries => {
                    self.pause(n);
                    n += 1;
                    match self.idem(|| self.inner.get(key)) {
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
        self.idem(|| self.inner.now_ms())
    }
    fn modified_ms(&self, key: &str) -> Result<Option<u64>, BackendError> {
        self.idem(|| self.inner.modified_ms(key))
    }
    fn touch(&self, key: &str) -> Result<bool, BackendError> {
        self.idem(|| self.inner.touch(key))
    }
}
