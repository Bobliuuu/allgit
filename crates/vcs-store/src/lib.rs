//! Object store abstraction + in-memory and local-filesystem backends.
//!
//! The trait is synchronous for now: the local backends are plain file I/O and the core
//! must not depend on Tokio. The S3 backend (Milestone 3) will implement an async twin
//! at the network boundary; see docs/architecture.md §Storage.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use vcs_objects::{DecodeError, Object, ObjectId};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("object {0} not found")]
    NotFound(ObjectId),
    #[error("object {id} is corrupt: {reason}")]
    Corrupt { id: ObjectId, reason: String },
    #[error("object decode error: {0}")]
    Decode(#[from] DecodeError),
    #[error("i/o error at {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
}

fn io<T>(path: &Path, r: std::io::Result<T>) -> Result<T, StoreError> {
    r.map_err(|source| StoreError::Io {
        path: path.to_owned(),
        source,
    })
}

pub trait ObjectStore: Send + Sync {
    /// Fetch and **verify** (hash must equal `id`) an object.
    fn get(&self, id: &ObjectId) -> Result<Object, StoreError>;
    /// Store an object; idempotent. Returns its id.
    ///
    /// Durability: the object is readable through this store immediately, but is only
    /// guaranteed to survive a crash once [`ObjectStore::sync`] has returned. Callers must call
    /// `sync()` after their last `put` and **before** writing any ref, index or manifest that
    /// names the object.
    fn put(&self, obj: &Object) -> Result<ObjectId, StoreError>;
    /// Durability barrier: when this returns, every object `put` so far is complete and
    /// durable under its final name. Cheap no-op when nothing is pending.
    fn sync(&self) -> Result<(), StoreError> {
        Ok(())
    }
    fn contains(&self, id: &ObjectId) -> Result<bool, StoreError>;
    /// Enumerate all stored ids (fsck / GC mark phase). Not for hot paths.
    fn list(&self) -> Result<Vec<ObjectId>, StoreError>;
    /// When the object was written, if the store can tell. `None` means unknown, and callers
    /// that delete (GC) must then treat the object as brand new and keep it.
    fn modified(&self, _id: &ObjectId) -> Result<Option<std::time::SystemTime>, StoreError> {
        Ok(None)
    }

    fn put_blob(&self, data: &[u8]) -> Result<ObjectId, StoreError> {
        self.put(&Object::Blob(data.to_vec()))
    }
    fn get_blob(&self, id: &ObjectId) -> Result<Vec<u8>, StoreError> {
        match self.get(id)? {
            Object::Blob(b) => Ok(b),
            _ => Err(StoreError::Corrupt {
                id: *id,
                reason: "expected blob".into(),
            }),
        }
    }
}

fn decode_verified(id: &ObjectId, bytes: &[u8]) -> Result<Object, StoreError> {
    let actual = vcs_objects::id_of_encoded(bytes);
    if &actual != id {
        return Err(StoreError::Corrupt {
            id: *id,
            reason: format!("hash mismatch (content hashes to {actual})"),
        });
    }
    Ok(Object::decode(bytes)?)
}

/// Test backend.
#[derive(Default)]
pub struct MemoryStore {
    objects: Mutex<BTreeMap<ObjectId, Vec<u8>>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
    /// Test hook: overwrite raw bytes to simulate corruption.
    pub fn corrupt(&self, id: &ObjectId, bytes: Vec<u8>) {
        self.objects.lock().unwrap().insert(*id, bytes);
    }
}

impl ObjectStore for MemoryStore {
    fn get(&self, id: &ObjectId) -> Result<Object, StoreError> {
        let g = self.objects.lock().unwrap();
        let b = g.get(id).ok_or(StoreError::NotFound(*id))?;
        decode_verified(id, b)
    }
    fn put(&self, obj: &Object) -> Result<ObjectId, StoreError> {
        let enc = obj.encode();
        let id = vcs_objects::id_of_encoded(&enc);
        self.objects.lock().unwrap().entry(id).or_insert(enc);
        Ok(id)
    }
    fn contains(&self, id: &ObjectId) -> Result<bool, StoreError> {
        Ok(self.objects.lock().unwrap().contains_key(id))
    }
    fn list(&self) -> Result<Vec<ObjectId>, StoreError> {
        Ok(self.objects.lock().unwrap().keys().copied().collect())
    }
}

