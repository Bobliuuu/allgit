//! Backup, restore and disaster-recovery tooling (issue #570). Design: docs/backup-restore.md.
//!
//! Three pieces, all built on the same key space the publisher and the sweeper use:
//!
//! * [`verify_remote`]: load the manifest, walk the closure of every branch head, snapshot, tag, shelf
//!   base, pending shard record and retained transaction, and hash every object on the way. Reports
//!   missing and corrupt objects instead of stopping at the first. `deep` also reads every pack end to
//!   end (length, trailer, every entry against its id) and every loose object, reachable or not.
//! * [`Hold`] and the **GC safe point**. A backup takes a lease under `gc/hold/` before it reads the
//!   manifest; `remote_gc` and `compact` refuse to run (after taking `gc/lock`) while an unexpired hold
//!   exists, and the backup refuses to start while an unexpired `gc/lock` exists. Each side writes first
//!   and reads second, so at least one of them sees the other. A killed backup leaves its hold behind,
//!   which expires on the backend clock (default 1 h); it is renewed while the backup lives. Every
//!   destructive sweep also records `gc/safe-point` (backend time, units deleted): a manifest saved
//!   before that time may name objects that sweep removed, which `verify --manifest` then reports exactly.
//! * [`backup_copy`]: copy a remote (or a backup) into another backend. Content-addressed keys are
//!   copied with create-only puts (so a re-run only moves what is new and a kill at any point loses
//!   nothing), packs before their indexes, shelf metadata last, and the manifest **last of all**, only
//!   after the closure of exactly that manifest was verified in the destination. A destination without a
//!   manifest is therefore "no backup yet", never a half-written one, and a destination with a manifest
//!   is complete. A sharded source is flattened to one ordinary manifest (a linearizable snapshot).

use crate::backend::{Backend, BackendError};
use crate::gc::{lock_expiry, GcError, View, LOCK_KEY};
use crate::manifest::{Manifest, KEY as MANIFEST_KEY};
use crate::protocol::{deps, load_manifest, tx_key, PublishError};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use vcs_objects::{id_of_encoded, Object, ObjectId};

pub const HOLD_PREFIX: &str = "gc/hold/";
pub const SAFE_POINT_KEY: &str = "gc/safe-point";
/// Default lifetime of a backup hold; renewed while the backup runs.
pub const DEFAULT_HOLD_MS: u64 = 60 * 60 * 1000;
/// A hold that claims to outlive this (planted or corrupt) is clamped, so it cannot wedge gc forever.
const MAX_HOLD_MS: u64 = 24 * 60 * 60 * 1000;
/// Key spaces a backup copies. Everything else (`gc/`, `tomb/`, `shard/`, `decision/`, `manifest`) is
/// coordination state of the live store, not data.
const COPY_PREFIXES: [&str; 4] = ["objects/", "packs/", "tx/", "shelves/"];
/// Problems listed in full in a report; the counters keep counting past it.
const LIST_CAP: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum DrError {
    #[error("gc is running (gc/lock is held until {0}, backend clock); retry after it finishes or the lease expires")]
    GcRunning(u64),
    #[error("lost the backup hold (it expired or was removed); the backup stopped before writing a manifest, re-run it")]
    HoldLost,
    #[error("destination is not empty: {0}; restore needs a fresh store (use `backup` to update an existing backup)")]
    NotEmpty(String),
    #[error("the source has no manifest, nothing to copy")]
    NoManifest,
    #[error("the destination manifest changed while copying (another writer?); re-run")]
    DestChanged,
    #[error("verification failed, the manifest was NOT written: {0}")]
    Verify(String),
    #[error("remote is damaged: {0}")]
    Damaged(String),
    #[error(transparent)]
    Backend(#[from] BackendError),
}

impl From<GcError> for DrError {
    fn from(e: GcError) -> Self {
        match e {
            GcError::Backend(b) => DrError::Backend(b),
            other => DrError::Damaged(other.to_string()),
        }
    }
}
impl From<PublishError> for DrError {
    fn from(e: PublishError) -> Self {
        match e {
            PublishError::Backend(b) => DrError::Backend(b),
            other => DrError::Damaged(other.to_string()),
        }
    }
}

// ---------------------------------------------------------------- safe point and hold

/// What the last destructive sweep removed and when (backend clock). Manifests or backups captured before
/// `at_ms` without a hold may reference objects that sweep deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafePoint {
    pub at_ms: u64,
    pub objects_deleted: u64,
    pub packs_deleted: u64,
}

