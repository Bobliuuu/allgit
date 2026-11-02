//! A persistent, self-checking disk cache for immutable remote reads (server side).
//!
//! `packs/*.idx`, ranged reads of `packs/*.pack` and `objects/*` are content-addressed and never
//! change once written, so their bytes can be kept on local disk across process restarts. A
//! restarted server (or a second server on the same host) then answers a warm clone from disk and
//! only asks the object store for the live state (manifest and pack listing): about 2 store
//! requests instead of one per index and pack block.
//!
//! The cache is advisory and verified: every entry file ends with a 32-byte blake3 over
//! `(key, offset, len, data)`, checked on every read; any mismatch (torn write, bit rot, a file
//! renamed by hand) deletes the entry and falls through to the store. Callers still hash-verify
//! the objects they decode. Entries are written to a temp name and renamed. The manifest, listings
//! and every write, delete and existence check always go to the inner backend; nothing here can
//! make a compare-and-swap decision.
//!
//! Size is bounded by `cap` bytes: past it, the oldest entries (by modification time) are removed
//! down to 80% of the cap. An entry larger than half the cap is not cached.

use crate::backend::{Backend, BackendError, Conditional, Versioned};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub struct DiskCacheBackend {
    inner: Arc<dyn Backend>,
    dir: PathBuf,
    cap: u64,
    size: AtomicU64,
    seq: AtomicU64,
    prune_lock: Mutex<()>,
    hits: AtomicU64,
}

const WHOLE: u64 = u64::MAX;

fn cacheable_whole(key: &str) -> bool {
    (key.starts_with("packs/") && key.ends_with(".idx")) || key.starts_with("objects/")
}
fn cacheable_range(key: &str) -> bool {
    key.starts_with("packs/") && key.ends_with(".pack")
}

fn digest(key: &str, off: u64, len: u64, data: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(&(key.len() as u64).to_le_bytes());
    h.update(key.as_bytes());
    h.update(&off.to_le_bytes());
    h.update(&len.to_le_bytes());
    h.update(data);
    *h.finalize().as_bytes()
}

impl DiskCacheBackend {
    pub fn new(inner: Arc<dyn Backend>, dir: impl Into<PathBuf>, cap: u64) -> std::io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        let mut size = 0;
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.ends_with(".tmp") {
                    let _ = std::fs::remove_file(e.path()); // leftover of a crash
                } else if let Ok(m) = e.metadata() {
                    size += m.len();
                }
            }
        }
        Ok(Self {
            inner,
            dir,
            cap,
            size: AtomicU64::new(size),
            seq: AtomicU64::new(0),
            prune_lock: Mutex::new(()),
            hits: AtomicU64::new(0),
        })
    }

    /// Reads answered from disk since start.
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    fn path(&self, key: &str, off: u64, len: u64) -> PathBuf {
        let mut h = blake3::Hasher::new();
        h.update(key.as_bytes());
        h.update(&off.to_le_bytes());
        h.update(&len.to_le_bytes());
        self.dir.join(format!("{}.c", &h.finalize().to_hex()[..40]))
    }

    /// Verified bytes of an entry, or `None` (damaged entries are removed).
    fn load(&self, key: &str, off: u64, len: u64) -> Option<Vec<u8>> {
        let p = self.path(key, off, len);
        let mut f = std::fs::read(&p).ok()?;
        if f.len() < 32 {
            let _ = std::fs::remove_file(&p);
            return None;
        }
        let sum = f.split_off(f.len() - 32);
        let ok = (len == 0 || off == WHOLE || f.len() as u64 == len) && digest(key, off, len, &f)[..] == sum[..];
        if !ok {
            let _ = std::fs::remove_file(&p);
            return None;
        }
        self.hits.fetch_add(1, Ordering::Relaxed);
        Some(f)
    }

    fn store(&self, key: &str, off: u64, len: u64, data: &[u8]) {
        let n = data.len() as u64 + 32;
        if n > self.cap / 2 {
            return;
        }
        let tmp = self.dir.join(format!(".{}.{}.tmp", std::process::id(), self.seq.fetch_add(1, Ordering::Relaxed)));
        let r = (|| -> std::io::Result<()> {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(data)?;
            f.write_all(&digest(key, off, len, data))?;
            drop(f);
            std::fs::rename(&tmp, self.path(key, off, len))
        })();
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        if self.size.fetch_add(n, Ordering::Relaxed) + n > self.cap {
            self.prune();
        }
    }

    fn prune(&self) {
        let Ok(_g) = self.prune_lock.try_lock() else { return };
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return };
        let mut v: Vec<_> = rd
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".c"))
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                Some((m.modified().ok()?, m.len(), e.path()))
            })
            .collect();
        let mut total: u64 = v.iter().map(|x| x.1).sum();
        v.sort();
        let goal = self.cap / 10 * 8;
        for (_, n, p) in v {
            if total <= goal {
                break;
            }
            if std::fs::remove_file(&p).is_ok() {
                total -= n;
            }
        }
        self.size.store(total, Ordering::Relaxed);
    }
}

