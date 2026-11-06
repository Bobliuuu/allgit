//! Minimal capability set required of an authoritative backend.
//!
//! Required: read-after-write for new keys, range reads, create-only PUT, and
//! compare-and-swap PUT keyed on an opaque version tag (S3: `If-None-Match: *` / `If-Match`).
//! A provider lacking either conditional primitive must not be used as authoritative.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    /// Transient: the operation may or may not have taken effect. Retry is safe
    /// because every protocol step is idempotent.
    #[error("backend unavailable: {0}")]
    Unavailable(String),
    #[error("backend error: {0}")]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Versioned {
    pub data: Vec<u8>,
    /// Opaque version identifier (ETag-like); only compared for equality.
    pub tag: String,
}

/// A byte range that may be a window into a larger shared buffer (a memoized pack): serving it
/// copies nothing. Derefs to `[u8]`.
#[derive(Debug, Clone)]
pub struct SharedBytes {
    buf: std::sync::Arc<Vec<u8>>,
    start: usize,
    end: usize,
}

impl SharedBytes {
    pub fn from_vec(v: Vec<u8>) -> Self {
        Self::from_vec_arc(std::sync::Arc::new(v))
    }
    pub fn from_vec_arc(buf: std::sync::Arc<Vec<u8>>) -> Self {
        let end = buf.len();
        Self { buf, start: 0, end }
    }
    /// `buf[start..end]`, both clamped to the buffer.
    pub fn window(buf: std::sync::Arc<Vec<u8>>, start: usize, end: usize) -> Self {
        let end = end.min(buf.len());
        Self {
            start: start.min(end),
            end,
            buf,
        }
    }
    /// Take the bytes out; copies only when the buffer is shared or this is a window.
    pub fn into_vec(self) -> Vec<u8> {
        if self.start == 0 && self.end == self.buf.len() {
            match std::sync::Arc::try_unwrap(self.buf) {
                Ok(v) => return v,
                Err(a) => return a[..].to_vec(),
            }
        }
        self.buf[self.start..self.end].to_vec()
    }
}

impl std::ops::Deref for SharedBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.buf[self.start..self.end]
    }
}

/// Result of a conditional read ([`Backend::get_conditional`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conditional {
    /// The key still has the caller's version tag: no body was transferred (HTTP 304).
    NotModified,
    /// The key exists with a different tag: the new content.
    Modified(Versioned),
    /// The key does not exist.
    Missing,
}

/// A large object opened for streaming: its length and version tag are known up front, the bytes are read
/// in windows from `reader` (exactly `len` of them), so a server need not hold the whole object in memory.
pub struct ObjectStream {
    pub len: u64,
    pub tag: String,
    pub reader: Box<dyn std::io::Read + Send>,
}

