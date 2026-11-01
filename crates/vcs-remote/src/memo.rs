//! Per-process read memo for one clone / fetch / sync, to remove repeated identical requests.
//!
//! * Always memoized (content-addressed, immutable): `GET packs/*.idx`, `GET objects/*`, and ranged
//!   reads of `packs/*.pack`. Callers still hash-verify everything they get, as before.
//! * With `snapshot`: also the manifest GET and `LIST packs/`, so the planning phase and the clone
//!   phase of `clone --for` read them once. Any write or delete through this backend drops the
//!   snapshot, and so does any "missing" answer for a pack key (a compaction removed a pack): the
//!   retry that follows then lists live, exactly like without the memo. Never use the snapshot for
//!   publishing: compare-and-swap decisions must see live state.
//! * [`MemoBackend::prime`] reads the manifest and `LIST packs/` concurrently, then the indexes in
//!   parallel, so the dependency chain at the start of a clone is 2 round trips instead of 3.

use crate::backend::{Backend, BackendError, Conditional, Versioned};
use crate::manifest;
use std::collections::HashMap;
use std::sync::Mutex;

type Range = (String, u64, u64);

pub struct MemoBackend {
    inner: Box<dyn Backend>,
    snapshot: bool,
    imm: Mutex<HashMap<String, Versioned>>,
    ranges: Mutex<HashMap<Range, Vec<u8>>>,
    /// Whole packs this process just wrote (write-through), served for any range.
    packs: Mutex<HashMap<String, Vec<u8>>>,
    live: Mutex<Live>,
    /// Bytes held; when a new entry would pass `cap` everything is dropped first (a memo, not a store).
    held: std::sync::atomic::AtomicUsize,
    cap: usize,
    write_through: bool,
}

#[derive(Default)]
struct Live {
    manifest: Option<Versioned>,
    packs: Option<Vec<String>>,
}

fn immutable(key: &str) -> bool {
    (key.starts_with("packs/") && key.ends_with(".idx")) || key.starts_with("objects/")
}

impl MemoBackend {
    pub fn new(inner: Box<dyn Backend>, snapshot: bool) -> Self {
        Self {
            inner,
            snapshot,
            imm: Default::default(),
            ranges: Default::default(),
            packs: Default::default(),
            live: Default::default(),
            held: Default::default(),
            cap: 256 << 20,
            write_through: false,
        }
    }

    /// For a long-lived process (the server): remember immutable objects, indexes and packs this
    /// process writes, so the reads that validate a publish right after hit memory, and bound the
    /// memory to `cap` bytes.
    pub fn with_write_through(mut self, cap: usize) -> Self {
        self.write_through = true;
        self.cap = cap;
        self
    }

    fn room(&self, n: usize) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        if n > self.cap / 4 {
            return false;
        }
        if self.held.load(Relaxed) + n > self.cap {
            self.imm.lock().unwrap().clear();
            self.ranges.lock().unwrap().clear();
            self.packs.lock().unwrap().clear();
            self.held.store(0, Relaxed);
        }
        self.held.fetch_add(n, Relaxed);
        true
    }

    /// Start of a clone: manifest and pack listing concurrently, then every index in parallel.
    /// Best effort: an error is simply not memoized and surfaces again on the real read.
    pub fn prime(&self) {
        if !self.snapshot {
            return;
        }
        let mut keys = Vec::new();
        std::thread::scope(|s| {
            s.spawn(|| {
                let _ = self.get(manifest::KEY);
            });
            keys = self.list("packs/").unwrap_or_default();
        });
        let idx: Vec<&String> = keys.iter().filter(|k| k.ends_with(".idx")).collect();
        let per = idx.len().div_ceil(8).max(1);
        std::thread::scope(|s| {
            for chunk in idx.chunks(per) {
                s.spawn(move || {
                    for k in chunk {
                        let _ = self.get(k);
                    }
                });
            }
        });
    }

    fn drop_live(&self) {
        *self.live.lock().unwrap() = Live::default();
    }
}