/// Loose-object filesystem store: `<dir>/<aa>/<rest-of-hex>`.
///
/// Durability model (batched, see docs/architecture.md "Durability"):
/// - `put` writes the encoded object to `<dir>/tmp/` with **no fsync** and remembers it as
///   pending. Nothing appears under a final object name yet, so a crash can never leave a
///   partial or zero-length file where an object is expected.
/// - `sync` is the barrier: flush every pending file's data, one hardware flush, rename the
///   files to their final names, flush the touched directories, one more hardware flush.
///   Only after it returns may a ref/index/manifest point at the objects.
/// - Pending objects are readable via `get`/`contains` from this store instance meanwhile.
///
/// Concurrent writers of the same id are safe (identical content; rename is atomic).
pub struct LocalStore {
    dir: PathBuf,
    /// id -> tmp file written but not yet published.
    pending: Mutex<HashMap<ObjectId, PathBuf>>,
    seq: AtomicU64,
}

/// Push a file's data to the device. On macOS `File::sync_all` is `F_FULLFSYNC` (flushes the
/// drive cache, very slow); plain `fsync` only hands data to the drive, so the batch uses it
/// per file and pays for ONE full flush per barrier phase instead (git's `core.fsync=batch`
/// does the same). Elsewhere `fsync` already is the real flush.
fn flush_file(f: &fs::File) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: the fd is valid for the duration of the call.
        if unsafe { libc::fsync(f.as_raw_fd()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        f.sync_all()
    }
}

/// Make everything handed to the drive so far durable (hardware cache flush). Only needed on
/// macOS, where `flush_file` does not do it; `sync_all` there is `F_FULLFSYNC`.
fn hardware_flush(f: &fs::File) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        f.sync_all()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = f;
        Ok(())
    }
}

impl LocalStore {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let dir = dir.into();
        io(&dir, fs::create_dir_all(dir.join("tmp")))?;
        Ok(Self {
            dir,
            pending: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
        })
    }
    fn path_of(&self, id: &ObjectId) -> PathBuf {
        let h = id.to_hex();
        self.dir.join(&h[..2]).join(&h[2..])
    }
}

impl ObjectStore for LocalStore {
    fn get(&self, id: &ObjectId) -> Result<Object, StoreError> {
        let pending = self.pending.lock().unwrap().get(id).cloned();
        let p = pending.unwrap_or_else(|| self.path_of(id));
        match fs::read(&p) {
            Ok(b) => decode_verified(id, &b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StoreError::NotFound(*id)),
            Err(source) => Err(StoreError::Io { path: p, source }),
        }
    }

    fn put(&self, obj: &Object) -> Result<ObjectId, StoreError> {
        let enc = obj.encode();
        let id = vcs_objects::id_of_encoded(&enc);
        // Existing objects are never rewritten: one stat (or a map lookup), no write I/O.
        if self.pending.lock().unwrap().contains_key(&id) || self.path_of(&id).exists() {
            return Ok(id);
        }
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let tmp = self.dir.join("tmp").join(format!("{}.{}.{n}.tmp", id.to_hex(), std::process::id()));
        {
            let mut f = io(&tmp, fs::File::create(&tmp))?;
            io(&tmp, f.write_all(&enc))?;
        }
        self.pending.lock().unwrap().insert(id, tmp);
        Ok(id)
    }

    fn sync(&self) -> Result<(), StoreError> {
        // Held for the whole barrier so readers never observe "neither pending nor published".
        let mut pending = self.pending.lock().unwrap();
        if pending.is_empty() {
            return Ok(());
        }
        // 1. object data to the device, then one hardware flush covering all of it.
        let mut last = None;
        for tmp in pending.values() {
            let f = io(tmp, fs::File::open(tmp))?;
            io(tmp, flush_file(&f))?;
            last = Some(f);
        }
        if let Some(f) = last {
            io(&self.dir, hardware_flush(&f))?;
        }
        // 2. publish under final names (data is already durable, so a visible name is complete).
        let mut dirs = BTreeSet::new();
        for (id, tmp) in pending.iter() {
            let dest = self.path_of(id);
            let parent = dest.parent().expect("object path has parent").to_owned();
            if dirs.insert(parent.clone()) {
                io(&parent, fs::create_dir_all(&parent))?;
            }
            if dest.exists() {
                let _ = fs::remove_file(tmp); // another writer won the race; same content
            } else {
                io(&dest, fs::rename(tmp, &dest))?;
            }
        }
        // 3. directory entries (renames, new fan-out dirs) durable before anything refers to them.
        dirs.insert(self.dir.clone());
        dirs.insert(self.dir.join("tmp"));
        let mut last = None;
        for d in &dirs {
            let f = io(d, fs::File::open(d))?;
            io(d, flush_file(&f))?;
            last = Some(f);
        }
        if let Some(f) = last {
            io(&self.dir, hardware_flush(&f))?;
        }
        pending.clear();
        Ok(())
    }

    fn contains(&self, id: &ObjectId) -> Result<bool, StoreError> {
        Ok(self.pending.lock().unwrap().contains_key(id) || self.path_of(id).exists())
    }

