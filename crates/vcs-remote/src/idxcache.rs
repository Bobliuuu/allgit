//! A client-side cache of remote pack indexes.
//!
//! A remote pack is named `packs/pack-<hex>` where `<hex>` is the pack's checksum, so the pack and
//! its `.idx` never change once written. Every clone or fetch used to download every index of
//! the remote again (44 bytes per object: about 0.9 MB for a 10k-file repository) just to learn
//! where the few new objects are. [`IdxCacheBackend`] wraps any [`Backend`] and answers
//! `GET packs/pack-<hex>.idx` from a local directory when it can.
//!
//! The cache is advisory and self-checking: a cached file is used only if its blake3 trailer is
//! intact and the pack checksum recorded in it equals `<hex>` from the key; anything else is
//! deleted and fetched again. Files are written to a temp name and renamed, so a crash leaves
//! either nothing or a complete file. Objects read through a cached index are still verified
//! against their ids, and the pack trailer is still compared with the index's checksum.

use crate::backend::{Backend, BackendError, Conditional, Versioned};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Wraps a backend; see the module docs.
pub struct IdxCacheBackend<'a> {
    inner: &'a dyn Backend,
    dir: PathBuf,
    seq: AtomicU64,
}

/// Cached indexes kept per directory; older ones (by modification time) are dropped beyond this.
const KEEP: usize = 256;

impl<'a> IdxCacheBackend<'a> {
    pub fn new(inner: &'a dyn Backend, dir: impl Into<PathBuf>) -> Self {
        Self {
            inner,
            dir: dir.into(),
            seq: AtomicU64::new(0),
        }
    }

    /// `Some(hex)` if `key` is `packs/pack-<64 hex>.idx`.
    fn stem_hex(key: &str) -> Option<&str> {
        let h = key.strip_prefix("packs/pack-")?.strip_suffix(".idx")?;
        (h.len() == 64 && h.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))).then_some(h)
    }

    /// Whether `data` is a complete index of the pack whose checksum is `hex`.
    fn valid(data: &[u8], hex: &str) -> bool {
        if data.len() < 32 + 32 {
            return false;
        }
        let (body, sum) = data.split_at(data.len() - 32);
        if blake3::hash(body).as_bytes() != sum {
            return false;
        }
        vcs_store::pack::PackIndex::parse(data, Path::new(hex))
            .map(|i| vcs_objects::ObjectId::from_bytes(*i.checksum()).to_hex() == hex)
            .unwrap_or(false)
    }

    fn store(&self, hex: &str, data: &[u8]) {
        let _ = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(&self.dir)?;
            let tmp = self.dir.join(format!(
                ".{hex}.{}.{}.tmp",
                std::process::id(),
                self.seq.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::write(&tmp, data)?;
            std::fs::rename(&tmp, self.dir.join(format!("{hex}.idx")))
        })();
    }

    /// Drop the oldest cached files beyond [`KEEP`]. Best effort.
    pub fn prune(&self) {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return };
        let mut v: Vec<_> = rd
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".idx"))
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        if v.len() > KEEP {
            v.sort();
            for (_, p) in &v[..v.len() - KEEP] {
                let _ = std::fs::remove_file(p);
            }
        }
    }
}

impl Backend for IdxCacheBackend<'_> {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        let Some(hex) = Self::stem_hex(key) else {
            return self.inner.get(key);
        };
        let path = self.dir.join(format!("{hex}.idx"));
        if let Ok(data) = std::fs::read(&path) {
            if Self::valid(&data, hex) {
                return Ok(Some(Versioned { data, tag: hex.to_owned() }));
            }
            let _ = std::fs::remove_file(&path);
        }
        let got = self.inner.get(key)?;
        if let Some(v) = &got {
            if Self::valid(&v.data, hex) {
                self.store(hex, &v.data);
            }
        }
        Ok(got)
    }
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        self.inner.get_conditional(key, have)
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.inner.get_range(key, offset, len)
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        self.inner.exists(key)
    }
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        self.inner.put_if_absent(key, data)
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
    use crate::{LoggingBackend, MemBackend};
    use vcs_objects::ObjectId;

    /// A real index body for a tiny pack, with its name.
    fn sample() -> (String, Vec<u8>) {
        let o = vec![(vcs_objects::blob_id(b"a"), vcs_objects::Object::Blob(b"a".to_vec()).encode())];
        let b = vcs_store::pack::build_pack(o);
        (b.name.strip_prefix("pack-").unwrap().to_owned(), b.idx)
    }

    #[test]
    fn second_read_is_local_and_tampering_is_refetched() {
        let (hex, idx) = sample();
        let key = format!("packs/pack-{hex}.idx");
        let mem = MemBackend::new();
        mem.put_if_absent(&key, &idx).unwrap();
        let d = tempfile::tempdir().unwrap();
        let log = LoggingBackend::new(&mem);
        let c = IdxCacheBackend::new(&log, d.path());
        assert_eq!(c.get(&key).unwrap().unwrap().data, idx);
        assert_eq!(c.get(&key).unwrap().unwrap().data, idx);
        assert_eq!(log.calls().len(), 1, "second read served from the cache: {:?}", log.calls());
        // damaged cache file: rejected, replaced from the remote
        let f = d.path().join(format!("{hex}.idx"));
        let mut bad = idx.clone();
        bad[40] ^= 1;
        std::fs::write(&f, &bad).unwrap();
        assert_eq!(c.get(&key).unwrap().unwrap().data, idx);
        assert_eq!(log.calls().len(), 2);
        assert_eq!(std::fs::read(&f).unwrap(), idx, "repaired");
        // an index filed under the wrong name is never trusted or stored
        let other = format!("packs/pack-{}.idx", ObjectId::from_bytes([7; 32]).to_hex());
        mem.put_if_absent(&other, &idx).unwrap();
        c.get(&other).unwrap();
        assert!(!d.path().join(format!("{}.idx", ObjectId::from_bytes([7; 32]).to_hex())).exists());
    }

    #[test]
    fn other_keys_pass_through() {
        let mem = MemBackend::new();
        mem.put_if_absent("packs/x.idx", b"zz").unwrap();
        let d = tempfile::tempdir().unwrap();
        let c = IdxCacheBackend::new(&mem, d.path());
        assert_eq!(c.get("packs/x.idx").unwrap().unwrap().data, b"zz");
        assert!(std::fs::read_dir(d.path()).map(|r| r.count()).unwrap_or(0) == 0);
    }
}