impl Backend for MemoBackend {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        if immutable(key) {
            if let Some(v) = self.imm.lock().unwrap().get(key) {
                return Ok(Some(v.clone()));
            }
            let r = self.inner.get(key)?;
            match &r {
                Some(v) => {
                    if self.room(v.data.len()) {
                        self.imm.lock().unwrap().insert(key.to_owned(), v.clone());
                    }
                }
                None if key.starts_with("packs/") => self.drop_live(),
                None => {}
            }
            return Ok(r);
        }
        if self.snapshot && key == manifest::KEY {
            if let Some(v) = self.live.lock().unwrap().manifest.clone() {
                return Ok(Some(v));
            }
            let r = self.inner.get(key)?;
            if let Some(v) = &r {
                self.live.lock().unwrap().manifest = Some(v.clone());
            }
            return Ok(r);
        }
        self.inner.get(key)
    }
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        self.inner.get_conditional(key, have)
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        if !(key.starts_with("packs/") && key.ends_with(".pack")) {
            return self.inner.get_range(key, offset, len);
        }
        if let Some(p) = self.packs.lock().unwrap().get(key) {
            let end = (offset.saturating_add(len)).min(p.len() as u64);
            return Ok(Some(p[(offset.min(end)) as usize..end as usize].to_vec()));
        }
        let k = (key.to_owned(), offset, len);
        if let Some(v) = self.ranges.lock().unwrap().get(&k) {
            return Ok(Some(v.clone()));
        }
        let r = self.inner.get_range(key, offset, len)?;
        match &r {
            Some(v) if v.len() as u64 == len => {
                if self.room(v.len()) {
                    self.ranges.lock().unwrap().insert(k, v.clone());
                }
            }
            Some(_) => {}
            None => self.drop_live(),
        }
        Ok(r)
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        // never answered from memory: GC's existence checks must see the live store
        self.inner.exists(key)
    }
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        self.drop_live();
        let created = self.inner.put_if_absent(key, data)?;
        if created && self.write_through && self.room(data.len()) {
            if key.starts_with("packs/") && key.ends_with(".pack") {
                self.packs.lock().unwrap().insert(key.to_owned(), data.to_vec());
            } else if immutable(key) {
                // content-addressed keys: callers never compare their tags; any opaque tag will do
                self.imm.lock().unwrap().insert(
                    key.to_owned(),
                    Versioned {
                        data: data.to_vec(),
                        tag: blake3::hash(data).to_hex().to_string(),
                    },
                );
            }
        }
        Ok(created)
    }
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        self.drop_live();
        self.inner.put_if_match(key, data, tag)
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        if self.snapshot && prefix == "packs/" {
            if let Some(v) = self.live.lock().unwrap().packs.clone() {
                return Ok(v);
            }
            let r = self.inner.list(prefix)?;
            self.live.lock().unwrap().packs = Some(r.clone());
            return Ok(r);
        }
        self.inner.list(prefix)
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        self.drop_live();
        self.inner.delete(key)
    }
    fn supports_gc(&self) -> bool {
        self.inner.supports_gc()
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        self.inner.now_ms()
    }
    fn modified_ms(&self, key: &str) -> Result<Option<u64>, BackendError> {
        self.inner.modified_ms(key)
    }
    fn touch(&self, key: &str) -> Result<bool, BackendError> {
        self.inner.touch(key)
    }
}

/// A shared trait object is a backend too (lets a server hand its `Arc<dyn Backend>` to a wrapper).
impl Backend for std::sync::Arc<dyn Backend> {
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
    fn supports_gc(&self) -> bool {
        (**self).supports_gc()
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        (**self).now_ms()
    }
    fn modified_ms(&self, key: &str) -> Result<Option<u64>, BackendError> {
        (**self).modified_ms(key)
    }
    fn touch(&self, key: &str) -> Result<bool, BackendError> {
        (**self).touch(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::TimelineBackend;
    use crate::MemBackend;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn repeats_are_served_from_memory_and_writes_drop_the_snapshot() {
        let m = MemBackend::new();
        m.put_if_absent("packs/p.idx", b"i").unwrap();
        m.put_if_absent("packs/p.pack", b"0123456789").unwrap();
        m.put_if_absent(manifest::KEY, b"m").unwrap();
        let tl = Arc::new(TimelineBackend::new(Arc::new(m), Duration::ZERO));
        let b = MemoBackend::new(Box::new(tl.clone()), true);
        for _ in 0..3 {
            b.get("packs/p.idx").unwrap();
            b.get_range("packs/p.pack", 2, 3).unwrap();
            b.list("packs/").unwrap();
            b.get(manifest::KEY).unwrap();
        }
        assert_eq!(tl.requests(), 4);
        b.put_if_absent("x/y", b"1").unwrap();
        b.list("packs/").unwrap();
        assert_eq!(tl.requests(), 6);
    }

    #[test]
    fn prime_reads_manifest_and_listing_in_one_round_then_indexes() {
        let m = MemBackend::new();
        for i in 0..3 {
            m.put_if_absent(&format!("packs/p{i}.idx"), b"i").unwrap();
        }
        m.put_if_absent(manifest::KEY, b"m").unwrap();
        let tl = Arc::new(TimelineBackend::new(Arc::new(m), Duration::from_millis(20)));
        let b = MemoBackend::new(Box::new(tl.clone()), true);
        b.prime();
        assert_eq!((tl.requests(), tl.depth()), (5, 2));
        b.get(manifest::KEY).unwrap();
        b.list("packs/").unwrap();
        b.get("packs/p1.idx").unwrap();
        assert_eq!(tl.requests(), 5);
    }
}
