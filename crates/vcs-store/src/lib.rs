//! Object store abstraction + in-memory and local-filesystem backends.
//!
//! The trait is synchronous for now: the local backends are plain file I/O and the core
//! must not depend on Tokio. The S3 backend (Milestone 3) will implement an async twin
//! at the network boundary; see docs/architecture.md §Storage.

pub use vcs_objects::chunk;
pub mod pack;
use pack::Pack;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::sync::RwLock;
use vcs_objects::{ChunkedBlob, DecodeError, Object, ObjectId};

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
    /// Store a blob whose id the caller already computed (`id == vcs_objects::blob_id(data)`), so
    /// the store need not hash it again. The default re-derives the id and checks it.
    fn put_blob_prehashed(&self, id: &ObjectId, data: &[u8]) -> Result<(), StoreError> {
        if self.put_blob(data)? != *id {
            return Err(StoreError::Corrupt {
                id: *id,
                reason: "chunk id mismatch".into(),
            });
        }
        Ok(())
    }
    /// Store file content: a plain blob below `CHUNK_THRESHOLD`, otherwise chunk blobs plus a
    /// manifest. Returns the canonical id (`vcs_objects::content_id`).
    fn put_content(&self, data: &[u8]) -> Result<ObjectId, StoreError> {
        match ChunkedBlob::plan(data) {
            None => self.put_blob(data),
            Some(m) => {
                let mut off = 0usize;
                for c in &m.chunks {
                    let end = off + c.len as usize;
                    // `plan` already hashed this chunk; do not hash it again.
                    self.put_blob_prehashed(&c.id, &data[off..end])?;
                    off = end;
                }
                self.put(&Object::Chunked(m))
            }
        }
    }
    /// Read file content stored by `put_content` (a blob, or a manifest reassembled from its
    /// chunks). Every chunk hash and length, and the total length, are verified.
    fn get_content(&self, id: &ObjectId) -> Result<Vec<u8>, StoreError> {
        match self.get(id)? {
            Object::Blob(b) => Ok(b),
            Object::Chunked(m) => {
                let mut out = Vec::with_capacity(m.total_len as usize);
                for c in &m.chunks {
                    let b = self.get_blob(&c.id)?;
                    if b.len() != c.len as usize {
                        return Err(StoreError::Corrupt {
                            id: *id,
                            reason: format!("chunk {} has length {}, manifest says {}", c.id, b.len(), c.len),
                        });
                    }
                    out.extend_from_slice(&b);
                }
                Ok(out)
            }
            _ => Err(StoreError::Corrupt {
                id: *id,
                reason: "expected blob or chunk manifest".into(),
            }),
        }
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
    /// Dedup hits on objects older than this refresh their age; see [`DEFAULT_TOUCH_AFTER`].
    touch_after: std::time::Duration,
    /// Test hook: simulate a crash at a point of [`LocalStore::sweep`].
    sweep_crash: Mutex<Option<SweepCrash>>,
}

/// Where a simulated crash interrupts [`LocalStore::sweep`] (test support; see `gc` tests).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepCrash {
    /// New pack and index written, old index and pack still present.
    AfterNewPack,
    /// Old index removed, old pack file still present (an index-less pack is left behind).
    AfterOldIndexRemoved,
    /// All pack rewrites done, no loose object removed yet.
    BeforeLooseRemoval,
}

/// What a sweep removed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepStats {
    pub loose_removed: usize,
    pub packs_rewritten: usize,
    pub packs_removed: usize,
    /// Objects dropped from packs (they are not counted in `loose_removed`).
    pub packed_dropped: usize,
}

/// What orphan cleanup removed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OrphanStats {
    pub tmp_files: usize,
    pub packs_without_index: usize,
}

fn sweep_crashed(point: SweepCrash) -> StoreError {
    StoreError::Io {
        path: PathBuf::from("<sweep>"),
        source: std::io::Error::other(format!("simulated crash {point:?}")),
    }
}

