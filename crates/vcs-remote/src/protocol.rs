//! Publish and fetch.
//!
//! Publish step machine (every step idempotent; state lives only in the Publisher):
//!
//! ```text
//! Collect  walk local closure of new heads, stopping at objects the remote already has
//!          (invariant: a remote object's whole closure is present – upload is post-order)
//! Guard    (only on backends with `supports_gc`) the handshake that makes remote garbage
//!          collection safe, protocol G1 in docs/gc.md. For every remote key this publish relies
//!          on but did not just create (a legacy loose object Collect pruned at; a pack or
//!          index that `put_if_absent` found already there): Touch it, then check no
//!          `tomb/...` exists for it, then check it still exists. Loose objects are guarded
//!          BEFORE anything is uploaded (nothing that depends on them is visible while they can
//!          still be swept); a deduplicated pack right after PutIdx. A failed check, or the
//!          hold-time fence in front of the CAS ([`Publisher::with_hold_ms`]), restarts at Collect.
//! Upload   packed (default): PutPack `packs/<name>.pack`, then PutIdx `packs/<name>.idx`
//!          (the index is the commit point of the pack: no index, no objects; an orphan
//!          pack is ignored). Legacy/tiny: put_if_absent each loose object, dependencies first
//! PutTx    put_if_absent tx/<txid>
//! Load     read manifest; if txid already applied -> done; check every update.old == current
//! Cas      conditional manifest write  <-- linearization point
//! ```
//! Packed Collect asks the remote nothing per object. What the remote has is derived from
//! heads it is known to have (every `update.old`, checked again by the CAS in `Load`, plus all
//! manifest heads when a branch is new): their closures are present, so the commit walk stops
//! at them and a new tree is diffed against the same-named tree of a present parent, pruning
//! every entry that tree already lists. Argument and model-checked tests: docs/remote-packs.md.
//!
//! `Unavailable` at any step re-runs that step (bounded); after a CAS error we always go
//! back to `Load`, which detects "applied but ack lost". Exhausted retries yield
//! `PublishError::Unknown`: the outcome is undetermined and re-running the same transaction
//! is safe and converges.

use crate::backend::{Backend, BackendError};
use crate::manifest::{self, Manifest, MAX_APPLIED};
use crate::packfetch;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use vcs_objects::{id_of_encoded, EntryMode, Object, ObjectId, Transaction};
use vcs_store::pack::{build_pack, BuiltPack};
use vcs_store::{ObjectStore, StoreError};

pub const MAX_RETRIES: u32 = 8;

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("remote branch {zone}/{branch} moved: expected {}, found {}", fmt_id(.expected), fmt_id(.found))]
    Diverged {
        zone: String,
        branch: String,
        expected: Option<ObjectId>,
        found: Option<ObjectId>,
    },
    #[error("outcome unknown after {0} retries (last error: {1}); re-run the same publish, it is idempotent")]
    Unknown(u32, String),
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error("local store: {0}")]
    Local(#[from] StoreError),
    #[error("remote data invalid: {0}")]
    Remote(String),
    #[error("{path} in zone {zone} is locked by {holder} until {expires_at} (unix ms)")]
    LockedBy {
        zone: String,
        path: String,
        holder: String,
        expires_at: u64,
    },
    #[error("lock on {path} in zone {zone} is not held with the presented token (broken, expired and re-taken, or never held)")]
    LockBroken { zone: String, path: String },
    #[error("{path} in zone {zone} is locked by {holder}; only the holder can release it")]
    NotHolder { zone: String, path: String, holder: String },
    #[error("zone definition conflict: {0}")]
    ZoneConflict(String),
}

fn fmt_id(i: &Option<ObjectId>) -> String {
    i.map(|i| i.short()).unwrap_or_else(|| "(none)".into())
}

/// Tombstone key a sweeper writes for a loose object it is about to delete (docs/gc.md, G1).
pub fn tomb_key(id: &ObjectId) -> String {
    format!("tomb/{}", id.to_hex())
}
/// Tombstone key for a remote pack, given its stem (`packs/pack-<hex>`).
pub fn pack_tomb_key(stem: &str) -> String {
    format!("tomb/{}", stem.rsplit('/').next().unwrap_or(stem))
}

/// Default for [`Publisher::with_hold_ms`]: the longest a publish may take from Collect to its
/// CAS before it starts over. Remote GC's grace must exceed twice this (assumption A1).
pub const DEFAULT_HOLD_MS: u64 = 60 * 60 * 1000;

/// Switches that disable one rule of the GC protocol each. They exist only so tests can show,
/// rule by rule, that removing it loses data (the mutation tests of `model_gc.rs`, run against
/// the real code). Never set outside tests.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GcQuirks {
    /// Publisher: do not touch what it only deduplicated against.
    pub publisher_no_touch: bool,
    /// Publisher: do not look for a tombstone.
    pub publisher_no_check_tomb: bool,
    /// Publisher: do not re-check existence after the tombstone check.
    pub publisher_no_check_exists: bool,
    /// Publisher: CAS without the hold-time fence.
    pub publisher_no_fence: bool,
    /// Sweeper: decide what to delete, then write tombstones (instead of tombstones first).
    pub sweeper_tombs_after_deciding: bool,
    /// Sweeper: young objects and their dependencies are not roots.
    pub sweeper_young_not_roots: bool,
    /// Sweeper: ignore the manifest (no branch / snapshot / transaction roots).
    pub sweeper_ignores_manifest: bool,
}

