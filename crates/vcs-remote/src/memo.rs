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

use crate::backend::{Backend, BackendError, Conditional, SharedBytes, Versioned};
use crate::manifest;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use vcs_objects::id_of_encoded;

type Range = (String, u64, u64);

pub struct MemoBackend {
    inner: Box<dyn Backend>,
    snapshot: bool,
    imm: Mutex<HashMap<String, Versioned>>,
    ranges: Mutex<HashMap<Range, Arc<Vec<u8>>>>,
    /// Whole packs this process just wrote (write-through), served for any range.
    packs: Mutex<HashMap<String, Arc<Vec<u8>>>>,
    live: Mutex<Live>,
    /// Bytes held; when a new entry would pass `cap` everything is dropped first (a memo, not a store).
    held: std::sync::atomic::AtomicUsize,
    cap: usize,
    write_through: bool,
    disk: Option<std::sync::Arc<crate::clientcache::ClientCache>>,
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
            disk: None,
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

    /// Add a persistent disk layer under the memory memo: immutable reads (indexes, objects, pack
    /// ranges) are served from it when intact and written to it after a miss.
    pub fn with_disk(mut self, disk: std::sync::Arc<crate::clientcache::ClientCache>) -> Self {
        self.disk = Some(disk);
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

    /// Write-through: the pack this process just wrote and its index (`idx_key`, `idx`) are both
    /// held, so remember the commits and trees in it as if they were loose objects. Validating the
    /// publish that follows then reads a new commit from memory, not by listing and probing the
    /// store. Only small packs and small objects (a clone-sized pack is not worth the copy); every
    /// object is hash-verified before it is remembered. Best effort: anything odd is skipped.
    fn index_new_pack(&self, idx_key: &str, idx: &[u8]) {
        const MAX_PACK: usize = 4 << 20;
        const MAX_OBJECT: u32 = 256 << 10;
        let Some(stem) = idx_key.strip_suffix(".idx") else { return };
        let pack = {
            let g = self.packs.lock().unwrap();
            match g.get(&format!("{stem}.pack")) {
                Some(p) if p.len() <= MAX_PACK => p.clone(),
                _ => return,
            }
        };
        let Ok(index) = vcs_store::pack::PackIndex::parse(idx, std::path::Path::new(stem)) else {
            return;
        };
        if index.pack_len != pack.len() as u64 {
            return;
        }
        for (id, off, len) in index.iter() {
            let (Some(end), true) = (off.checked_add(len as u64), len <= MAX_OBJECT) else {
                continue;
            };
            let Some(payload) = pack.get(off as usize..end as usize) else {
                continue;
            };
            let Ok(enc) = index.decode_entry(&id, payload) else { continue };
            // commits (3) and trees (2): what a validator reads. Blobs are never needed here.
            if !matches!(enc.first(), Some(2 | 3)) || id_of_encoded(&enc) != id || !self.room(enc.len()) {
                continue;
            }
            let v = Versioned {
                tag: blake3::hash(&enc).to_hex().to_string(),
                data: enc.into_owned(),
            };
            self.imm.lock().unwrap().insert(crate::protocol::object_key(&id), v);
        }
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
            if let Some(d) = &self.disk {
                if let Some((data, tag)) = d.get(key, 0, u64::MAX) {
                    let v = Versioned { data, tag };
                    if self.room(v.data.len()) {
                        self.imm.lock().unwrap().insert(key.to_owned(), v.clone());
                    }
                    return Ok(Some(v));
                }
            }
            let r = self.inner.get(key)?;
            match &r {
                Some(v) => {
                    if let Some(d) = &self.disk {
                        d.put(key, 0, u64::MAX, &v.tag, &v.data);
                    }
                    if self.room(v.data.len()) {
                        self.imm.lock().unwrap().insert(key.to_owned(), v.clone());
                    }
                }
                None if key.starts_with("packs/") => self.drop_live(),
                None => {}
            }
            return Ok(r);
        }
        if let (Some(d), true) = (&self.disk, key.starts_with("packs/") && key.ends_with(".pack")) {
            // whole-pack read (small packs of a clone): disk only, the catalog holds it in memory.
            // Callers verify the pack trailer against the index checksum, as before.
            if let Some((data, tag)) = d.get(key, 0, u64::MAX) {
                return Ok(Some(Versioned { data, tag }));
            }
            let r = self.inner.get(key)?;
            match &r {
                Some(v) => d.put(key, 0, u64::MAX, &v.tag, &v.data),
                None => self.drop_live(),
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
        Ok(self.get_range_shared(key, offset, len)?.map(SharedBytes::into_vec))
    }
    fn get_range_shared(&self, key: &str, offset: u64, len: u64) -> Result<Option<SharedBytes>, BackendError> {
        if !(key.starts_with("packs/") && key.ends_with(".pack")) {
            return Ok(self.inner.get_range(key, offset, len)?.map(SharedBytes::from_vec));
        }
        let held = self.packs.lock().unwrap().get(key).cloned();
        if let Some(p) = held {
            let end = offset.saturating_add(len).min(p.len() as u64) as usize;
            return Ok(Some(SharedBytes::window(p, (offset as usize).min(end), end)));
        }
        let k = (key.to_owned(), offset, len);
        let hit = self.ranges.lock().unwrap().get(&k).cloned();
        if let Some(v) = hit {
            return Ok(Some(SharedBytes::from_vec_arc(v)));
        }
        if let Some(d) = &self.disk {
            if let Some((data, _)) = d.get(key, offset, len) {
                let a = Arc::new(data);
                if self.room(a.len()) {
                    self.ranges.lock().unwrap().insert(k, a.clone());
                }
                return Ok(Some(SharedBytes::from_vec_arc(a)));
            }
        }
        let r = self.inner.get_range(key, offset, len)?;
        match r {
            Some(v) => {
                // a short answer is the clamped end of an immutable pack: the same (offset, len)
                // always answers the same bytes, so the disk cache may keep it too
                if let Some(d) = &self.disk {
                    d.put(key, offset, len, "", &v);
                }
                let a = Arc::new(v);
                if a.len() as u64 == len && self.room(a.len()) {
                    self.ranges.lock().unwrap().insert(k, a.clone());
                }
                Ok(Some(SharedBytes::from_vec_arc(a)))
            }
            None => {
                self.drop_live();
                Ok(None)
            }
        }
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
                self.packs.lock().unwrap().insert(key.to_owned(), Arc::new(data.to_vec()));
            } else if immutable(key) {
                // content-addressed keys: callers never compare their tags; any opaque tag will do
                self.imm.lock().unwrap().insert(
                    key.to_owned(),
                    Versioned {
                        data: data.to_vec(),
                        tag: blake3::hash(data).to_hex().to_string(),
                    },
                );
                if key.starts_with("packs/") {
                    self.index_new_pack(key, data);
                }
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
    fn get_range_shared(&self, key: &str, offset: u64, len: u64) -> Result<Option<SharedBytes>, BackendError> {
        (**self).get_range_shared(key, offset, len)
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

    #[test]
    fn disk_layer_makes_a_second_process_send_no_immutable_requests() {
        let m = MemBackend::new();
        m.put_if_absent("packs/p.idx", b"idx").unwrap();
        m.put_if_absent("packs/p.pack", b"0123456789").unwrap();
        m.put_if_absent("objects/o", b"obj").unwrap();
        let tl = Arc::new(TimelineBackend::new(Arc::new(m), Duration::ZERO));
        let dir = tempfile::tempdir().unwrap();
        let mk = || {
            let d = Arc::new(crate::ClientCache::open(dir.path(), "r", 1 << 20).unwrap());
            MemoBackend::new(Box::new(tl.clone()), false).with_disk(d)
        };
        let run = |b: &MemoBackend| {
            assert_eq!(b.get("packs/p.idx").unwrap().unwrap().data, b"idx");
            assert_eq!(b.get("objects/o").unwrap().unwrap().data, b"obj");
            assert_eq!(b.get_range("packs/p.pack", 2, 3).unwrap().unwrap(), b"234");
            assert_eq!(b.get("packs/p.pack").unwrap().unwrap().data, b"0123456789");
        };
        run(&mk());
        assert_eq!(tl.requests(), 4);
        run(&mk()); // fresh memo, same disk: zero new requests
        assert_eq!(tl.requests(), 4);
        // a corrupted entry is refetched, not trusted
        for e in walk(dir.path()) {
            let mut raw = std::fs::read(&e).unwrap();
            let n = raw.len() - 1;
            raw[n] ^= 1;
            std::fs::write(&e, raw).unwrap();
        }
        run(&mk());
        assert_eq!(tl.requests(), 8);
    }

    fn walk(p: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut v = Vec::new();
        for e in std::fs::read_dir(p).unwrap().flatten() {
            if e.path().is_dir() {
                v.extend(walk(&e.path()));
            } else {
                v.push(e.path());
            }
        }
        v
    }
}

#[cfg(test)]
mod shared_tests {
    use super::*;
    use crate::MemBackend;

    #[test]
    fn written_pack_ranges_are_windows_without_copies_and_exact() {
        let pack: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let b = MemoBackend::new(Box::new(MemBackend::new()), false).with_write_through(1 << 20);
        b.put_if_absent("packs/p.pack", &pack).unwrap();
        for (o, l) in [(0u64, 10_000u64), (3, 4000), (9_990, 50), (10_000, 5), (20_000, 5)] {
            let s = b.get_range_shared("packs/p.pack", o, l).unwrap().unwrap();
            let end = ((o + l) as usize).min(pack.len());
            let start = (o as usize).min(end);
            assert_eq!(&s[..], &pack[start..end], "range {o}+{l}");
            assert_eq!(b.get_range("packs/p.pack", o, l).unwrap().unwrap(), pack[start..end].to_vec());
        }
    }

    #[test]
    fn read_through_range_is_memoized_once_and_equal_on_hit() {
        let inner = MemBackend::new();
        let pack: Vec<u8> = (0..4096u32).map(|i| (i * 7 % 256) as u8).collect();
        inner.put_if_absent("packs/q.pack", &pack).unwrap();
        let b = MemoBackend::new(Box::new(inner), false).with_write_through(1 << 20);
        let a = b.get_range_shared("packs/q.pack", 100, 1000).unwrap().unwrap();
        let c = b.get_range_shared("packs/q.pack", 100, 1000).unwrap().unwrap();
        assert_eq!(&a[..], &pack[100..1100]);
        assert_eq!(&a[..], &c[..]);
        assert!(b.get_range_shared("packs/none.pack", 0, 4).unwrap().is_none());
    }
}