impl Backend for DiskCacheBackend {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        if !cacheable_whole(key) {
            return self.inner.get(key);
        }
        if let Some(data) = self.load(key, WHOLE, 0) {
            let tag = blake3::hash(&data).to_hex().to_string();
            return Ok(Some(Versioned { data, tag }));
        }
        let got = self.inner.get(key)?;
        if let Some(v) = &got {
            self.store(key, WHOLE, 0, &v.data);
        }
        Ok(got)
    }
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        self.inner.get_conditional(key, have)
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        if !cacheable_range(key) || len == 0 {
            return self.inner.get_range(key, offset, len);
        }
        if let Some(d) = self.load(key, offset, len) {
            return Ok(Some(d));
        }
        let got = self.inner.get_range(key, offset, len)?;
        // only complete answers are cached: a short read means the range ran past the end
        if let Some(d) = &got {
            if d.len() as u64 == len {
                self.store(key, offset, len, d);
            }
        }
        Ok(got)
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        self.inner.exists(key)
    }
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        let created = self.inner.put_if_absent(key, data)?;
        // content-addressed and just written: the next clone through this server finds it on disk
        if created && cacheable_whole(key) {
            self.store(key, WHOLE, 0, data);
        }
        Ok(created)
    }
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        self.inner.put_if_match(key, data, tag)
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        self.inner.list(prefix)
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::TimelineBackend;
    use crate::MemBackend;
    use std::time::Duration;

    fn setup() -> (Arc<dyn Backend>, tempfile::TempDir) {
        let m: Arc<dyn Backend> = Arc::new(MemBackend::new());
        m.put_if_absent("packs/p.idx", b"index-bytes").unwrap();
        m.put_if_absent("packs/p.pack", b"0123456789abcdef").unwrap();
        m.put_if_absent("objects/o", b"obj").unwrap();
        m.put_if_absent("manifest", b"m1").unwrap();
        (m, tempfile::tempdir().unwrap())
    }

    #[test]
    fn warm_reads_survive_a_restart_and_live_keys_never_cache() {
        let (m, d) = setup();
        for round in 0..2 {
            let log = Arc::new(TimelineBackend::new(m.clone(), Duration::ZERO));
            let c = DiskCacheBackend::new(log.clone(), d.path(), 1 << 20).unwrap();
            assert_eq!(c.get("packs/p.idx").unwrap().unwrap().data, b"index-bytes");
            assert_eq!(c.get("objects/o").unwrap().unwrap().data, b"obj");
            assert_eq!(c.get_range("packs/p.pack", 2, 5).unwrap().unwrap(), b"23456");
            assert_eq!(c.get("manifest").unwrap().unwrap().data, b"m1");
            assert_eq!(c.list("packs/").unwrap().len(), 2);
            let n = log.requests();
            // round 0 cold: 5 requests; round 1 (new process, same dir): manifest + list only
            assert_eq!(n, if round == 0 { 5 } else { 2 }, "requests {n}");
        }
    }

    #[test]
    fn damaged_or_swapped_entries_are_refetched() {
        let (m, d) = setup();
        let c = DiskCacheBackend::new(m.clone(), d.path(), 1 << 20).unwrap();
        c.get_range("packs/p.pack", 0, 4).unwrap();
        c.get_range("packs/p.pack", 4, 4).unwrap();
        let files: Vec<_> = std::fs::read_dir(d.path()).unwrap().flatten().map(|e| e.path()).collect();
        assert_eq!(files.len(), 2);
        // flip a data byte of one, and put the other's content under the first's name
        let a = std::fs::read(&files[0]).unwrap();
        let mut bad = a.clone();
        bad[0] ^= 1;
        std::fs::write(&files[0], &bad).unwrap();
        std::fs::write(&files[1], &a).unwrap();
        assert_eq!(c.get_range("packs/p.pack", 0, 4).unwrap().unwrap(), b"0123");
        assert_eq!(c.get_range("packs/p.pack", 4, 4).unwrap().unwrap(), b"4567");
    }

    #[test]
    fn short_reads_are_not_cached_and_the_cap_is_enforced() {
        let (m, d) = setup();
        let c = DiskCacheBackend::new(m.clone(), d.path(), 400).unwrap();
        assert_eq!(c.get_range("packs/p.pack", 10, 100).unwrap().unwrap().len(), 6);
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
        for i in 0..40u64 {
            c.get_range("packs/p.pack", i % 8, 4 + (i / 8)).unwrap();
        }
        let total: u64 = std::fs::read_dir(d.path()).unwrap().flatten().map(|e| e.metadata().unwrap().len()).sum();
        assert!(total <= 400, "cache grew to {total}");
    }
}