/// What to do when the current guard list has been processed.
#[derive(Clone, Copy)]
enum Then {
    PutPack,
    Upload,
    PutTx,
}

pub fn object_key(id: &ObjectId) -> String {
    let h = id.to_hex();
    format!("objects/{}/{}", &h[..2], &h[2..])
}
pub(crate) fn tx_key(id: &ObjectId) -> String {
    format!("tx/{}", id.to_hex())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Applied {
        generation: u64,
    },
    /// A previous attempt (possibly by a crashed/ack-lost client) already committed this tx.
    AlreadyApplied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Continue,
    Done(Outcome),
}

enum State {
    Collect,
    Upload(usize),
    PutPack,
    PutIdx,
    /// GC handshake over `Publisher::guard`: phase 0 touch, 1 tombstone check, 2 existence check.
    Guard {
        phase: u8,
        i: usize,
    },
    PutTx,
    Load,
    Cas {
        expected_tag: Option<String>,
        new: Box<Manifest>,
    },
    Finished,
}

pub struct Publisher<'a> {
    backend: &'a dyn Backend,
    local: &'a dyn ObjectStore,
    tx: Transaction,
    txid: ObjectId,
    state: State,
    to_upload: Vec<ObjectId>,
    /// Packed mode: the bundle of this transaction's new objects, built once in Collect.
    built: Option<BuiltPack>,
    packed: bool,
    retries: u32,
    zones: BTreeMap<String, String>,
    labels: BTreeMap<String, String>,
    tags: BTreeMap<(String, String), ObjectId>,
    principal: String,
    clock: Box<dyn Fn() -> u64 + 'a>,
    fault: Option<crate::locks::LockFault>,
    skew_eps: u64,
    /// (remote key, tombstone key) pairs the GC handshake covers next.
    guard: Vec<(String, String)>,
    guard_then: Then,
    /// Pack keys `put_if_absent` found already present in this attempt.
    dedup: Vec<(String, String)>,
    /// Objects uploaded by an earlier, abandoned attempt (loose layout only): not trusted to
    /// prune at, because GC may have removed their dependencies since.
    distrust: HashSet<ObjectId>,
    /// Backend-clock time this attempt started, for the hold-time fence.
    started_ms: u64,
    hold_ms: u64,
    quirks: GcQuirks,
}

/// Dependencies of an object (what must exist remotely before it may).
pub(crate) fn deps(o: &Object) -> Vec<ObjectId> {
    deps_of(o, false)
}

/// Like [`deps`], but with `skip_files` a tree contributes only its subdirectories, so a walk
/// reaches commits and trees and never a file blob or chunk manifest.
fn deps_of(o: &Object, skip_files: bool) -> Vec<ObjectId> {
    match o {
        Object::Tree(t) if skip_files => t
            .entries()
            .iter()
            .filter(|e| e.mode == EntryMode::Directory)
            .map(|e| e.id)
            .collect(),
        Object::Chunked(m) => m.chunks.iter().map(|c| c.id).collect(),
        Object::Blob(_) | Object::Transaction(_) | Object::Operation(_) => vec![],
        Object::Tree(t) => t.entries().iter().map(|e| e.id).collect(),
        Object::Commit(c) => c.parents.iter().copied().chain(std::iter::once(c.root)).collect(),
        Object::Snapshot(s) => s.zones.values().copied().collect(),
        Object::Tag(t) => vec![t.target],
    }
}

/// Iterative post-order walk (dependencies before dependents) from `root`, pruning at
/// objects for which `present` is true. No recursion: histories may be very deep.
fn post_order(
    root: ObjectId,
    present: &mut dyn FnMut(&ObjectId) -> Result<bool, PublishError>,
    load: &mut dyn FnMut(&ObjectId) -> Result<Object, PublishError>,
    emit: &mut dyn FnMut(&ObjectId, &Object) -> Result<(), PublishError>,
    done: &mut HashSet<ObjectId>,
    deps: &dyn Fn(&Object) -> Vec<ObjectId>,
) -> Result<(), PublishError> {
    // `done` = emitted or known-present. A node may be pushed more than once (shared
    // subobjects); only the first pop that finds it not done expands it, and its
    // dependencies are always emitted before it because they sit above it on the stack.
    if done.contains(&root) {
        return Ok(());
    }
    if present(&root)? {
        done.insert(root);
        return Ok(());
    }
    let mut stack: Vec<(ObjectId, Option<Object>)> = vec![(root, None)];
    while let Some((id, obj)) = stack.pop() {
        if done.contains(&id) {
            continue;
        }
        match obj {
            Some(o) => {
                emit(&id, &o)?;
                done.insert(id);
            }
            None => {
                let o = load(&id)?;
                let ds = deps(&o);
                stack.push((id, Some(o)));
                for d in ds.into_iter().rev() {
                    if done.contains(&d) {
                        continue;
                    }
                    if present(&d)? {
                        done.insert(d);
                        continue;
                    }
                    stack.push((d, None));
                }
            }
        }
    }
    Ok(())
}

