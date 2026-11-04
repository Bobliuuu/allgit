//! Persistent on-disk warm cache for immutable remote reads (packs' ranges, indexes, objects).
//!
//! Remote keys `packs/pack-<checksum>.{pack,idx}` and `objects/<id>` are content addressed and never
//! change, so their bytes can be kept across processes and served without a request. Every entry is
//! self-checking: a header, the opaque version tag, the data and a blake3 trailer that also covers
//! the cache key, offset and length. A truncated, bit-flipped, foreign or misplaced file fails the
//! check, is deleted and reported as a miss. Callers still hash-verify what they receive, as before.
//!
//! * Writes go to a unique temp name and are renamed into place, so concurrent writers (threads or
//!   processes) leave either nothing or one complete entry; two writers of the same key write the
//!   same bytes.
//! * The cache is bounded: after roughly `cap / 8` bytes written, the least recently used entries
//!   (modification time, refreshed on every hit) are deleted down to 90% of `cap`. Deleting a file
//!   another process is reading is harmless: that reader holds the bytes or sees a miss.
//! * GC-aware: a cache never makes anything reachable. Reachability comes from the live manifest
//!   and listing; `exists` is never answered from here. A cached pack that GC later deletes remotely
//!   is simply never asked for again and ages out.
//! * One namespace directory per remote (`ns`), so two remotes that reuse a key cannot collide.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::SystemTime;

const MAGIC: &[u8; 8] = b"VCSDC001";

pub struct ClientCache {
    dir: PathBuf,
    cap: u64,
    written: AtomicU64,
    seq: AtomicUsize,
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub corrupt: AtomicU64,
}

fn digest(key: &str, off: u64, len: u64, tag: &str, data: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(MAGIC);
    h.update(&(key.len() as u32).to_le_bytes());
    h.update(key.as_bytes());
    h.update(&off.to_le_bytes());
    h.update(&len.to_le_bytes());
    h.update(&(tag.len() as u32).to_le_bytes());
    h.update(tag.as_bytes());
    h.update(data);
    *h.finalize().as_bytes()
}