    fn modified(&self, id: &ObjectId) -> Result<Option<std::time::SystemTime>, StoreError> {
        // Written but not yet published objects are, by definition, brand new.
        if self.pending.lock().unwrap().contains_key(id) {
            return Ok(Some(std::time::SystemTime::now()));
        }
        let p = self.path_of(id);
        match fs::metadata(&p) {
            Ok(m) => Ok(m.modified().ok()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StoreError::NotFound(*id)),
            Err(source) => Err(StoreError::Io { path: p, source }),
        }
    }

    fn list(&self) -> Result<Vec<ObjectId>, StoreError> {
        let mut out = Vec::new();
        for d in io(&self.dir, fs::read_dir(&self.dir))? {
            let d = io(&self.dir, d)?;
            let name = d.file_name().to_string_lossy().into_owned();
            if name.len() != 2 || !d.path().is_dir() {
                continue;
            }
            for f in io(&d.path(), fs::read_dir(d.path()))? {
                let f = io(&d.path(), f)?;
                let rest = f.file_name().to_string_lossy().into_owned();
                if let Ok(id) = ObjectId::from_hex(&format!("{name}{rest}")) {
                    out.push(id);
                }
            }
        }
        out.sort();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exercise(s: &dyn ObjectStore) {
        let id = s.put_blob(b"hello").unwrap();
        assert_eq!(s.put_blob(b"hello").unwrap(), id, "idempotent");
        assert!(s.contains(&id).unwrap());
        assert_eq!(s.get_blob(&id).unwrap(), b"hello");
        let missing = ObjectId::from_bytes([7; 32]);
        assert!(matches!(s.get(&missing), Err(StoreError::NotFound(_))));
        s.sync().unwrap();
        assert_eq!(s.list().unwrap(), vec![id]);
    }

    #[test]
    fn memory_store() {
        exercise(&MemoryStore::new());
    }

    #[test]
    fn local_store() {
        let d = tempfile::tempdir().unwrap();
        exercise(&LocalStore::open(d.path().join("objects")).unwrap());
    }

    #[test]
    fn memory_detects_corruption() {
        let s = MemoryStore::new();
        let id = s.put_blob(b"hello").unwrap();
        let mut bad = Object::Blob(b"HELLO".to_vec()).encode();
        s.corrupt(&id, bad.clone());
        assert!(matches!(s.get(&id), Err(StoreError::Corrupt { .. })));
        bad.truncate(1);
        s.corrupt(&id, bad);
        assert!(s.get(&id).is_err());
    }

    #[test]
    fn local_detects_corruption_and_bitflip() {
        let d = tempfile::tempdir().unwrap();
        let s = LocalStore::open(d.path().join("o")).unwrap();
        let id = s.put_blob(b"important data").unwrap();
        s.sync().unwrap();
        let p = s.path_of(&id);
        let mut b = fs::read(&p).unwrap();
        *b.last_mut().unwrap() ^= 1;
        fs::write(&p, b).unwrap();
        assert!(matches!(s.get(&id), Err(StoreError::Corrupt { .. })));
    }

    #[test]
    fn local_unsynced_objects_are_not_published_and_synced_ones_are() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("o");
        let (a, b) = {
            let s = LocalStore::open(&dir).unwrap();
            let a = s.put_blob(b"durable").unwrap();
            s.sync().unwrap();
            let b = s.put_blob(b"not yet durable").unwrap();
            // readable in-process before the barrier
            assert_eq!(s.get_blob(&b).unwrap(), b"not yet durable");
            assert!(s.contains(&b).unwrap());
            // "crash": store dropped without sync()
            (a, b)
        };
        let s = LocalStore::open(&dir).unwrap();
        assert_eq!(s.get_blob(&a).unwrap(), b"durable");
        assert!(!s.contains(&b).unwrap(), "unsynced object must not appear under a final name");
        assert!(matches!(s.get(&b), Err(StoreError::NotFound(_))));
        assert_eq!(s.list().unwrap(), vec![a]);
        // re-putting after the crash writes it properly
        assert_eq!(s.put_blob(b"not yet durable").unwrap(), b);
        s.sync().unwrap();
        assert_eq!(s.get_blob(&b).unwrap(), b"not yet durable");
        s.sync().unwrap(); // no-op barrier
    }

    #[test]
    fn local_survives_reopen_and_leftover_tmp() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("o");
        let id = {
            let s = LocalStore::open(&dir).unwrap();
            // simulate crash mid-write: stray tmp file must not be visible as an object
            fs::write(dir.join("tmp").join("junk.tmp"), b"partial").unwrap();
            let id = s.put_blob(b"x").unwrap();
            s.sync().unwrap();
            id
        };
        let s = LocalStore::open(&dir).unwrap();
        assert_eq!(s.get_blob(&id).unwrap(), b"x");
        assert_eq!(s.list().unwrap(), vec![id]);
    }
}