impl<'a> Publisher<'a> {
    pub fn new(backend: &'a dyn Backend, local: &'a dyn ObjectStore, tx: Transaction) -> Self {
        let txid = Object::Transaction(tx.clone()).id();
        Publisher {
            backend,
            local,
            tx,
            txid,
            state: State::Collect,
            to_upload: vec![],
            built: None,
            packed: true,
            retries: 0,
            zones: BTreeMap::new(),
            labels: BTreeMap::new(),
            tags: BTreeMap::new(),
            principal: String::new(),
            clock: Box::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0)
            }),
            fault: None,
            skew_eps: 0,
            guard: vec![],
            guard_then: Then::PutTx,
            dedup: vec![],
            distrust: HashSet::new(),
            started_ms: 0,
            hold_ms: DEFAULT_HOLD_MS,
            quirks: GcQuirks::default(),
        }
    }
    /// Longest time (backend clock) from the start of an attempt to its CAS. A slower publish
    /// starts over from Collect, re-touching what it relies on, so remote GC can bound how long
    /// an unreferenced-but-needed object may look old (A1 in docs/gc.md). Only enforced against
    /// backends with `supports_gc`.
    pub fn with_hold_ms(mut self, ms: u64) -> Self {
        self.hold_ms = ms;
        self
    }
    #[doc(hidden)]
    pub fn with_gc_quirks(mut self, q: GcQuirks) -> Self {
        self.quirks = q;
        self
    }
    /// Write every object as its own key (`objects/xx/...`, one HEAD per dependency) instead of
    /// one pack. The legacy layout; readers handle both, so this only exists for old-layout tests.
    pub fn with_loose_objects(mut self) -> Self {
        self.packed = false;
        self
    }
    /// The authenticated identity lock decisions are made for (NOT `Transaction.author`).
    pub fn with_principal(mut self, p: impl Into<String>) -> Self {
        self.principal = p.into();
        self
    }
    /// Validator clock in unix ms (injectable for tests and the model checker).
    pub fn with_clock(mut self, c: impl Fn() -> u64 + 'a) -> Self {
        self.clock = Box::new(c);
        self
    }
    /// Replica clock-skew allowance in ms (default 0): locks stay live this long past
    /// `expires_at` (docs/locks.md, "Clock handling").
    pub fn with_skew_epsilon(mut self, eps_ms: u64) -> Self {
        self.skew_eps = eps_ms;
        self
    }
    #[doc(hidden)]
    pub fn with_lock_fault(mut self, f: crate::locks::LockFault) -> Self {
        self.fault = Some(f);
        self
    }
    /// Zone definitions (name -> path) to register in the manifest atomically with this tx.
    pub fn with_zones(mut self, zones: BTreeMap<String, String>) -> Self {
        self.zones = zones;
        self
    }
    /// Display names (zone id -> name) of renamed zones, recorded in the manifest with this tx.
    /// A later publisher's label for a zone replaces the stored one.
    pub fn with_labels(mut self, labels: BTreeMap<String, String>) -> Self {
        self.labels = labels;
        self
    }
    /// Tags ((zone, name) -> commit or annotated tag object) to record in the manifest with this
    /// tx. Their objects are uploaded first. A tag the remote already has with another target is
    /// a conflict (tags are never moved by a publish); an identical one is a no-op.
    pub fn with_tags(mut self, tags: BTreeMap<(String, String), ObjectId>) -> Self {
        self.tags = tags;
        self
    }
    pub fn txid(&self) -> ObjectId {
        self.txid
    }
    pub fn is_finished(&self) -> bool {
        matches!(self.state, State::Finished)
    }
    /// Name of the next step (for the model checker).
    pub fn next_is_cas(&self) -> bool {
        matches!(self.state, State::Cas { .. })
    }

    pub fn run(mut self) -> Result<Outcome, PublishError> {
        loop {
            if let Step::Done(o) = self.step()? {
                return Ok(o);
            }
        }
    }

    fn retry(&mut self, e: BackendError) -> Result<Step, PublishError> {
        match e {
            BackendError::Unavailable(m) => {
                self.retries += 1;
                if self.retries > MAX_RETRIES {
                    self.state = State::Finished;
                    return Err(PublishError::Unknown(MAX_RETRIES, m));
                }
                Ok(Step::Continue)
            }
            other => {
                self.state = State::Finished;
                Err(other.into())
            }
        }
    }

    /// Packed Collect: the new objects (id, encoded), dependencies first, found without asking
    /// the remote about individual objects. See the module docs for why pruning is sound.
    #[allow(clippy::type_complexity)]
    fn collect_packed(&self) -> Result<(Vec<(ObjectId, Vec<u8>)>, Vec<ObjectId>), PublishError> {
        // Legacy loose objects this walk pruned at (they need the GC handshake).
        let loose_pruned = std::cell::RefCell::new(Vec::<ObjectId>::new());
        // Objects whose whole closure is on the remote (or will be, see `Load`).
        let mut known: HashSet<ObjectId> = self.tx.updates.iter().filter_map(|u| u.old).collect();
        if self.tx.updates.iter().any(|u| u.old.is_none()) {
            // A new branch has no `old` to lean on: its parent may be any current head.
            let (m, _) = load_manifest(self.backend)?;
            known.extend(m.branches.values().copied());
        }
        let remote_has_history = !known.is_empty();
        let heads_known = known.clone();
        let mut ancestors: Option<HashSet<ObjectId>> = None;
        // new tree/file id -> id of the same-named entry in a present parent (its closure is present)
        let mut hint: HashMap<ObjectId, ObjectId> = HashMap::new();
        let mut done: HashSet<ObjectId> = HashSet::new();
        let mut out: Vec<(ObjectId, Vec<u8>)> = Vec::new();
        let local = self.local;

        let mut commit_on_remote = |id: &ObjectId, known: &HashSet<ObjectId>| -> Result<bool, PublishError> {
            if known.contains(id) {
                return Ok(true);
            }
            let anc = ancestors.get_or_insert_with(|| {
                let mut seen: HashSet<ObjectId> = HashSet::new();
                let mut stack: Vec<ObjectId> = heads_known.iter().copied().collect();
                while let Some(c) = stack.pop() {
                    if !seen.insert(c) {
                        continue;
                    }
                    if let Ok(Object::Commit(c)) = local.get(&c) {
                        stack.extend(c.parents.iter().copied());
                    }
                }
                seen
            });
            if anc.contains(id) {
                return Ok(true);
            }
            // Not derivable from the heads: maybe a legacy loose upload. One HEAD for a commit.
            let here = self.backend.exists(&object_key(id))?;
            if here {
                loose_pruned.borrow_mut().push(*id);
            }
            Ok(here)
        };

        // Branch heads and tag targets (annotated tag objects are not reachable from any commit).
        let heads: BTreeSet<ObjectId> = self.tx.updates.iter().map(|u| u.new).chain(self.tags.values().copied()).collect();
        for h in heads {
            let mut stack: Vec<(ObjectId, Option<Object>)> = vec![(h, None)];
            while let Some((id, obj)) = stack.pop() {
                if known.contains(&id) || done.contains(&id) {
                    continue;
                }
                if let Some(o) = obj {
                    out.push((id, o.encode()));
                    done.insert(id);
                    continue;
                }
                let o = local.get(&id)?;
                let mut ds: Vec<ObjectId> = Vec::new();
                match &o {
                    Object::Commit(c) => {
                        if remote_has_history && commit_on_remote(&id, &known)? {
                            known.insert(id);
                            continue;
                        }
                        for p in &c.parents {
                            if remote_has_history && !done.contains(p) && commit_on_remote(p, &known)? {
                                known.insert(*p);
                            }
                            if known.contains(p) {
                                if let Ok(Object::Commit(pc)) = local.get(p) {
                                    hint.entry(c.root).or_insert(pc.root);
                                }
                            }
                        }
                        ds.extend(c.parents.iter().copied());
                        ds.push(c.root);
                    }
                    Object::Tree(t) => {
                        let old = hint.get(&id).and_then(|oid| match local.get(oid) {
                            Ok(Object::Tree(ot)) => Some(ot),
                            _ => None,
                        });
                        let mut by_name: HashMap<&str, (&ObjectId, EntryMode)> = HashMap::new();
                        if let Some(ot) = &old {
                            for e in ot.entries() {
                                known.insert(e.id);
                                by_name.insert(e.name.as_str(), (&e.id, e.mode));
                            }
                        }
                        for e in t.entries() {
                            if known.contains(&e.id) {
                                continue;
                            }
                            if let Some((oid, mode)) = by_name.get(e.name.as_str()) {
                                if *mode == e.mode {
                                    hint.entry(e.id).or_insert(**oid);
                                }
                            }
                            ds.push(e.id);
                        }
                    }
                    Object::Chunked(m) => {
                        // an edited big file shares most chunks with its previous version
                        if let Some(Ok(Object::Chunked(om))) = hint.get(&id).map(|oid| local.get(oid)) {
                            known.extend(om.chunks.iter().map(|c| c.id));
                        }
                        ds.extend(m.chunks.iter().map(|c| c.id));
                    }
                    other => ds = deps(other),
                }
                stack.push((id, Some(o)));
                for d in ds.into_iter().rev() {
                    if !known.contains(&d) && !done.contains(&d) {
                        stack.push((d, None));
                    }
                }
            }
        }
        // Commits and trees first: a clone reads that prefix of the pack in a few big requests.
        // Stable, and within a group still dependencies-first.
        out.sort_by_key(|(_, enc)| match enc.first() {
            Some(3) => 0u8,
            Some(2) => 1,
            _ => 2,
        });
        let mut lp = loose_pruned.into_inner();
        lp.sort();
        lp.dedup();
        Ok((out, lp))
    }

    /// Arm the GC handshake over `keys` and continue with `then`; no-op unless the backend
    /// supports GC and there is something to guard.
    fn guard_then(&mut self, keys: Vec<(String, String)>, then: Then) -> State {
        if keys.is_empty() || !self.backend.supports_gc() {
            return match then {
                Then::PutPack => State::PutPack,
                Then::Upload => State::Upload(0),
                Then::PutTx => State::PutTx,
            };
        }
        self.guard = keys;
        self.guard_then = then;
        State::Guard { phase: 0, i: 0 }
    }

    /// One step of the GC handshake (Touch < CheckTomb < CheckExists < CAS for each guarded
    /// key). Any failed check abandons the attempt and redoes Collect.
    fn guard_step(&mut self, mut phase: u8, mut i: usize) -> Result<Step, PublishError> {
        // Normalise: skip finished or disabled phases without spending a step.
        loop {
            let skip = match phase {
                0 => self.quirks.publisher_no_touch,
                1 => self.quirks.publisher_no_check_tomb,
                _ => self.quirks.publisher_no_check_exists,
            };
            if phase > 2 || (phase == 2 && (skip || i >= self.guard.len())) {
                self.state = match self.guard_then {
                    Then::PutPack => State::PutPack,
                    Then::Upload => State::Upload(0),
                    Then::PutTx => State::PutTx,
                };
                return Ok(Step::Continue);
            }
            if skip || i >= self.guard.len() {
                phase += 1;
                i = 0;
                continue;
            }
            break;
        }
        let (key, tomb) = self.guard[i].clone();
        let r: Result<bool, BackendError> = match phase {
            0 => self.backend.touch(&key),               // false: gone
            1 => self.backend.exists(&tomb).map(|t| !t), // false: tombstoned
            _ => self.backend.exists(&key),              // false: gone
        };
        match r {
            Ok(true) => {
                self.state = State::Guard { phase, i: i + 1 };
                Ok(Step::Continue)
            }
            Ok(false) => {
                self.state = State::Collect;
                self.retry(BackendError::Unavailable(match phase {
                    1 => format!("{key} is tombstoned by a running gc"),
                    _ => format!("{key} vanished (swept) before the CAS"),
                }))
            }
            Err(e) => {
                self.state = State::Guard { phase, i };
                self.retry(e)
            }
        }
    }

    pub fn step(&mut self) -> Result<Step, PublishError> {
        match std::mem::replace(&mut self.state, State::Finished) {
            State::Finished => Ok(Step::Continue),
            State::Collect if self.packed => {
                if self.backend.supports_gc() {
                    match self.backend.now_ms() {
                        Ok(t) => self.started_ms = t,
                        Err(e) => {
                            self.state = State::Collect;
                            return self.retry(e);
                        }
                    }
                }
                self.dedup.clear();
                match self.collect_packed() {
                    Ok((staged, loose_pruned)) => {
                        let then = if staged.len() >= 2 {
                            self.built = Some(build_pack(staged));
                            Then::PutPack
                        } else {
                            // zero or one object: a pack would cost more than it saves
                            self.to_upload = staged.iter().map(|s| s.0).collect();
                            Then::Upload
                        };
                        let keys = loose_pruned.iter().map(|id| (object_key(id), tomb_key(id))).collect();
                        self.state = self.guard_then(keys, then);
                        Ok(Step::Continue)
                    }
                    Err(PublishError::Backend(e)) => {
                        self.state = State::Collect;
                        self.retry(e)
                    }
                    Err(e) => Err(e),
                }
            }
            State::Collect => {
                let gc = self.backend.supports_gc();
                if gc {
                    match self.backend.now_ms() {
                        Ok(t) => self.started_ms = t,
                        Err(e) => {
                            self.state = State::Collect;
                            return self.retry(e);
                        }
                    }
                }
                let pruned = std::cell::RefCell::new(Vec::new());
                let distrust = std::mem::take(&mut self.distrust);
                let mut list = Vec::new();
                let mut seen = HashSet::new();
                let heads: BTreeSet<ObjectId> = self.tx.updates.iter().map(|u| u.new).chain(self.tags.values().copied()).collect();
                for h in heads {
                    let (backend, local) = (self.backend, self.local);
                    let r = post_order(
                        h,
                        &mut |id| {
                            if distrust.contains(id) {
                                return Ok(false);
                            }
                            let here = backend.exists(&object_key(id))?;
                            if here {
                                pruned.borrow_mut().push(*id);
                            }
                            Ok(here)
                        },
                        &mut |id| local.get(id).map_err(Into::into),
                        &mut |id, _| {
                            list.push(*id);
                            Ok(())
                        },
                        &mut seen,
                        &deps,
                    );
                    match r {
                        Err(PublishError::Backend(e)) => {
                            self.distrust = distrust;
                            self.state = State::Collect;
                            return self.retry(e);
                        }
                        Err(e) => return Err(e),
                        Ok(()) => {}
                    }
                }
                self.to_upload = list;
                self.distrust = distrust;
                let mut p = pruned.into_inner();
                p.sort();
                p.dedup();
                let keys = p.iter().map(|id| (object_key(id), tomb_key(id))).collect();
                self.state = self.guard_then(keys, Then::Upload);
                Ok(Step::Continue)
            }
            State::Guard { phase, i } => self.guard_step(phase, i),
            State::PutPack => {
                let b = self.built.as_ref().expect("built in Collect");
                let (name, pack) = (b.name.clone(), &b.pack);
                let key = format!("{}{name}.pack", packfetch::PACK_PREFIX);
                match self.backend.put_if_absent(&key, pack) {
                    Ok(created) => {
                        if !created {
                            self.dedup.push((key, pack_tomb_key(&name)));
                        }
                        self.state = State::PutIdx;
                    }
                    Err(e) => {
                        self.state = State::PutPack;
                        return self.retry(e);
                    }
                }
                Ok(Step::Continue)
            }
            State::PutIdx => {
                let b = self.built.as_ref().expect("built in Collect");
                let (name, idx) = (b.name.clone(), &b.idx);
                let key = format!("{}{name}.idx", packfetch::PACK_PREFIX);
                match self.backend.put_if_absent(&key, idx) {
                    Ok(created) => {
                        if !created {
                            self.dedup.push((key, pack_tomb_key(&name)));
                        }
                        let keys = std::mem::take(&mut self.dedup);
                        self.state = self.guard_then(keys, Then::PutTx);
                    }
                    Err(e) => {
                        self.state = State::PutIdx;
                        return self.retry(e);
                    }
                }
                Ok(Step::Continue)
            }
            State::Upload(i) => {
                if i >= self.to_upload.len() {
                    self.distrust.extend(self.to_upload.iter().copied());
                    self.state = State::PutTx;
                    return Ok(Step::Continue);
                }
                let id = self.to_upload[i];
                let obj = self.local.get(&id)?;
                match self.backend.put_if_absent(&object_key(&id), &obj.encode()) {
                    Ok(_) => self.state = State::Upload(i + 1),
                    Err(e) => {
                        self.state = State::Upload(i);
                        return self.retry(e);
                    }
                }
                Ok(Step::Continue)
            }
            State::PutTx => {
                match self
                    .backend
                    .put_if_absent(&tx_key(&self.txid), &Object::Transaction(self.tx.clone()).encode())
                {
                    Ok(_) => self.state = State::Load,
                    Err(e) => {
                        self.state = State::PutTx;
                        return self.retry(e);
                    }
                }
                Ok(Step::Continue)
            }
            State::Load => {
                let (cur, tag) = match load_manifest(self.backend) {
                    Ok(x) => x,
                    Err(PublishError::Backend(e)) => {
                        self.state = State::Load;
                        return self.retry(e);
                    }
                    Err(e) => return Err(e),
                };
                if cur.applied.contains(&self.txid) {
                    self.state = State::Finished;
                    return Ok(Step::Done(Outcome::AlreadyApplied));
                }
                for u in &self.tx.updates {
                    let found = cur.head(&u.zone, &u.branch);
                    if found != u.old {
                        self.state = State::Finished;
                        return Err(PublishError::Diverged {
                            zone: u.zone.clone(),
                            branch: u.branch.clone(),
                            expected: u.old,
                            found,
                        });
                    }
                }
                let mut new = cur.clone();
                for (n, p) in &self.zones {
                    match cur.zones.get(n) {
                        Some(q) if q == p => {}
                        Some(q) => {
                            self.state = State::Finished;
                            return Err(PublishError::ZoneConflict(format!(
                                "zone {n:?} is at {q:?} on the remote, {p:?} locally"
                            )));
                        }
                        None => {
                            if let Some((o, q)) = cur.zones.iter().find(|(_, q)| vcs_objects::names::paths_overlap(q, p)) {
                                self.state = State::Finished;
                                return Err(PublishError::ZoneConflict(format!(
                                    "zone {n:?} ({p:?}) overlaps remote zone {o:?} ({q:?})"
                                )));
                            }
                            new.zones.insert(n.clone(), p.clone());
                        }
                    }
                }
                for n in self.zones.keys() {
                    match self.labels.get(n) {
                        Some(l) => new.labels.insert(n.clone(), l.clone()),
                        None => new.labels.remove(n),
                    };
                }
                for ((z, n), id) in &self.tags {
                    match cur.tags.get(&(z.clone(), n.clone())) {
                        Some(q) if q == id => {}
                        Some(q) => {
                            self.state = State::Finished;
                            return Err(PublishError::Remote(format!(
                                "tag {z}/{n} already exists on the remote at {}; tags are not moved",
                                q.short()
                            )));
                        }
                        None => {
                            new.tags.insert((z.clone(), n.clone()), *id);
                        }
                    }
                }
                if !self.tx.lock_ops.is_empty() || cur.has_lock_state() {
                    let (backend, now) = (self.backend, (self.clock)());
                    let r = crate::locks::apply(
                        &cur,
                        &mut new,
                        &self.tx,
                        &self.principal,
                        now,
                        self.skew_eps,
                        self.fault,
                        &mut |i| {
                            let u = &self.tx.updates[i];
                            changed_paths(backend, u.old, u.new)
                        },
                    );
                    match r {
                        Ok(()) => {}
                        Err(PublishError::Backend(e)) => {
                            self.state = State::Load;
                            return self.retry(e);
                        }
                        Err(e) => {
                            self.state = State::Finished;
                            return Err(e);
                        }
                    }
                }
                new.generation += 1;
                for u in &self.tx.updates {
                    new.branches.insert((u.zone.clone(), u.branch.clone()), u.new);
                }
                new.applied.push(self.txid);
                if new.applied.len() > MAX_APPLIED {
                    new.applied.remove(0);
                }
                self.state = State::Cas {
                    expected_tag: tag,
                    new: Box::new(new),
                };
                Ok(Step::Continue)
            }
            State::Cas { expected_tag, new } => {
                if self.backend.supports_gc() && !self.quirks.publisher_no_fence {
                    let now = match self.backend.now_ms() {
                        Ok(t) => t,
                        Err(e) => {
                            self.state = State::Cas { expected_tag, new };
                            return self.retry(e);
                        }
                    };
                    if now.saturating_sub(self.started_ms) > self.hold_ms {
                        // Too slow: what this publish relies on may have aged past GC's grace.
                        // Start over (re-touching everything it relies on); the retry counter
                        // bounds how often.
                        self.state = State::Collect;
                        return self.retry(BackendError::Unavailable("hold time exceeded before CAS".into()));
                    }
                }
                let bytes = new.encode();
                let r = match &expected_tag {
                    None => self
                        .backend
                        .put_if_absent(manifest::KEY, &bytes)
                        .map(|created| created.then(String::new)),
                    Some(t) => self.backend.put_if_match(manifest::KEY, &bytes, t),
                };
                match r {
                    Ok(Some(_)) => {
                        self.state = State::Finished;
                        Ok(Step::Done(Outcome::Applied {
                            generation: new.generation,
                        }))
                    }
                    // Lost the race: reload, re-validate `old` heads (disjoint writers reconcile here).
                    Ok(None) => {
                        self.state = State::Load;
                        Ok(Step::Continue)
                    }
                    // Unknown whether applied: Load will find our txid if it was.
                    Err(e) => {
                        self.state = State::Load;
                        self.retry(e)
                    }
                }
            }
        }
    }
}

