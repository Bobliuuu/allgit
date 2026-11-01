//! Latency injection and request timelines for any [`Backend`].
//!
//! [`TimelineBackend`] sleeps a fixed time per call (emulating an object store that is far away),
//! records the start and end of every call, and can append each call to a trace file. From the
//! timeline, [`depth`] computes the sequential depth: the longest chain of calls in which each one
//! started only after the previous one had finished. That is the number of network round trips a
//! client really waits for, which request counts alone hide. Tests assert it instead of timings.

use crate::backend::{Backend, BackendError, Conditional, Versioned};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One recorded backend call. Times are microseconds since the backend was created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub op: &'static str,
    pub key: String,
    pub start_us: u64,
    pub end_us: u64,
}

pub struct TimelineBackend {
    inner: Arc<dyn Backend>,
    latency: Duration,
    t0: Instant,
    events: Mutex<Vec<Event>>,
    sink: Mutex<Option<std::fs::File>>,
}

impl TimelineBackend {
    pub fn new(inner: Arc<dyn Backend>, latency: Duration) -> Self {
        Self {
            inner,
            latency,
            t0: Instant::now(),
            events: Mutex::new(Vec::new()),
            sink: Mutex::new(None),
        }
    }
    /// Append one line per call (`op<TAB>start_us<TAB>end_us<TAB>key`) to `path` as calls finish.
    pub fn with_trace_file(self, path: &std::path::Path) -> std::io::Result<Self> {
        let f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        *self.sink.lock().unwrap() = Some(f);
        Ok(self)
    }
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }
    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }
    pub fn requests(&self) -> usize {
        self.events.lock().unwrap().len()
    }
    pub fn depth(&self) -> usize {
        depth(&self.events.lock().unwrap())
    }
    fn run<T>(&self, op: &'static str, key: &str, f: impl FnOnce() -> T) -> T {
        let start = self.t0.elapsed().as_micros() as u64;
        if !self.latency.is_zero() {
            std::thread::sleep(self.latency);
        }
        let r = f();
        let end = self.t0.elapsed().as_micros() as u64;
        let ev = Event {
            op,
            key: key.to_owned(),
            start_us: start,
            end_us: end,
        };
        if let Some(s) = self.sink.lock().unwrap().as_mut() {
            let _ = writeln!(s, "{op}\t{start}\t{end}\t{key}");
        }
        self.events.lock().unwrap().push(ev);
        r
    }
}

/// Longest chain of calls where each starts at or after the end of the previous one.
/// Fully parallel calls have depth 1; N strictly sequential calls have depth N.
pub fn depth(events: &[Event]) -> usize {
    let mut by_start: Vec<&Event> = events.iter().collect();
    by_start.sort_by_key(|e| e.start_us);
    let mut best: Vec<usize> = Vec::with_capacity(by_start.len());
    for (i, e) in by_start.iter().enumerate() {
        let prev = (0..i)
            .filter(|&j| by_start[j].end_us <= e.start_us)
            .map(|j| best[j])
            .max()
            .unwrap_or(0);
        best.push(prev + 1);
    }
    best.into_iter().max().unwrap_or(0)
}

impl Backend for TimelineBackend {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        self.run("GET", key, || self.inner.get(key))
    }
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        self.run("GET-INM", key, || self.inner.get_conditional(key, have))
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.run("RANGE", key, || self.inner.get_range(key, offset, len))
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        self.run("HEAD", key, || self.inner.exists(key))
    }
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        self.run("PUT-NEW", key, || self.inner.put_if_absent(key, data))
    }
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        self.run("PUT-CAS", key, || self.inner.put_if_match(key, data, tag))
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        self.run("LIST", prefix, || self.inner.list(prefix))
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        self.run("DELETE", key, || self.inner.delete(key))
    }
    fn supports_gc(&self) -> bool {
        self.inner.supports_gc()
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        self.inner.now_ms()
    }
    fn modified_ms(&self, key: &str) -> Result<Option<u64>, BackendError> {
        self.run("MTIME", key, || self.inner.modified_ms(key))
    }
    fn touch(&self, key: &str) -> Result<bool, BackendError> {
        self.run("TOUCH", key, || self.inner.touch(key))
    }
}

/// A shared handle is a backend too, so one timeline can sit under several wrappers.
impl Backend for Arc<TimelineBackend> {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        (**self).get(key)
    }
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        (**self).get_conditional(key, have)
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        (**self).get_range(key, offset, len)
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        (**self).exists(key)
    }
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        (**self).put_if_absent(key, data)
    }
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        (**self).put_if_match(key, data, tag)
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        (**self).list(prefix)
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        (**self).delete(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemBackend;

    #[test]
    fn depth_counts_sequential_chains_not_parallel_calls() {
        let ev = |s, e| Event {
            op: "GET",
            key: String::new(),
            start_us: s,
            end_us: e,
        };
        assert_eq!(depth(&[]), 0);
        assert_eq!(depth(&[ev(0, 10), ev(0, 10), ev(1, 9)]), 1);
        assert_eq!(depth(&[ev(0, 10), ev(10, 20), ev(20, 30)]), 3);
        assert_eq!(depth(&[ev(0, 10), ev(0, 12), ev(12, 20), ev(11, 19)]), 2);
    }

    #[test]
    fn records_calls_and_sleeps() {
        let t = TimelineBackend::new(Arc::new(MemBackend::new()), Duration::from_millis(5));
        t.put_if_absent("a/b", b"x").unwrap();
        t.get("a/b").unwrap();
        assert_eq!((t.requests(), t.depth()), (2, 2));
        assert!(t.events()[0].end_us - t.events()[0].start_us >= 5000);
    }
}