pub trait Backend: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError>;
    /// Open `key` for a windowed read if the backend can supply its length without loading it and it is at
    /// least `min_len` bytes. `Ok(None)` means "not streamed" (missing, smaller, or unsupported): the caller
    /// falls back to [`Backend::get`]. The default never streams (object stores such as S3 would need a HEAD
    /// plus a ranged/streaming GET body; see docs/known-limits.md).
    fn get_stream(&self, _key: &str, _min_len: u64) -> Result<Option<ObjectStream>, BackendError> {
        Ok(None)
    }
    /// Read `key` only if its version tag differs from `have` (S3/HTTP: `If-None-Match`, answered
    /// 304 with no body when equal). The default is a plain [`Backend::get`] compared locally, so
    /// a backend without the primitive is still correct, just not cheaper. Used for freshness
    /// probes of the manifest; never for compare-and-swap.
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        Ok(match self.get(key)? {
            None => Conditional::Missing,
            Some(v) if v.tag == have => Conditional::NotModified,
            Some(v) => Conditional::Modified(v),
        })
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError>;
    /// Like [`Backend::get_range`] but the bytes may be shared with a cache, so a server can send
    /// them without copying. The default wraps `get_range`.
    fn get_range_shared(&self, key: &str, offset: u64, len: u64) -> Result<Option<SharedBytes>, BackendError> {
        Ok(self.get_range(key, offset, len)?.map(SharedBytes::from_vec))
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError>;
    /// Create-only. `Ok(true)` created, `Ok(false)` key already existed (unchanged).
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError>;
    /// Replace iff the current version tag equals `tag`. `Ok(Some(new_tag))` on success,
    /// `Ok(None)` on precondition failure (including key missing).
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError>;
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError>;

    /// **DESTRUCTIVE.** Remove `key`. Idempotent: deleting an absent key succeeds. Only garbage
    /// collection (`gc` module) may call it, and only inside the tombstone handshake.
    ///
    /// The delete is *unconditional*: S3 (and the other backends here) cannot say "delete only
    /// if unmodified since version V" (S3 has no conditional DeleteObject on ETag or
    /// LastModified), so a concurrent touch cannot make the delete fail. That is why GC needs
    /// the handshake of docs/gc.md (protocol G1) instead of check-then-delete.
    fn delete(&self, _key: &str) -> Result<(), BackendError> {
        Err(BackendError::Other("this backend does not support delete".into()))
    }

    /// True if this backend provides everything remote GC needs: [`Backend::delete`],
    /// [`Backend::now_ms`], [`Backend::modified_ms`] and [`Backend::touch`]. Publishers perform
    /// the GC handshake (touch, tombstone check, hold-time fence) only against such backends;
    /// `vcs gc --remote` refuses any other.
    fn supports_gc(&self) -> bool {
        false
    }
    /// Kernel-released exclusive lock `name` for same-host backends (`FsBackend`). A holder that is
    /// SIGKILLed releases it, so the GC lease can be taken over immediately instead of waiting out
    /// its TTL. Default: unsupported (S3 etc. rely on the lease alone).
    fn local_flock(&self, _name: &str) -> Result<LocalFlock, BackendError> {
        Ok(LocalFlock::Unsupported)
    }
    /// The backend's own clock in unix ms. Remote GC never uses the client's clock.
    fn now_ms(&self) -> Result<u64, BackendError> {
        Err(BackendError::Other("backend has no trusted clock".into()))
    }
    /// Last write or touch time of `key` on the backend's clock (unix ms); `None` if absent.
    fn modified_ms(&self, _key: &str) -> Result<Option<u64>, BackendError> {
        Err(BackendError::Other("backend cannot report object ages".into()))
    }
    /// Refresh `key`'s modification time to now (data and version tag unchanged). `Ok(false)`
    /// if the key does not exist (the caller must then treat the object as gone).
    fn touch(&self, _key: &str) -> Result<bool, BackendError> {
        Err(BackendError::Other("backend cannot touch objects".into()))
    }
}

/// Keys come from our own code, but backends also receive them from remote metadata;
/// reject anything that could escape a filesystem root or confuse a key space.
pub fn validate_key(key: &str) -> Result<(), BackendError> {
    let bad = key.is_empty()
        || key.len() > 512
        || key.starts_with('/')
        || key
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == ".." || c.contains('\\') || c.chars().any(char::is_control));
    if bad {
        Err(BackendError::Other(format!("invalid key {key:?}")))
    } else {
        Ok(())
    }
}

/// Shared conformance suite run against every backend implementation.
pub fn conformance(b: &dyn Backend) {
    assert_eq!(b.get("k/one").unwrap(), None);
    assert!(!b.exists("k/one").unwrap());
    assert!(b.put_if_absent("k/one", b"v1").unwrap());
    assert!(!b.put_if_absent("k/one", b"other").unwrap(), "create-only must not overwrite");
    let v = b.get("k/one").unwrap().unwrap();
    assert_eq!(v.data, b"v1");
    assert_eq!(b.get_range("k/one", 1, 1).unwrap().unwrap(), b"1");
    assert_eq!(b.get_range("k/one", 0, 100).unwrap().unwrap(), b"v1");
    assert_eq!(b.get_range("k/missing", 0, 1).unwrap(), None);
    // range edge cases every backend must agree on
    assert_eq!(b.get_range("k/one", 0, 0).unwrap().unwrap(), b"", "zero length");
    assert_eq!(b.get_range("k/one", 1, 100).unwrap().unwrap(), b"1", "range clamps at end");
    assert_eq!(b.get_range("k/one", 2, 5).unwrap().unwrap(), b"", "offset at end");
    assert_eq!(b.get_range("k/one", 50, 5).unwrap().unwrap(), b"", "offset past end");
    // CAS
    let t2 = b.put_if_match("k/one", b"v2", &v.tag).unwrap().expect("match succeeds");
    assert_eq!(b.put_if_match("k/one", b"v3", &v.tag).unwrap(), None, "stale tag rejected");
    let cur = b.get("k/one").unwrap().unwrap();
    assert_eq!((cur.data.as_slice(), cur.tag.as_str()), (&b"v2"[..], t2.as_str()));
    assert_eq!(
        b.put_if_match("k/none", b"x", "whatever").unwrap(),
        None,
        "CAS on missing key fails"
    );
    b.put_if_absent("k/two", b"2").unwrap();
    b.put_if_absent("z/three", b"3").unwrap();
    assert_eq!(b.list("k/").unwrap(), vec!["k/one", "k/two"]);
    assert!(b.get("../escape").is_err());
    assert!(b.put_if_absent("/abs", b"x").is_err());
    conformance_conditional(b);
    conformance_delete(b);
}