fn tree_of(backend: &dyn Backend, id: &ObjectId) -> Result<vcs_objects::Tree, PublishError> {
    match fetch_object(backend, id)? {
        Object::Tree(t) => Ok(t),
        _ => Err(PublishError::Remote(format!("object {id} is not a tree"))),
    }
}
fn root_of(backend: &dyn Backend, commit: &ObjectId) -> Result<ObjectId, PublishError> {
    match fetch_object(backend, commit)? {
        Object::Commit(c) => Ok(c.root),
        _ => Err(PublishError::Remote(format!("object {commit} is not a commit"))),
    }
}

/// All files at or below an entry (a file yields itself).
fn files_under(backend: &dyn Backend, prefix: &str, e: &vcs_objects::TreeEntry, out: &mut BTreeSet<String>) -> Result<(), PublishError> {
    let path = if prefix.is_empty() {
        e.name.clone()
    } else {
        format!("{prefix}/{}", e.name)
    };
    if e.mode == vcs_objects::EntryMode::Directory {
        for c in tree_of(backend, &e.id)?.entries() {
            files_under(backend, &path, c, out)?;
        }
    } else {
        out.insert(path);
    }
    Ok(())
}

fn diff_trees(
    backend: &dyn Backend,
    prefix: &str,
    a: Option<&ObjectId>,
    b: &ObjectId,
    out: &mut BTreeSet<String>,
) -> Result<(), PublishError> {
    let tb = tree_of(backend, b)?;
    let ta = match a {
        Some(a) if a == b => return Ok(()),
        Some(a) => Some(tree_of(backend, a)?),
        None => None,
    };
    let old: BTreeMap<&str, &vcs_objects::TreeEntry> = ta.iter().flat_map(|t| t.entries().iter()).map(|e| (e.name.as_str(), e)).collect();
    let join = |n: &str| if prefix.is_empty() { n.to_owned() } else { format!("{prefix}/{n}") };
    for e in tb.entries() {
        match old.get(e.name.as_str()) {
            Some(o) if o.id == e.id && o.mode == e.mode => {}
            Some(o) if o.mode == vcs_objects::EntryMode::Directory && e.mode == vcs_objects::EntryMode::Directory => {
                diff_trees(backend, &join(&e.name), Some(&o.id), &e.id, out)?
            }
            Some(o) => {
                files_under(backend, prefix, o, out)?;
                files_under(backend, prefix, e, out)?;
            }
            None => files_under(backend, prefix, e, out)?,
        }
    }
    let new_names: BTreeSet<&str> = tb.entries().iter().map(|e| e.name.as_str()).collect();
    for (n, o) in &old {
        if !new_names.contains(n) {
            files_under(backend, prefix, o, out)?;
        }
    }
    Ok(())
}

