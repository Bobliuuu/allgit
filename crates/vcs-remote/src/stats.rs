//! Request and byte accounting for any [`Backend`], used by `vcs fetch --stats`.

use crate::backend::{Backend, BackendError, Versioned};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Totals observed through a [`CountingBackend`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransferStats {
    /// Every backend call (GET, ranged GET, HEAD, PUT, LIST).
    pub requests: u64,
    /// Payload bytes returned by GET and ranged GET.
    pub bytes_down: u64,
    /// Payload bytes sent by PUT.
    pub bytes_up: u64,
}

impl fmt::Display for TransferStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "requests: {}, downloaded: {} bytes, uploaded: {} bytes",
            self.requests, self.bytes_down, self.bytes_up
        )
    }
}

/// Wraps a backend and counts calls and payload bytes. Failed calls still count as requests.
pub struct CountingBackend<'a> {
    inner: &'a dyn Backend,
    requests: AtomicU64,
    down: AtomicU64,
    up: AtomicU64,
}

impl<'a> CountingBackend<'a> {
    pub fn new(inner: &'a dyn Backend) -> Self {
        Self {
            inner,
            requests: AtomicU64::new(0),
            down: AtomicU64::new(0),
            up: AtomicU64::new(0),
        }
    }
    pub fn stats(&self) -> TransferStats {
        TransferStats {
            requests: self.requests.load(Ordering::Relaxed),
            bytes_down: self.down.load(Ordering::Relaxed),
            bytes_up: self.up.load(Ordering::Relaxed),
        }
    }
    fn req(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
    }
}

impl Backend for CountingBackend<'_> {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        self.req();
        let r = self.inner.get(key)?;
        if let Some(v) = &r {
            self.down.fetch_add(v.data.len() as u64, Ordering::Relaxed);
        }
        Ok(r)
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.req();
        let r = self.inner.get_range(key, offset, len)?;
        if let Some(v) = &r {
            self.down.fetch_add(v.len() as u64, Ordering::Relaxed);
        }
        Ok(r)
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        self.req();
        self.inner.exists(key)
    }
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        self.req();
        self.up.fetch_add(data.len() as u64, Ordering::Relaxed);
        self.inner.put_if_absent(key, data)
    }
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        self.req();
        self.up.fetch_add(data.len() as u64, Ordering::Relaxed);
        self.inner.put_if_match(key, data, tag)
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        self.req();
        self.inner.list(prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemBackend;

    #[test]
    fn counts_requests_and_bytes() {
        let m = MemBackend::new();
        let c = CountingBackend::new(&m);
        c.put_if_absent("a/b", b"hello").unwrap();
        c.get("a/b").unwrap();
        c.get_range("a/b", 1, 2).unwrap();
        c.get("a/none").unwrap();
        let s = c.stats();
        assert_eq!(
            s,
            TransferStats {
                requests: 4,
                bytes_down: 7,
                bytes_up: 5
            }
        );
        assert_eq!(s.requests, m.requests());
    }
}
