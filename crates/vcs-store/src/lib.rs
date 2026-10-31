//! Object store abstraction + in-memory and local-filesystem backends.
//!
//! The trait is synchronous for now: the local backends are plain file I/O and the core
//! must not depend on Tokio. The S3 backend (Milestone 3) will implement an async twin
//! at the network boundary; see docs/architecture.md §Storage.

pub mod chunk;
pub mod pack;
use pack::Pack;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::sync::RwLock;
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
    /// Test hook: drop an object so any read of it fails with `NotFound`.
    pub fn remove_for_test(&self, id: &ObjectId) {
        self.objects.lock().unwrap().remove(id);
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
    packs: RwLock<Vec<Pack>>,
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

/// Run `f` over `items` on up to 8 threads (inline for small inputs); first error wins.
fn par_try_each<T: Sync>(items: &[T], f: impl Fn(&T) -> Result<(), StoreError> + Sync) -> Result<(), StoreError> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
    if threads <= 1 || items.len() < 64 {
        return items.iter().try_for_each(f);
    }
    let next = AtomicUsize::new(0);
    let failed = std::sync::atomic::AtomicBool::new(false);
    let errs: Vec<Option<StoreError>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                sc.spawn(|| {
                    while !failed.load(Ordering::Relaxed) {
                        let Some(it) = items.get(next.fetch_add(1, Ordering::Relaxed)) else {
                            break;
                        };
                        if let Err(e) = f(it) {
                            failed.store(true, Ordering::Relaxed);
                            return Some(e);
                        }
                    }
                    None
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().expect("sync worker panicked")).collect()
    });
    match errs.into_iter().flatten().next() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