impl ClientCache {
    /// `root` is the cache directory, `ns` names the remote (any string; it is hashed), `cap` the byte bound.
    pub fn open(root: impl AsRef<Path>, ns: &str, cap: u64) -> std::io::Result<Self> {
        let dir = root.as_ref().join(&blake3::hash(ns.as_bytes()).to_hex()[..24]);
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            cap,
            written: AtomicU64::new(0),
            seq: AtomicUsize::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            corrupt: AtomicU64::new(0),
        })
    }

    fn path(&self, key: &str, off: u64, len: u64) -> PathBuf {
        let mut h = blake3::Hasher::new();
        h.update(key.as_bytes());
        h.update(&[0]);
        h.update(&off.to_le_bytes());
        h.update(&len.to_le_bytes());
        let hex = h.finalize().to_hex();
        self.dir.join(&hex[..2]).join(&hex[2..])
    }

    /// Whole value (`len == u64::MAX`) or one range; returns `(data, tag)`.
    pub fn get(&self, key: &str, off: u64, len: u64) -> Option<(Vec<u8>, String)> {
        let p = self.path(key, off, len);
        let raw = match fs::read(&p) {
            Ok(r) => r,
            Err(_) => {
                self.misses.fetch_add(1, Relaxed);
                return None;
            }
        };
        match Self::decode(&raw, key, off, len) {
            Some(v) => {
                self.hits.fetch_add(1, Relaxed);
                if let Ok(f) = fs::OpenOptions::new().write(true).open(&p) {
                    let _ = f.set_modified(SystemTime::now());
                }
                Some(v)
            }
            None => {
                self.corrupt.fetch_add(1, Relaxed);
                self.misses.fetch_add(1, Relaxed);
                let _ = fs::remove_file(&p);
                None
            }
        }
    }

    fn decode(raw: &[u8], key: &str, off: u64, len: u64) -> Option<(Vec<u8>, String)> {
        if raw.len() < 8 + 4 + 32 || &raw[..8] != MAGIC {
            return None;
        }
        let tl = u32::from_le_bytes(raw[8..12].try_into().ok()?) as usize;
        let body = raw.len() - 32;
        if 12 + tl > body {
            return None;
        }
        let tag = std::str::from_utf8(&raw[12..12 + tl]).ok()?;
        let data = &raw[12 + tl..body];
        if len != u64::MAX && data.len() as u64 > len {
            return None;
        }
        if digest(key, off, len, tag, data)[..] != raw[body..] {
            return None;
        }
        Some((data.to_vec(), tag.to_owned()))
    }

    /// Best effort: any I/O error just means the entry is not cached.
    pub fn put(&self, key: &str, off: u64, len: u64, tag: &str, data: &[u8]) {
        if data.len() as u64 > self.cap / 4 {
            return;
        }
        let p = self.path(key, off, len);
        let Some(parent) = p.parent() else { return };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let tmp = parent.join(format!(".tmp-{}-{}", std::process::id(), self.seq.fetch_add(1, Relaxed)));
        let mut buf = Vec::with_capacity(data.len() + tag.len() + 52);
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&(tag.len() as u32).to_le_bytes());
        buf.extend_from_slice(tag.as_bytes());
        buf.extend_from_slice(data);
        buf.extend_from_slice(&digest(key, off, len, tag, data));
        let ok = fs::File::create(&tmp).and_then(|mut f| f.write_all(&buf)).is_ok() && fs::rename(&tmp, &p).is_ok();
        if !ok {
            let _ = fs::remove_file(&tmp);
            return;
        }
        if self.written.fetch_add(buf.len() as u64, Relaxed) + buf.len() as u64 > self.cap / 8 {
            self.written.store(0, Relaxed);
            self.evict();
        }
    }

    /// Delete least recently used entries until the cache is at most 90% of its cap. Also removes stale temp files.
    pub fn evict(&self) {
        let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        let now = SystemTime::now();
        let Ok(rd) = fs::read_dir(&self.dir) else { return };
        for sub in rd.flatten() {
            let Ok(inner) = fs::read_dir(sub.path()) else { continue };
            for e in inner.flatten() {
                let Ok(m) = e.metadata() else { continue };
                if !m.is_file() {
                    continue;
                }
                let mt = m.modified().unwrap_or(now);
                if e.file_name().to_string_lossy().starts_with(".tmp-") {
                    if now.duration_since(mt).map(|d| d.as_secs() > 3600).unwrap_or(false) {
                        let _ = fs::remove_file(e.path());
                    }
                    continue;
                }
                files.push((mt, m.len(), e.path()));
            }
        }
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        if total <= self.cap {
            return;
        }
        files.sort();
        let target = self.cap / 10 * 9;
        for (_, n, p) in files {
            if total <= target {
                break;
            }
            if fs::remove_file(&p).is_ok() {
                total -= n;
            }
        }
    }

    /// Bytes currently on disk (entries only); for tests and `vcs` diagnostics.
    pub fn size(&self) -> u64 {
        let mut t = 0;
        if let Ok(rd) = fs::read_dir(&self.dir) {
            for sub in rd.flatten() {
                if let Ok(inner) = fs::read_dir(sub.path()) {
                    t += inner
                        .flatten()
                        .filter_map(|e| e.metadata().ok())
                        .filter(|m| m.is_file())
                        .map(|m| m.len())
                        .sum::<u64>();
                }
            }
        }
        t
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn cache(cap: u64) -> (tempfile::TempDir, ClientCache) {
        let d = tempfile::tempdir().unwrap();
        let c = ClientCache::open(d.path(), "remote-a", cap).unwrap();
        (d, c)
    }

    #[test]
    fn hit_and_miss() {
        let (_d, c) = cache(1 << 20);
        assert!(c.get("objects/a", 0, u64::MAX).is_none());
        c.put("objects/a", 0, u64::MAX, "t1", b"hello");
        assert_eq!(c.get("objects/a", 0, u64::MAX), Some((b"hello".to_vec(), "t1".into())));
        c.put("packs/p.pack", 4, 3, "", b"abc");
        assert_eq!(c.get("packs/p.pack", 4, 3).unwrap().0, b"abc");
        assert!(c.get("packs/p.pack", 4, 4).is_none());
        assert_eq!((c.hits.load(Relaxed), c.misses.load(Relaxed)), (2, 2));
    }

    #[test]
    fn corrupt_truncated_and_swapped_entries_are_misses_and_removed() {
        let (_d, c) = cache(1 << 20);
        c.put("objects/a", 0, u64::MAX, "t", b"payload-bytes");
        let p = c.path("objects/a", 0, u64::MAX);
        let mut raw = fs::read(&p).unwrap();
        let mid = raw.len() / 2;
        raw[mid] ^= 1;
        fs::write(&p, &raw).unwrap();
        assert!(c.get("objects/a", 0, u64::MAX).is_none());
        assert!(!p.exists(), "corrupt entry deleted");
        c.put("objects/a", 0, u64::MAX, "t", b"payload-bytes");
        let raw = fs::read(&p).unwrap();
        fs::write(&p, &raw[..raw.len() - 5]).unwrap();
        assert!(c.get("objects/a", 0, u64::MAX).is_none());
        // a valid entry for another key moved into this slot is rejected (key is in the digest)
        c.put("objects/b", 0, u64::MAX, "t", b"other");
        fs::copy(c.path("objects/b", 0, u64::MAX), &p).unwrap();
        assert!(c.get("objects/a", 0, u64::MAX).is_none());
        assert_eq!(c.corrupt.load(Relaxed), 3);
        // and it recovers
        c.put("objects/a", 0, u64::MAX, "t", b"payload-bytes");
        assert!(c.get("objects/a", 0, u64::MAX).is_some());
    }

    #[test]
    fn namespaces_do_not_collide() {
        let d = tempfile::tempdir().unwrap();
        let a = ClientCache::open(d.path(), "a", 1 << 20).unwrap();
        let b = ClientCache::open(d.path(), "b", 1 << 20).unwrap();
        a.put("objects/x", 0, u64::MAX, "", b"A");
        assert!(b.get("objects/x", 0, u64::MAX).is_none());
    }

    #[test]
    fn concurrent_writers_and_readers_never_see_a_partial_entry() {
        let (_d, c) = cache(64 << 20);
        let c = Arc::new(c);
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::thread::scope(|s| {
            for _ in 0..8 {
                let c = c.clone();
                let data = &data;
                s.spawn(move || {
                    for _ in 0..30 {
                        c.put("objects/big", 0, u64::MAX, "t", data);
                        if let Some((d, t)) = c.get("objects/big", 0, u64::MAX) {
                            assert_eq!((d.len(), t.as_str()), (data.len(), "t"));
                            assert_eq!(&d, data);
                        }
                    }
                });
            }
        });
        assert_eq!(c.corrupt.load(Relaxed), 0);
        assert_eq!(c.get("objects/big", 0, u64::MAX).unwrap().0, data);
    }

    #[test]
    fn eviction_bounds_size_and_keeps_recently_used() {
        let (_d, c) = cache(100_000);
        let blob = vec![7u8; 10_000];
        c.put("objects/keep", 0, u64::MAX, "", &blob);
        for i in 0..40 {
            c.put(&format!("objects/{i}"), 0, u64::MAX, "", &blob);
            std::thread::sleep(std::time::Duration::from_millis(3));
            assert!(c.get("objects/keep", 0, u64::MAX).is_some() || i < 8, "hot entry survives (i={i})");
        }
        c.evict();
        assert!(c.size() <= 100_000, "size {}", c.size());
        assert!(c.get("objects/keep", 0, u64::MAX).is_some());
        assert!(c.get("objects/0", 0, u64::MAX).is_none(), "oldest cold entry evicted");
    }
}