/// Net changed file paths (zone-relative) between two commits: adds, modifications, deletions,
/// mode changes; a rename contributes both paths. `old = None` diffs against the empty tree.
pub fn changed_paths(backend: &dyn Backend, old: Option<ObjectId>, new: ObjectId) -> Result<BTreeSet<String>, PublishError> {
    let nr = root_of(backend, &new)?;
    let or = old.map(|o| root_of(backend, &o)).transpose()?;
    let mut out = BTreeSet::new();
    diff_trees(backend, "", or.as_ref(), &nr, &mut out)?;
    Ok(out)
}

/// Read and validate the manifest. Missing manifest = empty repository (tag `None`).
pub fn load_manifest(backend: &dyn Backend) -> Result<(Manifest, Option<String>), PublishError> {
    match backend.get(manifest::KEY)? {
        None => Ok((Manifest::default(), None)),
        Some(v) => {
            let m = Manifest::decode(&v.data).map_err(|e| PublishError::Remote(e.to_string()))?;
            Ok((m, Some(v.tag)))
        }
    }
}

/// Fetch ONE object by id from the remote and verify its hash before returning it.
/// Nothing is stored: the caller decides, so a corrupt remote object can never reach a store.
pub fn fetch_object(backend: &dyn Backend, id: &ObjectId) -> Result<Object, PublishError> {
    let data = match backend.get(&object_key(id))? {
        Some(v) => v.data,
        // Not stored loose: try remote packs (`packs/<name>.pack` + `.idx`), reading only the
        // small index and the one object's byte range.
        None => {
            packfetch::fetch_from_any_pack(backend, id)?.ok_or_else(|| PublishError::Remote(format!("object {id} missing on remote")))?
        }
    };
    if id_of_encoded(&data) != *id {
        return Err(PublishError::Remote(format!("object {id} fails hash verification")));
    }
    Object::decode(&data).map_err(|e| PublishError::Remote(format!("object {id}: {e}")))
}

