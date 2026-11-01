//! Request and byte accounting for any [`Backend`], used by `vcs fetch --stats`.

use crate::backend::{Backend, BackendError, Conditional, Versioned};
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
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        self.req();
        let r = self.inner.get_conditional(key, have)?;
        if let Conditional::Modified(v) = &r {
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
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        self.req();
        self.inner.delete(key)
    }
    fn supports_gc(&self) -> bool {
        self.inner.supports_gc()
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        self.inner.now_ms()
    }
    fn modified_ms(&self, key: &str) -> Result<Option<u64>, BackendError> {
        self.req();
        self.inner.modified_ms(key)
    }
    fn touch(&self, key: &str) -> Result<bool, BackendError> {
        self.req();
        self.inner.touch(key)
    }
}

/// Wraps a backend and records `METHOD key` for every call, in order, so a test can print or
/// assert the request breakdown of an operation.
pub struct LoggingBackend<'a> {
    inner: &'a dyn Backend,
    log: std::sync::Mutex<Vec<String>>,
}

impl<'a> LoggingBackend<'a> {
    pub fn new(inner: &'a dyn Backend) -> Self {
        Self {
            inner,
            log: Default::default(),
        }
    }
    fn note(&self, method: &str, key: &str) {
        self.log.lock().unwrap().push(format!("{method} {key}"));
    }
    /// All calls so far.
    pub fn calls(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
    /// Number of calls whose record starts with `prefix` (e.g. `"GET manifest"`).
    pub fn count(&self, prefix: &str) -> usize {
        self.log.lock().unwrap().iter().filter(|l| l.starts_with(prefix)).count()
    }
    pub fn clear(&self) {
        self.log.lock().unwrap().clear();
    }
    /// Print the breakdown (use with `--nocapture`).
    pub fn dump(&self, what: &str) {
        let l = self.log.lock().unwrap();
        println!("{what}: {} requests", l.len());
        for x in l.iter() {
            println!("    {x}");
        }
    }
}

impl Backend for LoggingBackend<'_> {
    fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
        self.note("GET", k);
        self.inner.get(k)
    }
    fn get_conditional(&self, k: &str, have: &str) -> Result<Conditional, BackendError> {
        self.note("GET-IF-NONE-MATCH", k);
        self.inner.get_conditional(k, have)
    }
    fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.note(&format!("RANGE[{o}+{l}]"), k);
        self.inner.get_range(k, o, l)
    }
    fn exists(&self, k: &str) -> Result<bool, BackendError> {
        self.note("HEAD", k);
        self.inner.exists(k)
    }
    fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
        self.note("PUT-NEW", k);
        self.inner.put_if_absent(k, d)
    }
    fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
        self.note("PUT-CAS", k);
        self.inner.put_if_match(k, d, t)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
        self.note("LIST", p);
        self.inner.list(p)
    }
    fn delete(&self, k: &str) -> Result<(), BackendError> {
        self.note("DELETE", k);
        self.inner.delete(k)
    }
    fn supports_gc(&self) -> bool {
        self.inner.supports_gc()
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        self.inner.now_ms()
    }
    fn modified_ms(&self, k: &str) -> Result<Option<u64>, BackendError> {
        self.note("HEAD-MTIME", k);
        self.inner.modified_ms(k)
    }
    fn touch(&self, k: &str) -> Result<bool, BackendError> {
        self.note("TOUCH", k);
        self.inner.touch(k)
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