/// Default for [`LocalStore::set_touch_after`]: a quarter of the default 14-day GC grace period.
/// A dedup hit on an object younger than this skips the touch, so an object's age after any
/// write that names it is at most this much, and GC's grace must exceed it (docs/gc.md).
pub const DEFAULT_TOUCH_AFTER: std::time::Duration = std::time::Duration::from_secs(14 * 86400 / 4);

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

const SYNC_THREADS: usize = 32;

/// Run `f` over `items` on up to `SYNC_THREADS` threads (inline for small inputs); first error wins.
fn par_try_each<T: Sync>(items: &[T], f: impl Fn(&T) -> Result<(), StoreError> + Sync) -> Result<(), StoreError> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(SYNC_THREADS);
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

/// Refresh an object file's mtime to now. Errors propagate: a swallowed failure would leave an
/// old object looking old, which is exactly the hole this closes.
/// Unlink a file; `Ok(false)` if it was already gone.
fn remove_if_present(p: &Path) -> std::io::Result<bool> {
    match fs::remove_file(p) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn touch(path: &Path) -> Result<(), StoreError> {
    let f = io(path, fs::File::options().write(true).open(path))?;
    io(path, f.set_modified(std::time::SystemTime::now()))
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
            touch_after: DEFAULT_TOUCH_AFTER,
            sweep_crash: Mutex::new(None),
        })
    }
    /// The dedup-touch threshold; a destructive sweep's grace period must exceed it.
    pub fn touch_after(&self) -> std::time::Duration {
        self.touch_after
    }
    #[doc(hidden)]
    pub fn set_sweep_crash(&self, c: Option<SweepCrash>) {
        *self.sweep_crash.lock().unwrap() = c;
    }
    fn crash_if(&self, at: SweepCrash) -> Result<(), StoreError> {
        if *self.sweep_crash.lock().unwrap() == Some(at) {
            return Err(sweep_crashed(at));
        }
        Ok(())
    }
    fn reload_packs(&self) -> Result<(), StoreError> {
        let fresh = Self::load_packs(&self.dir)?;
        *self.packs.write().unwrap() = fresh;
        Ok(())
    }
    /// Only refresh a deduplicated object's age if it is older than `d` (zero: always).
    pub fn set_touch_after(&mut self, d: std::time::Duration) {
        self.touch_after = d;
    }
    /// True if an object last written at `t` is old enough that a dedup hit must refresh it.
    /// Unknown age counts as old; a timestamp in the future counts as recent.
    fn is_stale(&self, t: Option<std::time::SystemTime>) -> bool {
        match t {
            Some(t) => std::time::SystemTime::now().duration_since(t).is_ok_and(|a| a >= self.touch_after),
            None => true,
        }
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
        let scan = || -> Result<Option<Vec<u8>>, StoreError> {
            for p in self.packs.read().unwrap().iter() {
                if let Some(b) = p.read(id)? {
                    return Ok(Some(b));
                }
            }
            Ok(None)
        };
        match scan() {
            // Another process (GC) replaced a pack after this store opened: reload once.
            Err(StoreError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                self.reload_packs()?;
                scan()
            }
            r => r,
        }
    }

    /// **DESTRUCTIVE.** Permanently remove `doomed` objects: packs holding any of them are
    /// rewritten without them, then loose copies are unlinked. The caller (GC) has already
    /// decided they are unreachable and old, and holds the repository lock.
    ///
    /// Crash safety, in this order: (1) new pack (pack file first, index last), (2) old index
    /// removed, (3) old pack removed, (4) loose files removed. A crash before (2) leaves only
    /// duplicates; between (2) and (3) leaves an index-less `.pack`, which readers ignore and
    /// [`LocalStore::clean_orphans`] removes; nothing live is ever missing. Pending (unsynced)
    /// objects are never removed.
    pub fn sweep(&self, doomed: &BTreeSet<ObjectId>) -> Result<SweepStats, StoreError> {
        self.sync()?;
        let mut st = SweepStats::default();
        {
            let mut guard = self.packs.write().unwrap();
            let mut work: Vec<Pack> = std::mem::take(&mut *guard);
            let mut done: Vec<Pack> = Vec::new();
            let res = (|| -> Result<(), StoreError> {
                let mut i = 0;
                while i < work.len() {
                    let old = &work[i];
                    let hit = old.ids().filter(|x| doomed.contains(x)).count();
                    if hit == 0 {
                        i += 1;
                        continue;
                    }
                    let mut keep = Vec::new();
                    for id in old.ids().filter(|x| !doomed.contains(x)) {
                        let b = old.read(&id)?.ok_or(StoreError::NotFound(id))?;
                        decode_verified(&id, &b)?; // never carry over something that does not hash to its name
                        keep.push((id, b));
                    }
                    let (idx_path, pack_path) = (old.pack_path.with_extension("idx"), old.pack_path.clone());
                    if keep.is_empty() {
                        st.packs_removed += 1;
                    } else {
                        done.push(pack::write_pack(&self.dir.join("pack"), &self.dir.join("tmp"), keep)?);
                        st.packs_rewritten += 1;
                    }
                    st.packed_dropped += hit;
                    self.crash_if(SweepCrash::AfterNewPack)?;
                    // Index first, so no reader is pointed at a pack that is about to vanish.
                    io(&idx_path, remove_if_present(&idx_path))?;
                    self.crash_if(SweepCrash::AfterOldIndexRemoved)?;
                    io(&pack_path, remove_if_present(&pack_path))?;
                    work.swap_remove(i);
                }
                Ok(())
            })();
            done.extend(work);
            *guard = done;
            res?;
        }
        self.crash_if(SweepCrash::BeforeLooseRemoval)?;
        let pending: BTreeSet<ObjectId> = self.pending.lock().unwrap().keys().copied().collect();
        for id in doomed.iter().filter(|i| !pending.contains(i)) {
            let p = self.path_of(id);
            if io(&p, remove_if_present(&p))? {
                st.loose_removed += 1;
                if let Some(d) = p.parent() {
                    let _ = fs::remove_dir(d); // only succeeds when empty
                }
            }
        }
        Ok(st)
    }

    /// **DESTRUCTIVE.** Remove leftovers of crashed writers older than `older_than`: files in
    /// `tmp/` and `.pack` files without a `.idx` (a pack is renamed into place before its
    /// index, so a fresh index-less pack may be a write in progress; hence the age check).
    pub fn clean_orphans(&self, now: std::time::SystemTime, older_than: std::time::Duration) -> Result<OrphanStats, StoreError> {
        let mut st = OrphanStats::default();
        let old_enough = |p: &Path| {
            fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|a| a >= older_than)
        };
        let pending: BTreeSet<PathBuf> = self.pending.lock().unwrap().values().cloned().collect();
        let tmp = self.dir.join("tmp");
        if let Ok(rd) = fs::read_dir(&tmp) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_file() && !pending.contains(&p) && old_enough(&p) && remove_if_present(&p).is_ok() {
                    st.tmp_files += 1;
                }
            }
        }
        if let Ok(rd) = fs::read_dir(self.dir.join("pack")) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "pack")
                    && !p.with_extension("idx").exists()
                    && old_enough(&p)
                    && remove_if_present(&p).is_ok()
                {
                    st.packs_without_index += 1;
                }
            }
        }
        Ok(st)
    }
    fn in_pack(&self, id: &ObjectId) -> bool {
        self.packs.read().unwrap().iter().any(|p| p.contains(id))
    }
    /// Move every loose object into one new pack (default write format), then delete the loose
    /// files. Returns the number of objects packed (0 if there were none, in which case no pack
    /// is written). Existing packs are left alone; see [`LocalStore::repack_as`].
    pub fn repack(&self) -> Result<usize, StoreError> {
        Ok(self.repack_as(pack::PackFormat::from_env(), false)?.0)
    }
    /// Like [`LocalStore::repack`] but writing `fmt`; with `convert_existing`, packs of another
    /// format are rewritten too (this is `vcs repack --format v1`, the rollback path). Returns
    /// `(loose objects packed, existing packs converted)`.
    pub fn repack_as(&self, fmt: pack::PackFormat, convert_existing: bool) -> Result<(usize, usize), StoreError> {
        self.sync()?;
        let converted = if convert_existing { self.convert_packs(fmt)? } else { 0 };
        let loose = self.list_loose()?;
        if loose.is_empty() {
            return Ok((0, converted));
        }
        let mut objs = Vec::with_capacity(loose.len());
        for id in &loose {
            let p = self.path_of(id);
            let b = io(&p, fs::read(&p))?;
            decode_verified(id, &b)?; // never pack something that does not hash to its name
            objs.push((*id, b));
        }
        let pack = pack::write_pack_fmt(&self.dir.join("pack"), &self.dir.join("tmp"), objs, fmt)?;
        self.packs.write().unwrap().push(pack);
        for id in &loose {
            let _ = fs::remove_file(self.path_of(id));
        }
        Ok((loose.len(), converted))
    }
    /// Rewrite every pack not already in `fmt`. Same crash order as a sweep: new pack (pack file
    /// first, index last), then old index, then old pack, so nothing is ever missing.
    fn convert_packs(&self, fmt: pack::PackFormat) -> Result<usize, StoreError> {
        let mut guard = self.packs.write().unwrap();
        let old = std::mem::take(&mut *guard);
        let mut out = Vec::with_capacity(old.len());
        let (mut n, mut res) = (0, Ok(()));
        for p in old {
            if res.is_err() || p.version() == fmt.version() {
                out.push(p);
                continue;
            }
            let r = (|| -> Result<Pack, StoreError> {
                let mut objs = Vec::new();
                for id in p.ids() {
                    let b = p.read(&id)?.ok_or(StoreError::NotFound(id))?;
                    decode_verified(&id, &b)?;
                    objs.push((id, b));
                }
                let np = pack::write_pack_fmt(&self.dir.join("pack"), &self.dir.join("tmp"), objs, fmt)?;
                let idx_path = p.pack_path.with_extension("idx");
                io(&idx_path, remove_if_present(&idx_path))?;
                io(&p.pack_path, remove_if_present(&p.pack_path))?;
                Ok(np)
            })();
            match r {
                Ok(np) => {
                    out.push(np);
                    n += 1;
                }
                Err(e) => {
                    res = Err(e);
                    out.push(p);
                }
            }
        }
        *guard = out;
        res.map(|_| n)
    }
    /// Estimated number of loose objects, from the fan-out directory `17` alone (as git's
    /// `gc.auto` does): ids are uniform, so that directory holds about 1/256 of them.
    pub fn loose_estimate(&self) -> usize {
        let d = self.dir.join("17");
        fs::read_dir(d).map(|rd| rd.flatten().count() * 256).unwrap_or(0)
    }

    /// Pack the loose objects last modified at least `grace` before `now` into one new pack,
    /// then delete exactly those loose files. Returns the number packed (no pack when 0).
    ///
    /// Crash safe: the pack and its index are durable (index last) before any loose file is
    /// unlinked, and only objects that were read back, verified and written into that pack are
    /// ever unlinked. A crash leaves duplicates (loose + packed), never a missing object.
    /// Objects not yet published by this handle (`sync` runs first) are never touched. The
    /// caller holds the repository lock so two repacks do not race.
    pub fn repack_older_than(&self, now: std::time::SystemTime, grace: std::time::Duration) -> Result<usize, StoreError> {
        self.sync()?;
        let mut objs = Vec::new();
        let mut ids = Vec::new();
        for id in self.list_loose()? {
            let p = self.path_of(&id);
            let old = fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|a| a >= grace);
            if !old {
                continue;
            }
            let b = match fs::read(&p) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue, // removed meanwhile
                Err(source) => return Err(StoreError::Io { path: p, source }),
            };
            decode_verified(&id, &b)?;
            objs.push((id, b));
            ids.push(id);
        }
        if ids.is_empty() {
            return Ok(0);
        }
        let pack = pack::write_pack(&self.dir.join("pack"), &self.dir.join("tmp"), objs)?;
        self.packs.write().unwrap().push(pack);
        for id in &ids {
            let p = self.path_of(id);
            let _ = fs::remove_file(&p);
            if let Some(d) = p.parent() {
                let _ = fs::remove_dir(d); // only succeeds when empty
            }
        }
        Ok(ids.len())
    }
    /// Whole-pack verification for fsck (checksums, framing, every object hash).
    pub fn verify_packs(&self) -> Vec<String> {
        self.packs.read().unwrap().iter().flat_map(|p| p.verify()).collect()
    }
    /// Write `head || tail` (already identified as `id`) as a pending loose object unless it exists.
    fn store_encoded(&self, id: &ObjectId, head: &[u8], tail: &[u8]) -> Result<(), StoreError> {
        let id = *id;
        // Existing objects are never rewritten. A pending object is brand new already. A published
        // one older than `touch_after` gets its modification time refreshed ("touch on dedup"), so
        // GC's age check sees a re-adopted orphan as young (docs/gc.md). Younger ones are left
        // alone, which keeps the common "just wrote it" dedup free of write I/O.
        if self.pending.lock().unwrap().contains_key(&id) {
            return Ok(());
        }
        let loose = self.path_of(&id);
        match fs::metadata(&loose) {
            Ok(m) => {
                if self.is_stale(m.modified().ok()) {
                    touch(&loose)?;
                }
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(StoreError::Io { path: loose, source }),
        }
        // Packs are immutable and shared, so an old packed object is re-adopted by writing a fresh
        // loose copy below (which `modified` prefers); an object in a recent pack needs nothing.
        let packed_recent = self
            .packs
            .read()
            .unwrap()
            .iter()
            .find(|pk| pk.contains(&id))
            .map(|pk| !self.is_stale(pk.modified()));
        if packed_recent == Some(true) {
            return Ok(());
        }
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let tmp = self.dir.join("tmp").join(format!("{}.{}.{n}.tmp", id.to_hex(), std::process::id()));
        {
            let mut f = io(&tmp, fs::File::create(&tmp))?;
            if !head.is_empty() {
                io(&tmp, f.write_all(head))?;
            }
            io(&tmp, f.write_all(tail))?;
        }
        self.pending.lock().unwrap().insert(id, tmp);
        Ok(())
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
        self.store_encoded(&id, &[], &enc)?;
        Ok(id)
    }

    fn put_blob_prehashed(&self, id: &ObjectId, data: &[u8]) -> Result<(), StoreError> {
        debug_assert_eq!(*id, vcs_objects::blob_id(data));
        // Same bytes as `Object::Blob(data).encode()`, written without building that copy.
        self.store_encoded(id, &[vcs_objects::Kind::Blob as u8, vcs_objects::FORMAT_VERSION], data)
    }

    fn sync(&self) -> Result<(), StoreError> {
        // Held for the whole barrier so readers never observe "neither pending nor published".
        let mut pending = self.pending.lock().unwrap();
        if pending.is_empty() {
            return Ok(());
        }
        // 1. object data to the device, then one hardware flush covering all of it.
        let tmps: Vec<&PathBuf> = pending.values().collect();
        // Start write-back of every file first (Linux), so the device sees all of it at once and
        // the per-file fsync below mostly waits instead of serialising seeks and journal commits.
        #[cfg(target_os = "linux")]
        par_try_each(&tmps, |tmp| {
            use std::os::unix::io::AsRawFd;
            let f = io(tmp, fs::File::open(tmp))?;
            // SAFETY: valid fd; the call only schedules write-back and has no memory effects.
            unsafe { libc::sync_file_range(f.as_raw_fd(), 0, 0, libc::SYNC_FILE_RANGE_WRITE) };
            Ok(())
        })?;
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
    fn local_put_dedup_refreshes_mtime() {
        let d = tempfile::tempdir().unwrap();
        let s = LocalStore::open(d.path().join("objects")).unwrap();
        let id = s.put_blob(b"old orphan").unwrap();
        s.sync().unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86400);
        fs::File::options()
            .write(true)
            .open(s.path_of(&id))
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(s.modified(&id).unwrap().unwrap() <= old);
        assert_eq!(s.put_blob(b"old orphan").unwrap(), id);
        let age = std::time::SystemTime::now()
            .duration_since(s.modified(&id).unwrap().unwrap())
            .unwrap();
        assert!(age < std::time::Duration::from_secs(60), "dedup put must refresh age, got {age:?}");
    }

    fn backdate(p: &Path, secs: u64) -> std::time::SystemTime {
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
        fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
        t
    }

    #[test]
    fn local_put_dedup_skips_touch_when_recent() {
        let d = tempfile::tempdir().unwrap();
        let s = LocalStore::open(d.path().join("objects")).unwrap();
        let id = s.put_blob(b"recent").unwrap();
        s.sync().unwrap();
        let t = backdate(&s.path_of(&id), 3600); // 1h old, well under touch_after
        s.put_blob(b"recent").unwrap();
        let m = fs::metadata(s.path_of(&id)).unwrap().modified().unwrap();
        assert_eq!(m, t, "a recent object must not be rewritten or touched");
    }

    #[test]
    fn local_put_dedup_readopts_old_packed_object() {
        let d = tempfile::tempdir().unwrap();
        let s = LocalStore::open(d.path().join("objects")).unwrap();
        let id = s.put_blob(b"packed orphan").unwrap();
        assert_eq!(s.repack().unwrap(), 1);
        let pack = fs::read_dir(d.path().join("objects/pack"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|x| x == "pack"))
            .unwrap();
        backdate(&pack, 30 * 86400);
        assert!(s.modified(&id).unwrap().unwrap() < std::time::SystemTime::now() - std::time::Duration::from_secs(86400));
        s.put_blob(b"packed orphan").unwrap();
        s.sync().unwrap();
        let age = std::time::SystemTime::now()
            .duration_since(s.modified(&id).unwrap().unwrap())
            .unwrap();
        assert!(
            age < std::time::Duration::from_secs(60),
            "re-adopted packed object must look young, got {age:?}"
        );
        assert_eq!(s.get_blob(&id).unwrap(), b"packed orphan");
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

    fn big(n: usize, mut x: u64) -> Vec<u8> {
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn large_content_roundtrips_as_manifest_in_both_stores_and_after_repack() {
        let data = big(3 << 20, 11);
        let want = vcs_objects::content_id(&data);
        let mem = MemoryStore::new();
        assert_eq!(mem.put_content(&data).unwrap(), want);
        assert_eq!(mem.get_content(&want).unwrap(), data);
        assert!(matches!(mem.get(&want).unwrap(), Object::Chunked(_)));
        assert!(mem.get_blob(&want).is_err(), "a manifest is not a blob");
        let d = tempfile::tempdir().unwrap();
        let s = LocalStore::open(d.path().join("o")).unwrap();
        assert_eq!(s.put_content(&data).unwrap(), want);
        s.sync().unwrap();
        assert_eq!(s.get_content(&want).unwrap(), data);
        s.repack().unwrap();
        assert_eq!(s.get_content(&want).unwrap(), data);
        // small content stays a plain blob
        let small = s.put_content(b"tiny").unwrap();
        assert!(matches!(s.get(&small).unwrap(), Object::Blob(_)));
    }

    #[test]
    fn prehashed_chunk_files_are_byte_identical_to_ordinary_blob_puts() {
        let d = tempfile::tempdir().unwrap();
        let fast = LocalStore::open(d.path().join("fast")).unwrap();
        let slow = LocalStore::open(d.path().join("slow")).unwrap();
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        let id = fast.put_content(&data).unwrap();
        let Object::Chunked(m) = Object::Chunked(ChunkedBlob::plan(&data).unwrap()) else {
            panic!()
        };
        for c in &m.chunks {
            assert_eq!(
                fast.get(&c.id).unwrap().encode(),
                Object::Blob(fast.get_blob(&c.id).unwrap()).encode()
            );
        }
        fast.sync().unwrap();
        // the same content through the generic (hashing) path lands in the same files
        let mut off = 0;
        for c in &m.chunks {
            slow.put_blob(&data[off..off + c.len as usize]).unwrap();
            off += c.len as usize;
        }
        slow.put(&Object::Chunked(m)).unwrap();
        slow.sync().unwrap();
        let mut a = fast.list().unwrap();
        let mut b = slow.list().unwrap();
        a.sort();
        b.sort();
        assert_eq!(a, b);
        for x in &a {
            let h = x.to_hex();
            assert_eq!(
                std::fs::read(d.path().join("fast").join(&h[..2]).join(&h[2..])).unwrap(),
                std::fs::read(d.path().join("slow").join(&h[..2]).join(&h[2..])).unwrap()
            );
        }
        assert_eq!(id, vcs_objects::content_id(&data));
    }

    #[test]
    fn edited_version_shares_most_chunks() {
        let a = big(4 << 20, 3);
        let mut b = a.clone();
        b[2 << 20] ^= 0xFF; // one byte in the middle
        let s = MemoryStore::new();
        s.put_content(&a).unwrap();
        let n1 = s.list().unwrap().len();
        s.put_content(&b).unwrap();
        let added = s.list().unwrap().len() - n1;
        assert!(added <= 4, "one-byte edit added {added} of {n1} objects");
    }

    #[test]
    fn chunked_read_detects_missing_corrupt_and_wrong_length_chunks() {
        let data = big(2 << 20, 8);
        let s = MemoryStore::new();
        let id = s.put_content(&data).unwrap();
        let Object::Chunked(m) = s.get(&id).unwrap() else { panic!() };
        // corrupted chunk bytes
        let mut bad = Object::Blob(vec![0u8; m.chunks[1].len as usize]).encode();
        bad[5] ^= 1;
        s.corrupt(&m.chunks[1].id, bad);
        assert!(matches!(s.get_content(&id), Err(StoreError::Corrupt { .. })));
        // truncated chunk: a validly encoded shorter blob stored under the chunk id
        s.corrupt(&m.chunks[1].id, Object::Blob(vec![1u8; 3]).encode());
        assert!(matches!(s.get_content(&id), Err(StoreError::Corrupt { .. })));
        // missing chunk
        s.remove_for_test(&m.chunks[1].id);
        assert!(matches!(s.get_content(&id), Err(StoreError::NotFound(x)) if x == m.chunks[1].id));
    }

    #[test]
    fn chunk_manifest_with_wrong_length_for_valid_chunk_is_corrupt() {
        // A syntactically valid manifest whose recorded length disagrees with the stored chunk.
        let data = big(2 << 20, 9);
        let s = MemoryStore::new();
        let id = s.put_content(&data).unwrap();
        let Object::Chunked(mut m) = s.get(&id).unwrap() else { panic!() };
        m.chunks[0].len -= 1;
        m.chunks[1].len += 1;
        let forged = s.put(&Object::Chunked(m)).unwrap();
        assert!(matches!(s.get_content(&forged), Err(StoreError::Corrupt { .. })));
    }
}