/// Fetch many objects with as few requests as the remote layout allows (see
/// [`packfetch::fetch_many`]), verifying every hash. Order of the result is not specified.
/// Nothing is stored. An id the remote lacks is an error.
pub fn fetch_objects(backend: &dyn Backend, ids: &[ObjectId]) -> Result<Vec<(ObjectId, Object)>, PublishError> {
    let mut out = Vec::with_capacity(ids.len());
    for (id, data) in packfetch::fetch_many(backend, ids)? {
        if id_of_encoded(&data) != id {
            return Err(PublishError::Remote(format!("object {id} fails hash verification")));
        }
        out.push((
            id,
            Object::decode(&data).map_err(|e| PublishError::Remote(format!("object {id}: {e}")))?,
        ));
    }
    Ok(out)
}

/// Fetch the closure of the heads of `zones` (None = all) into `local`, storing
/// dependencies first and verifying every object's hash. Returns the fetched heads.
/// Only objects reachable from the requested zones are read.
pub fn fetch_heads(
    backend: &dyn Backend,
    local: &dyn ObjectStore,
    zones: Option<&[String]>,
) -> Result<Vec<(String, String, ObjectId)>, PublishError> {
    fetch_heads_impl(backend, local, zones, false)
}

/// Fetch the closure (commits and trees only) of every remote tag of `zones` (None = all) into
/// `local`, verifying hashes and storing dependencies first. Returns the tags fetched.
pub fn fetch_tags_lazy(
    backend: &dyn Backend,
    local: &dyn ObjectStore,
    zones: Option<&[String]>,
) -> Result<Vec<(String, String, ObjectId)>, PublishError> {
    let (m, _) = load_manifest(backend)?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for ((z, n), id) in &m.tags {
        if zones.is_some_and(|w| !w.iter().any(|x| x == z)) {
            continue;
        }
        post_order(
            *id,
            &mut |i| Ok(local.contains(i)?),
            &mut |i| fetch_object(backend, i),
            &mut |_, o| {
                local.put(o)?;
                Ok(())
            },
            &mut seen,
            &|o| deps_of(o, true),
        )?;
        out.push((z.clone(), n.clone(), *id));
    }
    local.sync()?;
    Ok(out)
}

