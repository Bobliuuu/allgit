//! Object store abstraction + in-memory and local-filesystem backends.
//!
//! The trait is synchronous for now: the local backends are plain file I/O and the core
//! must not depend on Tokio. The S3 backend (Milestone 3) will implement an async twin
//! at the network boundary; see docs/architecture.md §Storage.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
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
    fn put(&self, obj: &Object) -> Result<ObjectId, StoreError>;
    fn contains(&self, id: &ObjectId) -> Result<bool, StoreError>;
    /// Enumerate all stored ids (fsck / GC mark phase). Not for hot paths.
    fn list(&self) -> Result<Vec<ObjectId>, StoreError>;

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
/// Writes go to a temp file, are fsynced, then renamed: readers never see partial objects.
/// Concurrent writers of the same id are safe (identical content; rename is atomic).
pub struct LocalStore {
    dir: PathBuf,
}

impl LocalStore {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let dir = dir.into();
        io(&dir, fs::create_dir_all(dir.join("tmp")))?;
        Ok(Self { dir })
    }
    fn path_of(&self, id: &ObjectId) -> PathBuf {
        let h = id.to_hex();
        self.dir.join(&h[..2]).join(&h[2..])
    }
}

impl ObjectStore for LocalStore {
    fn get(&self, id: &ObjectId) -> Result<Object, StoreError> {
        let p = self.path_of(id);
        match fs::read(&p) {
            Ok(b) => decode_verified(id, &b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StoreError::NotFound(*id)),
            Err(source) => Err(StoreError::Io { path: p, source }),
        }
    }

    fn put(&self, obj: &Object) -> Result<ObjectId, StoreError> {
        let enc = obj.encode();
        let id = vcs_objects::id_of_encoded(&enc);
        let dest = self.path_of(&id);
        if dest.exists() {
            return Ok(id);
        }
        let parent = dest.parent().expect("object path has parent");
        io(parent, fs::create_dir_all(parent))?;
        let tmp = self.dir.join("tmp").join(format!("{}.{}.tmp", id.short(), std::process::id()));
        {
            let mut f = io(&tmp, fs::File::create(&tmp))?;
            io(&tmp, f.write_all(&enc))?;
            io(&tmp, f.sync_all())?;
        }
        io(&dest, fs::rename(&tmp, &dest))?;
        Ok(id)
    }

    fn contains(&self, id: &ObjectId) -> Result<bool, StoreError> {
        Ok(self.path_of(id).exists())
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
        let p = s.path_of(&id);
        let mut b = fs::read(&p).unwrap();
        *b.last_mut().unwrap() ^= 1;
        fs::write(&p, b).unwrap();
        assert!(matches!(s.get(&id), Err(StoreError::Corrupt { .. })));
    }

    #[test]
    fn local_survives_reopen_and_leftover_tmp() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("o");
        let id = {
            let s = LocalStore::open(&dir).unwrap();
            // simulate crash mid-write: stray tmp file must not be visible as an object
            fs::write(dir.join("tmp").join("junk.tmp"), b"partial").unwrap();
            s.put_blob(b"x").unwrap()
        };
        let s = LocalStore::open(&dir).unwrap();
        assert_eq!(s.get_blob(&id).unwrap(), b"x");
        assert_eq!(s.list().unwrap(), vec![id]);
    }
}