/// Conditional-GET semantics every backend must share (part of [`conformance`]).
pub fn conformance_conditional(b: &dyn Backend) {
    assert_eq!(b.get_conditional("cg/missing", "anything").unwrap(), Conditional::Missing);
    assert!(b.put_if_absent("cg/k", b"one").unwrap());
    let v1 = b.get("cg/k").unwrap().unwrap();
    assert_eq!(b.get_conditional("cg/k", &v1.tag).unwrap(), Conditional::NotModified);
    assert_eq!(
        b.get_conditional("cg/k", "no-such-tag").unwrap(),
        Conditional::Modified(v1.clone()),
        "a different tag returns the current content and tag"
    );
    let t2 = b.put_if_match("cg/k", b"two", &v1.tag).unwrap().unwrap();
    match b.get_conditional("cg/k", &v1.tag).unwrap() {
        Conditional::Modified(v) => assert_eq!((v.data.as_slice(), v.tag.as_str()), (&b"two"[..], t2.as_str())),
        other => panic!("stale tag must see the new version, got {other:?}"),
    }
    assert_eq!(b.get_conditional("cg/k", &t2).unwrap(), Conditional::NotModified);
    assert!(b.get_conditional("../escape", "x").is_err());
}

/// Delete semantics every backend must share (part of [`conformance`]).
pub fn conformance_delete(b: &dyn Backend) {
    assert!(b.put_if_absent("del/a", b"1").unwrap());
    assert!(b.put_if_absent("del/b", b"2").unwrap());
    b.delete("del/a").unwrap();
    assert!(!b.exists("del/a").unwrap());
    assert_eq!(b.get("del/a").unwrap(), None);
    assert_eq!(b.list("del/").unwrap(), vec!["del/b"], "only the deleted key disappears");
    b.delete("del/a").unwrap(); // idempotent
    b.delete("del/never").unwrap();
    // a deleted key can be created again (create-only works on a fresh key)
    assert!(b.put_if_absent("del/a", b"again").unwrap());
    assert_eq!(b.get("del/a").unwrap().unwrap().data, b"again");
    // CAS on a deleted key fails
    let t = b.get("del/b").unwrap().unwrap().tag;
    b.delete("del/b").unwrap();
    assert_eq!(b.put_if_match("del/b", b"x", &t).unwrap(), None);
    assert!(b.delete("../escape").is_err());
    b.delete("del/a").unwrap();
}

/// Extra conformance for backends that claim [`Backend::supports_gc`]: a trusted clock,
/// modification times, and touch.
pub fn conformance_gc(b: &dyn Backend) {
    assert!(b.supports_gc());
    assert_eq!(b.modified_ms("gc/missing").unwrap(), None);
    assert!(
        !b.touch("gc/missing").unwrap(),
        "touch of a missing key reports false and creates nothing"
    );
    assert!(!b.exists("gc/missing").unwrap());
    assert!(b.put_if_absent("gc/k", b"data").unwrap());
    let m0 = b.modified_ms("gc/k").unwrap().expect("present key has a time");
    let now = b.now_ms().unwrap();
    assert!(now + 5_000 >= m0, "modified time is on the backend clock");
    let tag = b.get("gc/k").unwrap().unwrap().tag;
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(b.touch("gc/k").unwrap());
    let m1 = b.modified_ms("gc/k").unwrap().unwrap();
    assert!(m1 > m0, "touch must advance the modified time ({m0} -> {m1})");
    let v = b.get("gc/k").unwrap().unwrap();
    assert_eq!(
        (v.data.as_slice(), v.tag.as_str()),
        (&b"data"[..], tag.as_str()),
        "touch changes neither data nor tag"
    );
    assert!(b.now_ms().unwrap() >= m1);
    b.delete("gc/k").unwrap();
    assert_eq!(b.modified_ms("gc/k").unwrap(), None);
}

/// Result of [`Backend::local_flock`].
pub enum LocalFlock {
    Unsupported,
    /// A live process holds it.
    Held,
    /// Acquired; released when dropped (or when the process dies).
    Acquired(Box<dyn std::any::Any + Send>),
}