/// Like [`fetch_heads`] but fetches only commits and trees. File blobs (and chunk manifests)
/// are left on the remote and come later via [`fetch_objects`] or [`fetch_object`].
pub fn fetch_heads_lazy(
    backend: &dyn Backend,
    local: &dyn ObjectStore,
    zones: Option<&[String]>,
) -> Result<Vec<(String, String, ObjectId)>, PublishError> {
    fetch_heads_impl(backend, local, zones, true)
}

fn fetch_heads_impl(
    backend: &dyn Backend,
    local: &dyn ObjectStore,
    zones: Option<&[String]>,
    skip_files: bool,
) -> Result<Vec<(String, String, ObjectId)>, PublishError> {
    let (m, _) = load_manifest(backend)?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    // Packs are listed and indexed once, on first need. A lazy walk (commits and trees) reads the
    // leading metadata of each pack it touches in a few large requests; a full walk of every
    // zone reads whole packs; a zone-filtered full walk reads object ranges.
    let ahead = if skip_files {
        packfetch::ReadAhead::Metadata
    } else if zones.is_none() {
        packfetch::ReadAhead::Whole
    } else {
        packfetch::ReadAhead::None
    };
    let mut src = packfetch::RemoteSource::new(backend, ahead);
    for ((a, b), id) in &m.branches {
        if zones.is_some_and(|w| !w.iter().any(|x| x == a)) {
            continue;
        }
        post_order(
            *id,
            &mut |i| Ok(local.contains(i)?),
            &mut |i| {
                let data = src
                    .fetch(i)?
                    .ok_or_else(|| PublishError::Remote(format!("object {i} missing on remote")))?;
                if id_of_encoded(&data) != *i {
                    return Err(PublishError::Remote(format!("object {i} fails hash verification")));
                }
                Object::decode(&data).map_err(|e| PublishError::Remote(format!("object {i}: {e}")))
            },
            &mut |_, o| {
                local.put(o)?;
                Ok(())
            },
            &mut seen,
            &|o| deps_of(o, skip_files),
        )?;
        out.push((a.clone(), b.clone(), *id));
    }
    // Durability barrier: fetched objects are durable before the caller moves any ref to them.
    local.sync()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use vcs_objects::{EntryMode, Tree, TreeEntry};
    use vcs_store::MemoryStore;

    #[test]
    fn post_order_diamond_emits_dependencies_first() {
        let s = MemoryStore::new();
        let blob = s.put_blob(b"x").unwrap();
        let e = |n: &str, m, id| TreeEntry {
            name: n.into(),
            mode: m,
            id,
        };
        // C (shared) <- B ; A -> B, A -> C  (names chosen so C is visited before B is expanded)
        let c = s
            .put(&Object::Tree(Tree::new(vec![e("f", EntryMode::File, blob)]).unwrap()))
            .unwrap();
        let b = s
            .put(&Object::Tree(Tree::new(vec![e("c", EntryMode::Directory, c)]).unwrap()))
            .unwrap();
        let a = s
            .put(&Object::Tree(
                Tree::new(vec![e("b", EntryMode::Directory, b), e("c", EntryMode::Directory, c)]).unwrap(),
            ))
            .unwrap();
        let mut order = Vec::new();
        post_order(
            a,
            &mut |_| Ok(false),
            &mut |i| Ok(s.get(i)?),
            &mut |i, _| {
                order.push(*i);
                Ok(())
            },
            &mut HashSet::new(),
            &deps,
        )
        .unwrap();
        let pos: BTreeMap<_, _> = order.iter().enumerate().map(|(i, x)| (*x, i)).collect();
        assert_eq!(order.len(), 4, "each object exactly once");
        assert!(pos[&blob] < pos[&c] && pos[&c] < pos[&b] && pos[&b] < pos[&a]);
    }
}
