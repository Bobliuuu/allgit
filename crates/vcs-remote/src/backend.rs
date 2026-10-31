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

pub trait Backend: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError>;
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError>;
    fn exists(&self, key: &str) -> Result<bool, BackendError>;
    /// Create-only. `Ok(true)` created, `Ok(false)` key already existed (unchanged).
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError>;
    /// Replace iff the current version tag equals `tag`. `Ok(Some(new_tag))` on success,
    /// `Ok(None)` on precondition failure (including key missing).
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError>;
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError>;
}

/// Keys come from our own code, but backends also receive them from remote metadata;
/// reject anything that could escape a filesystem root or confuse a key space.
pub fn validate_key(key: &str) -> Result<(), BackendError> {
    let bad = key.is_empty()
        || key.len() > 512
        || key.starts_with('/')
        || key
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == ".." || c.contains('\\') || c.contains('\0'));
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
}