impl SafePoint {
    fn encode(&self) -> Vec<u8> {
        format!(
            "safe-point v1\nat_ms={}\nobjects={}\npacks={}\n",
            self.at_ms, self.objects_deleted, self.packs_deleted
        )
        .into_bytes()
    }
    fn parse(data: &[u8]) -> Option<SafePoint> {
        let text = String::from_utf8_lossy(data);
        let mut lines = text.lines();
        if lines.next()? != "safe-point v1" {
            return None;
        }
        let mut f = |k: &str| -> Option<u64> { lines.next()?.strip_prefix(k)?.strip_prefix('=')?.parse().ok() };
        Some(SafePoint {
            at_ms: f("at_ms")?,
            objects_deleted: f("objects")?,
            packs_deleted: f("packs")?,
        })
    }
}

pub fn read_safe_point(b: &dyn Backend) -> Result<Option<SafePoint>, BackendError> {
    Ok(b.get(SAFE_POINT_KEY)?.and_then(|v| SafePoint::parse(&v.data)))
}

/// Called by the sweeper (under `gc/lock`) after a sweep that removed something.
pub(crate) fn record_safe_point(b: &dyn Backend, objects: u64, packs: u64) -> Result<(), BackendError> {
    let sp = SafePoint {
        at_ms: b.now_ms()?,
        objects_deleted: objects,
        packs_deleted: packs,
    };
    let data = sp.encode();
    for _ in 0..3 {
        match b.get(SAFE_POINT_KEY)? {
            None => {
                if b.put_if_absent(SAFE_POINT_KEY, &data)? {
                    return Ok(());
                }
            }
            Some(cur) => {
                if b.put_if_match(SAFE_POINT_KEY, &data, &cur.tag)?.is_some() {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

/// One live or expired hold found under `gc/hold/`.
#[derive(Debug, Clone)]
pub struct HoldInfo {
    pub key: String,
    pub owner: String,
    pub expires_ms: u64,
}

fn parse_hold(key: &str, data: &[u8], now: u64) -> HoldInfo {
    let text = String::from_utf8_lossy(data);
    let owner = text.rsplit_once(' ').map_or(text.as_ref(), |(o, _)| o).to_owned();
    HoldInfo {
        key: key.to_owned(),
        owner,
        expires_ms: lock_expiry(data).min(now.saturating_add(MAX_HOLD_MS)),
    }
}

/// Every hold under `gc/hold/` (expired ones included; compare `expires_ms` with `now_ms`).
pub fn list_holds(b: &dyn Backend, now: u64) -> Result<Vec<HoldInfo>, BackendError> {
    let mut out = Vec::new();
    for k in b.list(HOLD_PREFIX)? {
        if let Some(v) = b.get(&k)? {
            out.push(parse_hold(&k, &v.data, now));
        }
    }
    Ok(out)
}

/// Sweeper and compaction side: refuse while an unexpired hold exists; clear expired ones.
pub(crate) fn check_no_hold(b: &dyn Backend, now: u64) -> Result<(), GcError> {
    for h in list_holds(b, now)? {
        if h.expires_ms > now {
            return Err(GcError::BackupHold(format!(
                "{} until {} on the backend clock, in {} s",
                h.owner,
                h.expires_ms,
                (h.expires_ms - now) / 1000
            )));
        }
        let _ = b.delete(&h.key); // expired: its backup died
    }
    Ok(())
}

/// A lease that keeps `remote_gc` and `compact` off the remote. Released (deleted) on drop; a process
/// that is killed leaves it to expire.
pub struct Hold<'a> {
    b: &'a dyn Backend,
    key: String,
    owner: String,
    ttl_ms: u64,
    mine: Mutex<Vec<u8>>,
    last: Mutex<Instant>,
}

impl<'a> Hold<'a> {
    /// `Ok(None)` for a backend that cannot be collected at all (no clock, no delete): nothing to hold off.
    pub fn acquire(b: &'a dyn Backend, owner: &str, ttl_ms: u64) -> Result<Option<Hold<'a>>, DrError> {
        if !b.supports_gc() {
            return Ok(None);
        }
        static N: AtomicU64 = AtomicU64::new(0);
        let now = b.now_ms()?;
        let id = blake3::hash(format!("{owner}/{now}/{}/{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)).as_bytes()).to_hex();
        let key = format!("{HOLD_PREFIX}{}", &id[..24]);
        let mine = format!("{owner} {}", now.saturating_add(ttl_ms)).into_bytes();
        if !b.put_if_absent(&key, &mine)? {
            return Err(DrError::Damaged(format!("hold key {key} already exists")));
        }
        let hold = Hold {
            b,
            key,
            owner: owner.to_owned(),
            ttl_ms,
            mine: Mutex::new(mine),
            last: Mutex::new(Instant::now()),
        };
        // Write first (above), read second: if a sweeper took gc/lock before our hold was visible it may
        // already be deleting, so back off (dropping `hold` removes it).
        if let Some(l) = b.get(LOCK_KEY)? {
            let exp = lock_expiry(&l.data).min(now.saturating_add(10 * 60 * 1000));
            if exp > now {
                return Err(DrError::GcRunning(exp));
            }
        }
        Ok(Some(hold))
    }

    /// Extend the lease if a quarter of it has passed. Cheap to call often.
    pub fn renew(&self) -> Result<(), DrError> {
        {
            let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            if last.elapsed() < Duration::from_millis(self.ttl_ms / 4) {
                return Ok(());
            }
            *last = Instant::now();
        }
        let mut mine = self.mine.lock().unwrap_or_else(|e| e.into_inner());
        let Some(cur) = self.b.get(&self.key)? else {
            return Err(DrError::HoldLost);
        };
        if cur.data != *mine {
            return Err(DrError::HoldLost);
        }
        let now = self.b.now_ms()?;
        let next = format!("{} {}", self.owner, now.saturating_add(self.ttl_ms)).into_bytes();
        match self.b.put_if_match(&self.key, &next, &cur.tag)? {
            Some(_) => {
                *mine = next;
                Ok(())
            }
            None => Err(DrError::HoldLost),
        }
    }
}

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        let mine = self.mine.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(Some(v)) = self.b.get(&self.key) {
            if v.data == *mine {
                let _ = self.b.delete(&self.key);
            }
        }
    }
}

// ---------------------------------------------------------------- verify

#[derive(Debug, Clone, Default)]
pub struct VerifyOptions {
    /// Verify this manifest (for example one saved before a gc) instead of the remote's current one.
    pub manifest: Option<Manifest>,
    /// Also read every pack end to end and every loose object, reachable or not.
    pub deep: bool,
}

#[derive(Debug, Clone)]
pub struct Problem {
    pub id: Option<ObjectId>,
    /// What referenced it (a root such as `branch zone/main`, or a parent object id) or the key concerned.
    pub context: String,
    pub what: String,
}

#[derive(Debug, Clone, Default)]
pub struct VerifyReport {
    pub manifest_present: bool,
    pub generation: u64,
    pub branches: usize,
    pub snapshots: usize,
    pub tags: usize,
    pub roots: usize,
    /// Objects reached from the roots, read and hash-checked.
    pub objects_walked: u64,
    pub bytes_walked: u64,
    pub packs: usize,
    pub loose: usize,
    pub deep: bool,
    /// `--deep`: objects hash-checked in packs and loose files, reachable or not.
    pub deep_objects: u64,
    /// `.pack` files with no `.idx` (an interrupted publish; harmless, collected by gc once old).
    pub packs_without_index: usize,
    pub missing_total: usize,
    pub corrupt_total: usize,
    /// First [`LIST_CAP`] of each.
    pub missing: Vec<Problem>,
    pub corrupt: Vec<Problem>,
    pub safe_point: Option<SafePoint>,
    pub holds: Vec<HoldInfo>,
}

impl VerifyReport {
    pub fn is_ok(&self) -> bool {
        self.manifest_present && self.missing_total == 0 && self.corrupt_total == 0
    }
    fn add_missing(&mut self, p: Problem) {
        self.missing_total += 1;
        if self.missing.len() < LIST_CAP {
            self.missing.push(p);
        }
    }
    fn add_corrupt(&mut self, p: Problem) {
        self.corrupt_total += 1;
        if self.corrupt.len() < LIST_CAP {
            self.corrupt.push(p);
        }
    }
}

#[derive(Clone)]
enum Ref {
    Root(usize),
    Obj(ObjectId),
}

/// Walk and hash everything reachable from the manifest. Never stops at the first problem; a backend
/// error (as opposed to bad data) aborts with `Err`.
pub fn verify_remote(b: &dyn Backend, opts: &VerifyOptions) -> Result<VerifyReport, DrError> {
    let mut rep = VerifyReport {
        deep: opts.deep,
        ..Default::default()
    };
    let live = opts.manifest.is_none();
    let m = match &opts.manifest {
        Some(m) => Some(m.clone()),
        None => {
            let (m, tag) = load_manifest(b)?;
            tag.map(|_| m)
        }
    };
    let now = b.now_ms().unwrap_or_else(|_| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    });
    rep.safe_point = read_safe_point(b)?;
    rep.holds = list_holds(b, now)?;
    let Some(m) = m else { return Ok(rep) };
    rep.manifest_present = true;
    rep.generation = m.generation;
    rep.branches = m.branches.len();
    rep.snapshots = m.snapshots.len();
    rep.tags = m.tags.len();

    let mut roots: Vec<(ObjectId, String)> = Vec::new();
    for ((z, n), id) in &m.branches {
        roots.push((*id, format!("branch {z}/{n}")));
    }
    for (n, id) in &m.snapshots {
        roots.push((*id, format!("snapshot {n}")));
    }
    for ((z, n), id) in &m.tags {
        roots.push((*id, format!("tag {z}/{n}")));
    }
    for txid in &m.applied {
        if let Some(v) = b.get(&tx_key(txid))? {
            if let Ok(Object::Transaction(t)) = Object::decode(&v.data) {
                for u in &t.updates {
                    if let Some(o) = u.old {
                        roots.push((o, format!("retained transaction {}", txid.short())));
                    }
                    roots.push((u.new, format!("retained transaction {}", txid.short())));
                }
            }
        }
    }
    if live {
        let shelf = crate::shelf::roots(b, now).map_err(|e| DrError::Damaged(format!("shelves: {e}")))?;
        roots.extend(shelf.into_iter().map(|id| (id, "shelf base".to_owned())));
        let pend = crate::sharded::pending_roots(b).map_err(|e| DrError::Damaged(format!("sharded pending records: {e}")))?;
        roots.extend(pend.into_iter().map(|id| (id, "pending shard record".to_owned())));
    }
    rep.roots = roots.len();

    let view = View::load(b)?;
    rep.packs = view.packs.values().filter(|i| i.is_some()).count();
    rep.packs_without_index = view.packs.values().filter(|i| i.is_none()).count();
    rep.loose = view.loose.len();

    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut stack: Vec<(ObjectId, Ref)> = roots.iter().enumerate().map(|(i, (id, _))| (*id, Ref::Root(i))).collect();
    stack.reverse();
    let ctx = |f: &Ref| match f {
        Ref::Root(i) => roots[*i].1.clone(),
        Ref::Obj(p) => format!("referenced by {}", p.short()),
    };
    while let Some((id, from)) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        match view.read(&id) {
            Ok(Some(o)) => {
                rep.objects_walked += 1;
                stack.extend(deps(&o).into_iter().filter(|d| !seen.contains(d)).map(|d| (d, Ref::Obj(id))));
            }
            Ok(None) => rep.add_missing(Problem {
                id: Some(id),
                context: ctx(&from),
                what: "missing".into(),
            }),
            Err(GcError::Damaged(w)) => rep.add_corrupt(Problem {
                id: Some(id),
                context: ctx(&from),
                what: w,
            }),
            Err(e) => return Err(e.into()),
        }
    }
    rep.bytes_walked = 0; // filled in by deep only; the walk reads through the pack index and does not keep sizes

    if opts.deep {
        deep_scan(b, &view, &mut rep)?;
    }
    Ok(rep)
}

fn deep_scan(b: &dyn Backend, view: &View<'_>, rep: &mut VerifyReport) -> Result<(), DrError> {
    let stems: Vec<(&String, &vcs_store::pack::PackIndex)> = view.packs.iter().filter_map(|(s, i)| i.as_ref().map(|i| (s, i))).collect();
    let res = crate::par::map_bounded(
        &stems,
        crate::par::parallelism(),
        |(stem, idx)| -> Result<(u64, u64, Option<Problem>), DrError> {
            let info = crate::compact::PackInfo {
                stem: (*stem).clone(),
                len: idx.pack_len,
            };
            match crate::compact::read_pack(b, &info, idx) {
                Ok(objs) => Ok((objs.len() as u64, idx.pack_len, None)),
                Err(GcError::Damaged(w)) => Ok((
                    0,
                    0,
                    Some(Problem {
                        id: None,
                        context: format!("{stem}.pack"),
                        what: w,
                    }),
                )),
                Err(e) => Err(e.into()),
            }
        },
    )?;
    for (n, bytes, p) in res {
        rep.deep_objects += n;
        rep.bytes_walked += bytes;
        if let Some(p) = p {
            rep.add_corrupt(p);
        }
    }
    let loose: Vec<&ObjectId> = view.loose.iter().collect();
    let res = crate::par::map_bounded(
        &loose,
        crate::par::parallelism(),
        |id| -> Result<(u64, u64, Option<Problem>), DrError> {
            match b.get(&crate::protocol::object_key(id))? {
                None => Ok((0, 0, None)), // collected since the listing
                Some(v) if id_of_encoded(&v.data) == **id && Object::decode(&v.data).is_ok() => Ok((1, v.data.len() as u64, None)),
                Some(_) => Ok((
                    0,
                    0,
                    Some(Problem {
                        id: Some(**id),
                        context: crate::protocol::object_key(id),
                        what: "fails hash verification or does not decode".into(),
                    }),
                )),
            }
        },
    )?;
    for (n, bytes, p) in res {
        rep.deep_objects += n;
        rep.bytes_walked += bytes;
        if let Some(p) = p {
            rep.add_corrupt(p);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- copy (backup and restore)

#[derive(Debug, Clone)]
pub struct CopyOptions {
    /// Take a GC safe point on the source for the duration (needs a backend that can be collected; others have nothing to hold off).
    pub hold: bool,
    pub hold_ttl_ms: u64,
    pub owner: String,
    /// Read every pack of the destination end to end before the manifest is written (default).
    pub deep: bool,
    /// Restore: refuse a destination that already holds data. Backup: update an existing backup.
    pub require_empty_dest: bool,
}

impl Default for CopyOptions {
    fn default() -> Self {
        CopyOptions {
            hold: true,
            hold_ttl_ms: DEFAULT_HOLD_MS,
            owner: format!("vcs-backup pid {}", std::process::id()),
            deep: true,
            require_empty_dest: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CopyReport {
    pub copied: u64,
    pub already_there: u64,
    pub bytes_copied: u64,
    /// Keys that disappeared between listing and reading (a compaction on a source without hold support); the pass was repeated.
    pub vanished: u64,
    pub passes: u32,
    pub held: bool,
    pub branches: usize,
    pub verify: VerifyReport,
}

#[derive(Default)]
struct Counters {
    copied: AtomicU64,
    there: AtomicU64,
    bytes: AtomicU64,
    vanished: AtomicU64,
}

/// Rank of a key within a pass: data before the index that publishes it, shelf metadata last.
fn rank(key: &str) -> u8 {
    if key.ends_with(".idx") || key.ends_with("/idx") {
        1
    } else if key.ends_with("/meta") {
        2
    } else {
        0
    }
}

fn copy_key(src: &dyn Backend, dst: &dyn Backend, key: &str, c: &Counters) -> Result<(), DrError> {
    let Some(v) = src.get(key)? else {
        c.vanished.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    };
    if dst.put_if_absent(key, &v.data)? {
        c.copied.fetch_add(1, Ordering::Relaxed);
        c.bytes.fetch_add(v.data.len() as u64, Ordering::Relaxed);
        return Ok(());
    }
    // Mutable keys (shelf metadata) are brought up to date; content-named keys are identical by construction.
    if key.starts_with("shelves/") {
        if let Some(cur) = dst.get(key)? {
            if cur.data != v.data {
                dst.put_if_match(key, &v.data, &cur.tag)?;
                c.copied.fetch_add(1, Ordering::Relaxed);
                c.bytes.fetch_add(v.data.len() as u64, Ordering::Relaxed);
                return Ok(());
            }
        }
    }
    c.there.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Copy `src` into `dst` (see the module docs for the ordering that makes a kill at any point safe).
pub fn backup_copy(src: &dyn Backend, dst: &dyn Backend, opts: &CopyOptions) -> Result<CopyReport, DrError> {
    let hold = if opts.hold {
        Hold::acquire(src, &opts.owner, opts.hold_ttl_ms)?
    } else {
        None
    };
    let mut rep = CopyReport {
        held: hold.is_some(),
        ..Default::default()
    };
    if opts.require_empty_dest {
        if dst.get(MANIFEST_KEY)?.is_some() {
            return Err(DrError::NotEmpty("it has a manifest".into()));
        }
        for p in COPY_PREFIXES {
            if let Some(k) = dst.list(p)?.first() {
                return Err(DrError::NotEmpty(format!("it already holds {k}")));
            }
        }
    }
    // The snapshot the backup stands for. Taken after the hold, so nothing it names can be swept from here on.
    let (m, tag) = load_manifest(src)?;
    if tag.is_none() {
        return Err(DrError::NoManifest);
    }
    rep.branches = m.branches.len();
    let counters = Counters::default();
    for pass in 1..=3u32 {
        rep.passes = pass;
        counters.vanished.store(0, Ordering::Relaxed);
        let mut keys: Vec<String> = Vec::new();
        for p in COPY_PREFIXES {
            keys.extend(src.list(p)?);
        }
        for r in 0..=2u8 {
            let batch: Vec<&String> = keys.iter().filter(|k| rank(k) == r).collect();
            crate::par::map_bounded(&batch, crate::par::parallelism(), |k| {
                if let Some(h) = &hold {
                    h.renew()?;
                }
                copy_key(src, dst, k, &counters)
            })?;
        }
        if counters.vanished.load(Ordering::Relaxed) == 0 {
            break;
        }
        rep.vanished += counters.vanished.load(Ordering::Relaxed);
    }
    rep.copied = counters.copied.load(Ordering::Relaxed);
    rep.already_there = counters.there.load(Ordering::Relaxed);
    rep.bytes_copied = counters.bytes.load(Ordering::Relaxed);
    if let Some(h) = &hold {
        h.renew()?;
    }

    // Verify exactly the manifest we are about to install, in the destination, before it exists there.
    let v = verify_remote(
        dst,
        &VerifyOptions {
            manifest: Some(m.clone()),
            deep: opts.deep,
        },
    )?;
    if !v.is_ok() {
        let first = v.missing.first().or(v.corrupt.first());
        return Err(DrError::Verify(format!(
            "{} missing, {} corrupt in the destination{}",
            v.missing_total,
            v.corrupt_total,
            first.map_or(String::new(), |p| format!(
                " (first: {} {}: {})",
                p.id.map_or(String::new(), |i| i.short()),
                p.context,
                p.what
            ))
        )));
    }

    // Install the manifest, last. A destination that already has one is replaced by compare-and-swap.
    let bytes = m.encode();
    match dst.get(MANIFEST_KEY)? {
        None => {
            if !dst.put_if_absent(MANIFEST_KEY, &bytes)? {
                return Err(DrError::DestChanged);
            }
        }
        Some(cur) if cur.data == bytes => {}
        Some(cur) => {
            if dst.put_if_match(MANIFEST_KEY, &bytes, &cur.tag)?.is_none() {
                return Err(DrError::DestChanged);
            }
        }
    }
    rep.verify = v;
    Ok(rep)
}