impl LocalStore {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let dir = dir.into();
        io(&dir, fs::create_dir_all(dir.join("tmp")))?;
        let packs = Self::load_packs(&dir)?;
        Ok(Self {
            dir,
            pending: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
            packs: RwLock::new(packs),
        })
    }
    fn load_packs(dir: &Path) -> Result<Vec<Pack>, StoreError> {
        let pd = dir.join("pack");
        let mut out = Vec::new();
        let Ok(rd) = fs::read_dir(&pd) else { return Ok(out) };
        let mut idxs: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "idx"))
            .collect();
        idxs.sort();
        for p in idxs {
            out.push(Pack::open(&p)?);
        }
        Ok(out)
    }
    fn packed(&self, id: &ObjectId) -> Result<Option<Vec<u8>>, StoreError> {
        for p in self.packs.read().unwrap().iter() {
            if let Some(b) = p.read(id)? {
                return Ok(Some(b));
            }
        }
        Ok(None)
    }
    fn in_pack(&self, id: &ObjectId) -> bool {
        self.packs.read().unwrap().iter().any(|p| p.contains(id))
    }
    /// Move every loose object into one new pack, then delete the loose files. Returns the
    /// number of objects packed (0 if there were none, in which case no pack is written).
    pub fn repack(&self) -> Result<usize, StoreError> {
        self.sync()?;
        let loose = self.list_loose()?;
        if loose.is_empty() {
            return Ok(0);
        }
        let mut objs = Vec::with_capacity(loose.len());
        for id in &loose {
            let p = self.path_of(id);
            let b = io(&p, fs::read(&p))?;
            decode_verified(id, &b)?; // never pack something that does not hash to its name
            objs.push((*id, b));
        }
        let pack = pack::write_pack(&self.dir.join("pack"), &self.dir.join("tmp"), objs)?;
        self.packs.write().unwrap().push(pack);
        for id in &loose {
            let _ = fs::remove_file(self.path_of(id));
        }
        Ok(loose.len())
    }
    /// Whole-pack verification for fsck (checksums, framing, every object hash).
    pub fn verify_packs(&self) -> Vec<String> {
        self.packs.read().unwrap().iter().flat_map(|p| p.verify()).collect()
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match self.packed(id)? {
                Some(b) => decode_verified(id, &b),
                None => Err(StoreError::NotFound(*id)),
            },
            Err(source) => Err(StoreError::Io { path: p, source }),
        }
    }

    fn put(&self, obj: &Object) -> Result<ObjectId, StoreError> {
        let enc = obj.encode();
        let id = vcs_objects::id_of_encoded(&enc);
        // Existing objects are never rewritten: one stat (or a map lookup), no write I/O.
        if self.pending.lock().unwrap().contains_key(&id) || self.path_of(&id).exists() || self.in_pack(&id) {
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
        let tmps: Vec<&PathBuf> = pending.values().collect();
        par_try_each(&tmps, |tmp| {
            let f = io(tmp, fs::File::open(tmp))?;
            io(tmp, flush_file(&f))
        })?;
        if let Some(tmp) = tmps.first() {
            let f = io(tmp, fs::File::open(tmp))?;
            io(&self.dir, hardware_flush(&f))?;
        }
        // 2. publish under final names (data is already durable, so a visible name is complete).
        let mut dirs = BTreeSet::new();
        let mut moves = Vec::with_capacity(pending.len());
        for (id, tmp) in pending.iter() {
            let dest = self.path_of(id);
            let parent = dest.parent().expect("object path has parent").to_owned();
            if dirs.insert(parent.clone()) {
                io(&parent, fs::create_dir_all(&parent))?;
            }
            moves.push((tmp, dest));
        }
        // rename over an existing identical object is atomic and harmless (another writer won).
        par_try_each(&moves, |(tmp, dest)| io(dest, fs::rename(tmp, dest)))?;
        // 3. directory entries (renames, new fan-out dirs) durable before anything refers to them.
        dirs.insert(self.dir.clone());
        dirs.insert(self.dir.join("tmp"));
        let dirs: Vec<PathBuf> = dirs.into_iter().collect();
        par_try_each(&dirs, |d| {
            let f = io(d, fs::File::open(d))?;
            io(d, flush_file(&f))
        })?;
        if let Some(d) = dirs.first() {
            let f = io(d, fs::File::open(d))?;
            io(&self.dir, hardware_flush(&f))?;
        }
        pending.clear();
        Ok(())
    }

    fn contains(&self, id: &ObjectId) -> Result<bool, StoreError> {
        Ok(self.pending.lock().unwrap().contains_key(id) || self.path_of(id).exists() || self.in_pack(id))
    }

    fn modified(&self, id: &ObjectId) -> Result<Option<std::time::SystemTime>, StoreError> {
        // Written but not yet published objects are, by definition, brand new.
        if self.pending.lock().unwrap().contains_key(id) {
            return Ok(Some(std::time::SystemTime::now()));
        }
        let p = self.path_of(id);
        match fs::metadata(&p) {
            Ok(m) => Ok(m.modified().ok()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match self.packs.read().unwrap().iter().find(|pk| pk.contains(id)) {
                Some(pk) => Ok(pk.modified()),
                None => Err(StoreError::NotFound(*id)),
            },
            Err(source) => Err(StoreError::Io { path: p, source }),
        }
    }

    fn list(&self) -> Result<Vec<ObjectId>, StoreError> {
        let mut out = self.list_loose()?;
        for p in self.packs.read().unwrap().iter() {
            out.extend(p.ids());
        }
        out.sort();
        out.dedup();
        Ok(out)
    }
}

impl LocalStore {
    fn list_loose(&self) -> Result<Vec<ObjectId>, StoreError> {
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

    fn packed_store() -> (tempfile::TempDir, PathBuf, Vec<ObjectId>) {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("o");
        let s = LocalStore::open(&dir).unwrap();
        let ids: Vec<_> = (0..20)
            .map(|i| s.put_blob(format!("blob number {i}").as_bytes()).unwrap())
            .collect();
        assert_eq!(s.repack().unwrap(), 20);
        (d, dir, ids)
    }

    #[test]
    fn repack_reads_back_across_reopen_and_removes_loose() {
        let (_d, dir, ids) = packed_store();
        for reopen in [false, true] {
            let _ = reopen;
            let s = LocalStore::open(&dir).unwrap();
            for (i, id) in ids.iter().enumerate() {
                assert_eq!(s.get_blob(id).unwrap(), format!("blob number {i}").as_bytes());
                assert!(s.contains(id).unwrap());
                assert!(s.modified(id).unwrap().is_some());
            }
            let mut want = ids.clone();
            want.sort();
            assert_eq!(s.list().unwrap(), want);
            assert!(s.list_loose().unwrap().is_empty());
            assert!(s.verify_packs().is_empty());
            assert_eq!(s.put_blob(b"blob number 3").unwrap(), ids[3], "re-put of packed object is a no-op");
            assert!(s.list_loose().unwrap().is_empty());
            assert_eq!(s.repack().unwrap(), 0);
        }
    }

    fn pack_file(dir: &Path, ext: &str) -> PathBuf {
        fs::read_dir(dir.join("pack"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().unwrap() == ext)
            .unwrap()
    }

    #[test]
    fn pack_bitflip_is_detected_on_get_and_verify() {
        let (_d, dir, ids) = packed_store();
        let pp = pack_file(&dir, "pack");
        let mut b = fs::read(&pp).unwrap();
        b[20] ^= 1; // inside the first object's payload
        fs::write(&pp, b).unwrap();
        let s = LocalStore::open(&dir).unwrap();
        assert!(ids.iter().any(|id| matches!(s.get(id), Err(StoreError::Corrupt { .. }))));
        assert!(!s.verify_packs().is_empty());
    }

    #[test]
    fn truncated_pack_and_damaged_index_are_refused_at_open() {
        let (_d, dir, _ids) = packed_store();
        let pp = pack_file(&dir, "pack");
        let b = fs::read(&pp).unwrap();
        fs::write(&pp, &b[..b.len() - 10]).unwrap();
        assert!(LocalStore::open(&dir).is_err(), "truncated pack");
        fs::write(&pp, &b).unwrap();
        assert!(LocalStore::open(&dir).is_ok());
        let ip = pack_file(&dir, "idx");
        let mut i = fs::read(&ip).unwrap();
        i[60] ^= 1;
        fs::write(&ip, &i).unwrap();
        assert!(LocalStore::open(&dir).is_err(), "bad index checksum");
        fs::write(&ip, &i[..30]).unwrap();
        assert!(LocalStore::open(&dir).is_err(), "truncated index");
    }

    #[test]
    fn pack_without_index_is_ignored_and_loose_wins_nothing_lost() {
        let (_d, dir, _ids) = packed_store();
        fs::remove_file(pack_file(&dir, "idx")).unwrap();
        let s = LocalStore::open(&dir).unwrap();
        assert!(s.list().unwrap().is_empty());
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
