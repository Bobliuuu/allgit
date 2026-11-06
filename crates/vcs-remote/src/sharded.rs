//! Per-zone sharded manifest (issues #252, #313): the production layer on top of the protocol checked
//! in `tests/model_sharded.rs`, `model_sharded_migrate.rs` and `model_sharded_gc.rs`.
//!
//! Opt-in per repository. A repository starts with the single `manifest` object. `migrate_to_sharded`
//! replaces it, after a crash-safe freeze, by a *moved marker*; from then on the authoritative state
//! is a set of participant records, one per zone plus one small root record:
//!
//! * `shard/<zone>`  = branch heads, tags, locks and the applied-transaction ids of that zone, plus
//!   at most one pending transaction record (prepared, not yet finalized).
//! * `shard/~root`   = the zone registry (zone -> path prefix), labels, snapshots, lock token counter,
//!   validator time high-water mark (everything that is global), plus its own applied ids. Zone names
//!   cannot contain `~`, so the root never collides with a zone (and a name without a leading dot is listed by FsBackend, which skips dotfiles).
//! * `decision/<tx>` = create-only `C` or `A` (committed list of participants after `C\n`).
//!
//! [`ShardedBackend`] makes this invisible to every consumer: it wraps a [`Backend`] and, once the
//! `manifest` key holds the moved marker, answers `get("manifest")` by composing a double-collect snapshot
//! of all participants into an ordinary [`Manifest`], and turns a compare-and-swap of `manifest` into
//! a transaction on exactly the participants whose keys changed (per-key preconditions taken from the
//! manifest the writer loaded). The `Publisher`, fetch, clone, tags, locks, the server's group commit and
//! remote GC therefore run unchanged. A transaction on ONE participant is a single CAS (no decision
//! record); a transaction on several runs the two-phase protocol of the model.
//!
//! Rules that matter (each is a mutation test in the models): prepare in ascending participant order;
//! decide only after all are prepared; foreign pending records are resolved from the decision record,
//! undecided ones aborted (a lower txid is waited for a bounded time first: wound-wait); lock state of
//! a touched zone is a precondition even when the transaction does not change it (a guarded push
//! serialises with a steal); a committed decision record is deleted only after every participant
//! finalized it and recorded the txid in its applied list (a re-run is then a no-op, also after an ABA
//! revert); aborted decision records are deleted only when older than a grace (a zombie writer is
//! bounded by its retry budget); GC roots include pending records and take object ages when the root
//! read starts.

use crate::backend::{Backend, BackendError, Conditional, LocalFlock, SharedBytes, Versioned};
use crate::manifest::{Lock, Manifest, KEY};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::names::{validate_branch_name, validate_path, validate_zone_name};
use vcs_objects::ObjectId;

pub const SHARD_PREFIX: &str = "shard/";
pub const ROOT_NAME: &str = "~root";
pub const DECISION_PREFIX: &str = "decision/";
/// Per-participant applied-id window (publishes touching THIS participant).
pub const APPLIED_CAP: usize = 256;
/// Ids per participant included in a composed manifest's `applied` (Publisher's AlreadyApplied probe).
const COMPOSED_PER_PART: usize = 16;
const TAG_PREFIX: &str = "sh-";
const MARK: &[u8; 4] = b"AGMF";
const MARK_VERSION: u8 = 9;

pub fn part_key(name: &str) -> String {
    format!("{SHARD_PREFIX}{name}")
}
pub fn decision_key(txid: &ObjectId) -> String {
    format!("{DECISION_PREFIX}{}", txid.to_hex())
}

#[derive(Debug, thiserror::Error)]
pub enum ShardError {
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error("malformed sharded record: {0}")]
    Malformed(String),
    #[error("sharded manifest: {0}")]
    Other(String),
}

type Op<T> = Vec<(String, Option<T>, Option<T>)>;

/// Everything one participant stores. The root-only fields are empty in zone records.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartState {
    pub heads: BTreeMap<String, ObjectId>,
    pub tags: BTreeMap<String, ObjectId>,
    pub locks: BTreeMap<String, Lock>,
    pub applied: Vec<ObjectId>,
    pub zones: BTreeMap<String, String>,
    pub labels: BTreeMap<String, String>,
    pub snapshots: BTreeMap<String, ObjectId>,
    pub next_token: u64,
    pub time_hw: u64,
}

/// The key-level changes of one transaction on one participant, with their old values as
/// preconditions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartDelta {
    pub heads: Op<ObjectId>,
    pub tags: Op<ObjectId>,
    pub locks: Op<Lock>,
    pub zones: Op<String>,
    pub labels: Op<String>,
    pub snapshots: Op<ObjectId>,
    /// ((next_token, time_hw) before, after)
    pub tokens: Option<((u64, u64), (u64, u64))>,
    /// Digest of the whole lock map (zone) / zone registry (root) the transaction validated against.
    pub guard: Option<[u8; 32]>,
    /// Further transaction ids merged into this commit (a server batch); recorded as applied before the txid.
    pub ids: Vec<ObjectId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Part {
    pub generation: u64,
    pub state: PartState,
    pub pending: Option<(ObjectId, PartDelta)>,
}

fn check_map<V: PartialEq>(m: &BTreeMap<String, V>, ops: &Op<V>) -> bool {
    ops.iter().all(|(k, old, _)| m.get(k) == old.as_ref())
}
fn apply_map<V: Clone>(m: &mut BTreeMap<String, V>, ops: &Op<V>) {
    for (k, _, new) in ops {
        match new {
            Some(v) => {
                m.insert(k.clone(), v.clone());
            }
            None => {
                m.remove(k);
            }
        }
    }
}

fn digest_locks(m: &BTreeMap<String, Lock>) -> [u8; 32] {
    let mut w = Writer::new();
    for (p, l) in m {
        w.str(p);
        put_lock(&mut w, l);
    }
    *blake3::hash(&w.finish()).as_bytes()
}
fn digest_zones(m: &BTreeMap<String, String>) -> [u8; 32] {
    let mut w = Writer::new();
    for (z, p) in m {
        w.str(z).str(p);
    }
    *blake3::hash(&w.finish()).as_bytes()
}

impl PartDelta {
    pub fn is_empty(&self) -> bool {
        self.heads.is_empty()
            && self.tags.is_empty()
            && self.locks.is_empty()
            && self.zones.is_empty()
            && self.labels.is_empty()
            && self.snapshots.is_empty()
            && self.tokens.is_none()
    }

    /// Do the preconditions hold on `s`?
    pub fn holds(&self, s: &PartState, root: bool) -> bool {
        check_map(&s.heads, &self.heads)
            && check_map(&s.tags, &self.tags)
            && check_map(&s.locks, &self.locks)
            && check_map(&s.zones, &self.zones)
            && check_map(&s.labels, &self.labels)
            && check_map(&s.snapshots, &self.snapshots)
            && self.tokens.is_none_or(|(old, _)| old == (s.next_token, s.time_hw))
            && self
                .guard
                .is_none_or(|g| g == if root { digest_zones(&s.zones) } else { digest_locks(&s.locks) })
    }

    fn apply(&self, s: &mut PartState) {
        apply_map(&mut s.heads, &self.heads);
        apply_map(&mut s.tags, &self.tags);
        apply_map(&mut s.locks, &self.locks);
        apply_map(&mut s.zones, &self.zones);
        apply_map(&mut s.labels, &self.labels);
        apply_map(&mut s.snapshots, &self.snapshots);
        if let Some((_, (t, h))) = self.tokens {
            s.next_token = t;
            s.time_hw = h;
        }
    }
}

// ---- codec ----

fn put_lock(w: &mut Writer, l: &Lock) {
    w.str(&l.holder)
        .varint(l.token)
        .varint(l.acquired_at)
        .varint(l.expires_at)
        .str(&l.reason);
}
fn get_lock(r: &mut Reader) -> Result<Lock, String> {
    let e = |x: vcs_objects::DecodeError| x.to_string();
    Ok(Lock {
        holder: r.str(512).map_err(e)?,
        token: r.varint().map_err(e)?,
        acquired_at: r.varint().map_err(e)?,
        expires_at: r.varint().map_err(e)?,
        reason: r.str(1024).map_err(e)?,
    })
}
fn put_opt<T>(w: &mut Writer, o: &Option<T>, f: impl Fn(&mut Writer, &T)) {
    match o {
        Some(v) => {
            w.u8(1);
            f(w, v);
        }
        None => {
            w.u8(0);
        }
    }
}
fn get_opt<T>(r: &mut Reader, f: impl Fn(&mut Reader) -> Result<T, String>) -> Result<Option<T>, String> {
    match r.u8().map_err(|e| e.to_string())? {
        0 => Ok(None),
        1 => Ok(Some(f(r)?)),
        _ => Err("bad option tag".into()),
    }
}
fn put_map<V>(w: &mut Writer, m: &BTreeMap<String, V>, f: impl Fn(&mut Writer, &V)) {
    w.varint(m.len() as u64);
    for (k, v) in m {
        w.str(k);
        f(w, v);
    }
}
fn get_map<V>(
    r: &mut Reader,
    min: usize,
    key_ok: impl Fn(&str) -> Result<(), String>,
    f: impl Fn(&mut Reader) -> Result<V, String>,
) -> Result<BTreeMap<String, V>, String> {
    let e = |x: vcs_objects::DecodeError| x.to_string();
    let mut m = BTreeMap::new();
    for _ in 0..r.count(min).map_err(e)? {
        let k = r.str(4096).map_err(e)?;
        key_ok(&k)?;
        if m.last_key_value().is_some_and(|(p, _): (&String, &V)| *p >= k) {
            return Err("keys not strictly ascending".into());
        }
        m.insert(k, f(r)?);
    }
    Ok(m)
}
fn put_ops<V>(w: &mut Writer, ops: &Op<V>, f: impl Fn(&mut Writer, &V)) {
    w.varint(ops.len() as u64);
    for (k, a, b) in ops {
        w.str(k);
        put_opt(w, a, &f);
        put_opt(w, b, &f);
    }
}
fn get_ops<V>(
    r: &mut Reader,
    key_ok: impl Fn(&str) -> Result<(), String>,
    f: impl Fn(&mut Reader) -> Result<V, String>,
) -> Result<Op<V>, String> {
    let e = |x: vcs_objects::DecodeError| x.to_string();
    let mut v: Op<V> = Vec::new();
    for _ in 0..r.count(3).map_err(e)? {
        let k = r.str(4096).map_err(e)?;
        key_ok(&k)?;
        if v.last().is_some_and(|(p, _, _)| *p >= k) {
            return Err("ops not strictly ascending".into());
        }
        let a = get_opt(r, &f)?;
        let b = get_opt(r, &f)?;
        v.push((k, a, b));
    }
    Ok(v)
}
fn put_id(w: &mut Writer, i: &ObjectId) {
    w.id(i);
}
fn get_id(r: &mut Reader) -> Result<ObjectId, String> {
    r.id().map_err(|e| e.to_string())
}
#[allow(clippy::ptr_arg)]
fn put_s(w: &mut Writer, s: &String) {
    w.str(s);
}
fn get_s(r: &mut Reader) -> Result<String, String> {
    r.str(4096).map_err(|e| e.to_string())
}
fn ok_branch(s: &str) -> Result<(), String> {
    validate_branch_name(s).map_err(|e| e.to_string())
}
fn ok_zone(s: &str) -> Result<(), String> {
    validate_zone_name(s).map_err(|e| e.to_string())
}
fn ok_path(s: &str) -> Result<(), String> {
    validate_path(s).map_err(|e| e.to_string())
}

impl PartState {
    fn encode_into(&self, w: &mut Writer) {
        put_map(w, &self.heads, put_id);
        put_map(w, &self.tags, put_id);
        put_map(w, &self.locks, put_lock);
        w.varint(self.applied.len() as u64);
        for a in &self.applied {
            w.id(a);
        }
        put_map(w, &self.zones, put_s);
        put_map(w, &self.labels, put_s);
        put_map(w, &self.snapshots, put_id);
        w.varint(self.next_token).varint(self.time_hw);
    }
    fn decode_from(r: &mut Reader) -> Result<Self, String> {
        let e = |x: vcs_objects::DecodeError| x.to_string();
        let mut s = PartState {
            heads: get_map(r, 33, ok_branch, get_id)?,
            tags: get_map(r, 33, ok_branch, get_id)?,
            locks: get_map(r, 7, ok_path, get_lock)?,
            ..Default::default()
        };
        for _ in 0..r.count(32).map_err(e)? {
            s.applied.push(r.id().map_err(e)?);
        }
        s.zones = get_map(r, 2, ok_zone, get_s)?;
        s.labels = get_map(r, 2, ok_zone, get_s)?;
        s.snapshots = get_map(r, 33, ok_branch, get_id)?;
        for p in s.zones.values() {
            ok_path(p)?;
        }
        for p in s.labels.values() {
            ok_zone(p)?;
        }
        s.next_token = r.varint().map_err(e)?;
        s.time_hw = r.varint().map_err(e)?;
        Ok(s)
    }
}

impl PartDelta {
    fn encode_into(&self, w: &mut Writer) {
        put_ops(w, &self.heads, put_id);
        put_ops(w, &self.tags, put_id);
        put_ops(w, &self.locks, put_lock);
        put_ops(w, &self.zones, put_s);
        put_ops(w, &self.labels, put_s);
        put_ops(w, &self.snapshots, put_id);
        put_opt(w, &self.tokens, |w, ((a, b), (c, d))| {
            w.varint(*a).varint(*b).varint(*c).varint(*d);
        });
        put_opt(w, &self.guard, |w, g| {
            w.raw(g);
        });
        w.varint(self.ids.len() as u64);
        for i in &self.ids {
            w.id(i);
        }
    }
    fn decode_from(r: &mut Reader) -> Result<Self, String> {
        let e = |x: vcs_objects::DecodeError| x.to_string();
        Ok(PartDelta {
            heads: get_ops(r, ok_branch, get_id)?,
            tags: get_ops(r, ok_branch, get_id)?,
            locks: get_ops(r, ok_path, get_lock)?,
            zones: get_ops(r, ok_zone, get_s)?,
            labels: get_ops(r, ok_zone, get_s)?,
            snapshots: get_ops(r, ok_branch, get_id)?,
            tokens: get_opt(r, |r| {
                Ok((
                    (r.varint().map_err(e)?, r.varint().map_err(e)?),
                    (r.varint().map_err(e)?, r.varint().map_err(e)?),
                ))
            })?,
            guard: get_opt(r, |r| {
                let mut g = [0u8; 32];
                g.copy_from_slice(r.raw(32).map_err(e)?);
                Ok(g)
            })?,
            ids: {
                let mut v = Vec::new();
                for _ in 0..r.count(32).map_err(e)? {
                    v.push(r.id().map_err(e)?);
                }
                v
            },
        })
    }
}

impl Part {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.raw(b"AGSZ").u8(1).varint(self.generation);
        self.state.encode_into(&mut w);
        put_opt(&mut w, &self.pending, |w, (t, d)| {
            w.id(t);
            d.encode_into(w);
        });
        w.finish()
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, ShardError> {
        let m = ShardError::Malformed;
        let mut r = Reader::new(bytes);
        if r.raw(4).map_err(|e| m(e.to_string()))? != b"AGSZ" || r.u8().map_err(|e| m(e.to_string()))? != 1 {
            return Err(m("bad magic or version".into()));
        }
        let generation = r.varint().map_err(|e| m(e.to_string()))?;
        let state = PartState::decode_from(&mut r).map_err(m)?;
        let pending = get_opt(&mut r, |r| {
            let t = r.id().map_err(|e| e.to_string())?;
            Ok((t, PartDelta::decode_from(r)?))
        })
        .map_err(m)?;
        r.finish().map_err(|e| m(e.to_string()))?;
        Ok(Part {
            generation,
            state,
            pending,
        })
    }
}

// ---- the transaction engine ----

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum CommitOutcome {
    Committed,
    /// A precondition did not hold (the caller reloads and revalidates), or the transaction lost a
    /// conflict and was aborted.
    Conflict,
}

/// Counters for measurements.
#[derive(Default, Debug, Clone, Copy)]
pub struct EngineStats {
    pub cas_attempts: u64,
    pub cas_lost: u64,
    pub single_part: u64,
    pub multi_part: u64,
}

pub struct Engine<'a> {
    b: &'a dyn Backend,
    /// How long to wait for a lower-txid pending record before aborting it.
    pub wait: Duration,
    pub stats: Mutex<EngineStats>,
}

fn decode_decision(data: &[u8]) -> Result<(bool, Vec<String>), ShardError> {
    let s = std::str::from_utf8(data).map_err(|_| ShardError::Malformed("decision record".into()))?;
    let (head, rest) = s.split_once('\n').unwrap_or((s, ""));
    let parts = rest.split('\n').filter(|p| !p.is_empty()).map(str::to_owned).collect();
    match head {
        "C" => Ok((true, parts)),
        "A" => Ok((false, parts)),
        _ => Err(ShardError::Malformed("decision record".into())),
    }
}

impl<'a> Engine<'a> {
    pub fn new(b: &'a dyn Backend) -> Self {
        Self {
            b,
            wait: Duration::from_millis(250),
            stats: Mutex::new(EngineStats::default()),
        }
    }

    fn count(&self, f: impl FnOnce(&mut EngineStats)) {
        f(&mut self.stats.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn read(&self, name: &str) -> Result<(Part, Option<String>), ShardError> {
        match self.b.get(&part_key(name))? {
            None => Ok((Part::default(), None)),
            Some(v) => Ok((Part::decode(&v.data)?, Some(v.tag))),
        }
    }

    fn cas(&self, name: &str, p: &Part, tag: &Option<String>) -> Result<bool, ShardError> {
        let key = part_key(name);
        let ok = match tag {
            None => self.b.put_if_absent(&key, &p.encode())?,
            Some(t) => self.b.put_if_match(&key, &p.encode(), t)?.is_some(),
        };
        self.count(|s| {
            s.cas_attempts += 1;
            if !ok {
                s.cas_lost += 1;
            }
        });
        Ok(ok)
    }

    /// `Some((committed, participants))` once decided.
    pub fn decision(&self, txid: &ObjectId) -> Result<Option<(bool, Vec<String>)>, ShardError> {
        match self.b.get(&decision_key(txid))? {
            None => Ok(None),
            Some(v) => Ok(Some(decode_decision(&v.data)?)),
        }
    }

    fn decide(&self, txid: &ObjectId, commit: bool, parts: &[String]) -> Result<bool, ShardError> {
        let body = if commit {
            format!("C\n{}", parts.join("\n"))
        } else {
            "A".to_owned()
        };
        self.b.put_if_absent(&decision_key(txid), body.as_bytes())?;
        self.decision(txid)?
            .map(|d| d.0)
            .ok_or_else(|| ShardError::Malformed("decision vanished".into()))
    }

    fn finalize_state(p: &mut Part, name: &str) {
        let (t, d) = p.pending.take().expect("pending");
        d.apply(&mut p.state);
        Self::record(&mut p.state, &d, t);
        p.generation += 1;
        let _ = name;
    }

    fn record(s: &mut PartState, d: &PartDelta, t: ObjectId) {
        for i in &d.ids {
            Self::push_applied(s, *i);
        }
        Self::push_applied(s, t);
    }

    fn push_applied(s: &mut PartState, t: ObjectId) {
        s.applied.push(t);
        if s.applied.len() > APPLIED_CAP {
            s.applied.remove(0);
        }
    }

    /// Resolve a foreign pending record whose fate is `commit`.
    fn resolve(&self, name: &str, mut p: Part, tag: &Option<String>, commit: bool) -> Result<(), ShardError> {
        if commit {
            Self::finalize_state(&mut p, name);
        } else {
            p.pending = None;
            p.generation += 1;
        }
        self.cas(name, &p, tag)?; // lost race: somebody else resolved it; the caller re-reads
        Ok(())
    }

    /// Settle the foreign pending record `t` on `name`: apply or clear it per its decision; an
    /// undecided one is waited for if `t < txid` (wound-wait) for a bounded time, then aborted.
    fn settle_foreign(&self, name: &str, p: Part, tag: &Option<String>, started: Instant, txid: &ObjectId) -> Result<(), ShardError> {
        let t = p.pending.as_ref().expect("pending").0;
        match self.decision(&t)? {
            Some((c, _)) => self.resolve(name, p, tag, c),
            None if t < *txid && started.elapsed() < self.wait => {
                std::thread::sleep(Duration::from_micros(500));
                Ok(())
            }
            None => {
                let c = self.decide(&t, false, &[])?;
                self.resolve(name, p, tag, c)
            }
        }
    }

    /// Run one transaction to completion. Idempotent in `txid`.
    pub fn commit(&self, txid: ObjectId, parts: &BTreeMap<String, PartDelta>) -> Result<CommitOutcome, ShardError> {
        match parts.len() {
            0 => Ok(CommitOutcome::Committed),
            1 => {
                self.count(|s| s.single_part += 1);
                let (name, d) = parts.iter().next().unwrap();
                self.commit_single(txid, name, d)
            }
            _ => {
                self.count(|s| s.multi_part += 1);
                self.commit_multi(txid, parts)
            }
        }
    }

    fn commit_single(&self, txid: ObjectId, name: &str, d: &PartDelta) -> Result<CommitOutcome, ShardError> {
        let started = Instant::now();
        loop {
            let (mut p, tag) = self.read(name)?;
            if p.pending.is_some() {
                self.settle_foreign(name, p, &tag, started, &txid)?;
                continue;
            }
            if p.state.applied.contains(&txid) {
                return Ok(CommitOutcome::Committed);
            }
            if !d.holds(&p.state, name == ROOT_NAME) {
                return Ok(CommitOutcome::Conflict);
            }
            d.apply(&mut p.state);
            Self::record(&mut p.state, d, txid);
            p.generation += 1;
            if self.cas(name, &p, &tag)? {
                return Ok(CommitOutcome::Committed);
            }
        }
    }

    fn commit_multi(&self, txid: ObjectId, parts: &BTreeMap<String, PartDelta>) -> Result<CommitOutcome, ShardError> {
        let names: Vec<String> = parts.keys().cloned().collect();
        match self.decision(&txid)? {
            Some((true, _)) => return self.finalize(txid, &names, true),
            Some((false, _)) => return self.finalize(txid, &names, false),
            None => {
                // decision garbage-collected? then every participant remembers the txid
                let (p0, _) = self.read(&names[0])?;
                if p0.state.applied.contains(&txid) && p0.pending.is_none() {
                    let mut all = true;
                    for n in &names[1..] {
                        let (p, _) = self.read(n)?;
                        all &= p.state.applied.contains(&txid) && p.pending.is_none();
                    }
                    if all {
                        return Ok(CommitOutcome::Committed);
                    }
                }
            }
        }
        // Phase 1: prepare in ascending order
        for (name, d) in parts {
            let started = Instant::now();
            loop {
                let (mut p, tag) = self.read(name)?;
                match &p.pending {
                    Some((t, _)) if *t == txid => break,
                    Some(_) => {
                        self.settle_foreign(name, p, &tag, started, &txid)?;
                        continue;
                    }
                    None => {}
                }
                if p.state.applied.contains(&txid) {
                    break; // already finalized here (re-run)
                }
                if !d.holds(&p.state, name == ROOT_NAME) {
                    self.decide(&txid, false, &[])?;
                    self.finalize(txid, &names, false)?;
                    return Ok(CommitOutcome::Conflict);
                }
                p.pending = Some((txid, d.clone()));
                p.generation += 1;
                if self.cas(name, &p, &tag)? {
                    break;
                }
            }
        }
        // Phase 2: decide (the linearization point), then finalize
        if self.decide(&txid, true, &names)? {
            self.finalize(txid, &names, true)
        } else {
            self.finalize(txid, &names, false)
        }
    }

    fn finalize(&self, txid: ObjectId, names: &[String], commit: bool) -> Result<CommitOutcome, ShardError> {
        for name in names {
            loop {
                let (p, tag) = self.read(name)?;
                match &p.pending {
                    Some((t, _)) if *t == txid => self.resolve(name, p, &tag, commit)?,
                    _ => break,
                }
            }
        }
        Ok(if commit {
            CommitOutcome::Committed
        } else {
            CommitOutcome::Conflict
        })
    }

    /// Names of all participants (the root first).
    pub fn names(&self) -> Result<Vec<String>, ShardError> {
        let mut v: Vec<String> = self
            .b
            .list(SHARD_PREFIX)?
            .into_iter()
            .filter_map(|k| k.strip_prefix(SHARD_PREFIX).map(str::to_owned))
            .collect();
        v.sort();
        Ok(v)
    }

    /// Effective state of a participant: the pending record counts as applied iff Committed.
    fn effective(&self, p: &Part) -> Result<(PartState, bool), ShardError> {
        match &p.pending {
            Some((t, d)) => match self.decision(t)? {
                Some((true, _)) => {
                    let mut s = p.state.clone();
                    d.apply(&mut s);
                    Self::record(&mut s, d, *t);
                    Ok((s, true))
                }
                _ => Ok((p.state.clone(), false)),
            },
            None => Ok((p.state.clone(), false)),
        }
    }

    /// Linearizable snapshot of every participant (double collect), composed into a [`Manifest`]
    /// plus its tag. The validation pass uses conditional reads (304, no body) and re-checks the
    /// decision of any pending record, so a quiet repository costs one body read per participant.
    pub fn snapshot(&self) -> Result<(Manifest, String), ShardError> {
        type Row = (String, PartState, (String, bool), Option<Part>);
        let collect = |names: &[String]| -> Result<Vec<Row>, ShardError> {
            crate::par::map_bounded(names, crate::par::parallelism(), |n| match self.b.get(&part_key(n))? {
                None => Ok((n.clone(), PartState::default(), (String::new(), false), None)),
                Some(v) => {
                    let p = Part::decode(&v.data)?;
                    let (s, dec) = self.effective(&p)?;
                    if dec {
                        // help: a committed transaction was left unfinalized (its writer died or is slow)
                        let _ = self.resolve(n, p.clone(), &Some(v.tag.clone()), true);
                    }
                    Ok((n.clone(), s, (v.tag, dec), Some(p)))
                }
            })
        };
        let mut names = self.names()?;
        let mut prev = collect(&names)?;
        loop {
            let names2 = self.names()?;
            let mut same = names2 == names;
            if same {
                let flags = crate::par::map_bounded(&prev, crate::par::parallelism(), |(n, _, (tag, dec), p)| {
                    let Some(p) = p else { return Ok::<bool, ShardError>(false) };
                    match self.b.get_conditional(&part_key(n), tag)? {
                        Conditional::NotModified => match &p.pending {
                            Some((t, _)) => Ok(matches!(self.decision(t)?, Some((true, _))) == *dec),
                            None => Ok(true),
                        },
                        _ => Ok(false),
                    }
                })?;
                same = flags.iter().all(|f| *f);
            }
            if same {
                break;
            }
            names = names2;
            prev = collect(&names)?;
        }
        Ok(compose(
            &prev
                .iter()
                .map(|(n, s, t, _)| (n.clone(), s.clone(), t.clone()))
                .collect::<Vec<_>>(),
        ))
    }
}

/// Compose participants (root first) into an ordinary manifest and its tag.
fn compose(parts: &[(String, PartState, (String, bool))]) -> (Manifest, String) {
    let mut m = Manifest::default();
    let mut h = blake3::Hasher::new();
    let mut gen = 0u64;
    for (name, s, (tag, dec)) in parts {
        h.update(name.as_bytes())
            .update(&[0])
            .update(tag.as_bytes())
            .update(&[*dec as u8, 0]);
        gen = gen.wrapping_add(tag.bytes().fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64)));
        if name == ROOT_NAME {
            m.zones = s.zones.clone();
            m.labels = s.labels.clone();
            m.snapshots = s.snapshots.clone();
            m.next_token = s.next_token;
            m.time_hw = s.time_hw;
        } else {
            for (b, id) in &s.heads {
                m.branches.insert((name.clone(), b.clone()), *id);
            }
            for (t, id) in &s.tags {
                m.tags.insert((name.clone(), t.clone()), *id);
            }
            for (p, l) in &s.locks {
                m.locks.insert((name.clone(), p.clone()), l.clone());
            }
        }
        let n = s.applied.len();
        for a in &s.applied[n.saturating_sub(COMPOSED_PER_PART)..] {
            if !m.applied.contains(a) {
                m.applied.push(*a);
            }
        }
    }
    m.applied.truncate(crate::manifest::MAX_APPLIED);
    m.generation = gen & 0xFFFF_FFFF;
    let _ = h;
    let tag = content_tag(&m);
    (m, tag)
}

/// Version tag of a composed manifest: a hash of its content except `applied` and `generation`, so
/// the tag of the manifest a writer just committed can be computed without reading anything back.
pub fn content_tag(m: &Manifest) -> String {
    let mut w = Writer::new();
    for ((z, b), id) in &m.branches {
        w.str(z).str(b).id(id);
    }
    w.u8(0xff);
    for ((z, t), id) in &m.tags {
        w.str(z).str(t).id(id);
    }
    w.u8(0xff);
    for ((z, p), l) in &m.locks {
        w.str(z).str(p);
        put_lock(&mut w, l);
    }
    w.u8(0xff);
    for (z, p) in &m.zones {
        w.str(z).str(p);
    }
    w.u8(0xff);
    for (z, p) in &m.labels {
        w.str(z).str(p);
    }
    w.u8(0xff);
    for (n, id) in &m.snapshots {
        w.str(n).id(id);
    }
    w.varint(m.next_token).varint(m.time_hw);
    format!("{TAG_PREFIX}{}", blake3::hash(&w.finish()).to_hex())
}

/// The per-participant deltas that turn `old` into `new`.
pub fn diff(old: &Manifest, new: &Manifest) -> BTreeMap<String, PartDelta> {
    let mut out: BTreeMap<String, PartDelta> = BTreeMap::new();
    fn ops<K: Ord + Clone, V: Clone + PartialEq>(a: &BTreeMap<K, V>, b: &BTreeMap<K, V>, mut sink: impl FnMut(&K, Option<V>, Option<V>)) {
        for (k, v) in a {
            match b.get(k) {
                Some(w) if w == v => {}
                w => sink(k, Some(v.clone()), w.cloned()),
            }
        }
        for (k, w) in b {
            if !a.contains_key(k) {
                sink(k, None, Some(w.clone()));
            }
        }
    }
    fn part<'o>(out: &'o mut BTreeMap<String, PartDelta>, old: &Manifest, z: &str) -> &'o mut PartDelta {
        out.entry(z.to_owned()).or_insert_with(|| PartDelta {
            guard: Some(digest_locks(
                &old.locks
                    .iter()
                    .filter(|((lz, _), _)| lz == z)
                    .map(|((_, p), l)| (p.clone(), l.clone()))
                    .collect(),
            )),
            ..Default::default()
        })
    }
    ops(&old.branches, &new.branches, |(z, b), o, n| {
        part(&mut out, old, z).heads.push((b.clone(), o, n))
    });
    ops(&old.tags, &new.tags, |(z, t), o, n| {
        part(&mut out, old, z).tags.push((t.clone(), o, n))
    });
    ops(&old.locks, &new.locks, |(z, p), o, n| {
        part(&mut out, old, z).locks.push((p.clone(), o, n))
    });
    let root_guard = digest_zones(&old.zones);
    let mut root = PartDelta::default();
    ops(&old.zones, &new.zones, |z, o, n| root.zones.push((z.clone(), o, n)));
    ops(&old.labels, &new.labels, |z, o, n| root.labels.push((z.clone(), o, n)));
    ops(&old.snapshots, &new.snapshots, |s, o, n| root.snapshots.push((s.clone(), o, n)));
    if (old.next_token, old.time_hw) != (new.next_token, new.time_hw) {
        root.tokens = Some(((old.next_token, old.time_hw), (new.next_token, new.time_hw)));
    }
    if !root.zones.is_empty() {
        root.guard = Some(root_guard);
    }
    if !root.is_empty() {
        out.insert(ROOT_NAME.to_owned(), root);
    }
    // every transaction id this change adds (a server batch merges several); the last one is the txid
    let fresh: Vec<ObjectId> = new.applied.iter().filter(|a| !old.applied.contains(a)).copied().collect();
    if fresh.len() > 1 {
        for d in out.values_mut() {
            d.ids = fresh[..fresh.len() - 1].to_vec();
        }
    }
    out
}

/// The txid under which a manifest change is committed: the id newly present in `applied`
/// (the Publisher's txid), else a hash of the change.
fn txid_of(old: &Manifest, new: &Manifest, parts: &BTreeMap<String, PartDelta>) -> ObjectId {
    if let Some(last) = new.applied.last() {
        if !old.applied.contains(last) {
            return *last;
        }
    }
    let mut w = Writer::new();
    for (n, d) in parts {
        w.str(n);
        d.encode_into(&mut w);
    }
    ObjectId::from_bytes(*blake3::hash(&w.finish()).as_bytes())
}

// ---- marker and migration ----

enum Marker {
    Frozen(Vec<u8>),
    Moved,
}
fn marker(data: &[u8]) -> Option<Marker> {
    if data.len() >= 6 && &data[..4] == MARK && data[4] == MARK_VERSION {
        match data[5] {
            1 => Some(Marker::Frozen(data[6..].to_vec())),
            2 => Some(Marker::Moved),
            _ => None,
        }
    } else {
        None
    }
}
fn marker_bytes(frozen: Option<&[u8]>) -> Vec<u8> {
    let mut v = MARK.to_vec();
    v.push(MARK_VERSION);
    match frozen {
        Some(m) => {
            v.push(1);
            v.extend_from_slice(m);
        }
        None => v.push(2),
    }
    v
}

/// True once the repository's manifest key holds the moved marker.
pub fn is_sharded(b: &dyn Backend) -> Result<bool, ShardError> {
    Ok(b.get(KEY)?.is_some_and(|v| matches!(marker(&v.data), Some(Marker::Moved))))
}

#[derive(Debug, Default)]
pub struct MigrateReport {
    pub already: bool,
    pub zones: usize,
}

/// Move a single-manifest repository to sharded manifests. Safe to re-run after a crash at any
/// step (model: `model_sharded_migrate.rs`) and safe against concurrent publishers: they either
/// lose their CAS against the freeze or are held off until the moved marker is in place.
///
/// Steps: 1. freeze (CAS `manifest` to a marker that embeds the manifest bytes: every writer that
/// loaded earlier loses its CAS, every writer that loads later sees a retryable "migrating"
/// error); 2. create-only one record per zone; 3. create-only the root record; 4. CAS the marker
/// to "moved".
pub fn migrate_to_sharded(b: &dyn Backend) -> Result<MigrateReport, ShardError> {
    for _ in 0..64 {
        let Some(cur) = b.get(KEY)? else {
            return Err(ShardError::Other("repository has no manifest to migrate".into()));
        };
        let (embedded, frozen_tag) = match marker(&cur.data) {
            Some(Marker::Moved) => return Ok(MigrateReport { already: true, zones: 0 }),
            Some(Marker::Frozen(m)) => (m, cur.tag.clone()),
            None => {
                Manifest::decode(&cur.data).map_err(|e| ShardError::Other(e.to_string()))?;
                match b.put_if_match(KEY, &marker_bytes(Some(&cur.data)), &cur.tag)? {
                    Some(t) => (cur.data.clone(), t),
                    None => continue, // somebody published first: start over with the new manifest
                }
            }
        };
        let m = Manifest::decode(&embedded).map_err(|e| ShardError::Other(e.to_string()))?;
        let mut parts: BTreeMap<String, PartState> = BTreeMap::new();
        for ((z, br), id) in &m.branches {
            parts.entry(z.clone()).or_default().heads.insert(br.clone(), *id);
        }
        for ((z, t), id) in &m.tags {
            parts.entry(z.clone()).or_default().tags.insert(t.clone(), *id);
        }
        for ((z, p), l) in &m.locks {
            parts.entry(z.clone()).or_default().locks.insert(p.clone(), l.clone());
        }
        // applied ids of the single manifest cannot be attributed to zones: keep them all in the root
        parts.insert(
            ROOT_NAME.to_owned(),
            PartState {
                zones: m.zones.clone(),
                labels: m.labels.clone(),
                snapshots: m.snapshots.clone(),
                next_token: m.next_token,
                time_hw: m.time_hw,
                applied: m.applied.iter().rev().take(APPLIED_CAP).rev().copied().collect(),
                ..Default::default()
            },
        );
        let n = parts.len() - 1;
        for (name, state) in parts {
            let p = Part {
                generation: 1,
                state,
                pending: None,
            };
            b.put_if_absent(&part_key(&name), &p.encode())?; // already there: a previous run made it
        }
        match b.put_if_match(KEY, &marker_bytes(None), &frozen_tag)? {
            Some(_) => return Ok(MigrateReport { already: false, zones: n }),
            None => continue, // another migrator moved it, or the marker changed: re-read
        }
    }
    Err(ShardError::Other("migration did not settle".into()))
}

// ---- garbage collection support ----

/// Objects that pending (prepared or committed, not yet finalized) records make reachable. Remote GC
/// adds them to its roots (model: `model_sharded_gc.rs`, `pending_are_roots`).
pub fn pending_roots(b: &dyn Backend) -> Result<Vec<ObjectId>, ShardError> {
    let mut out = Vec::new();
    for k in b.list(SHARD_PREFIX)? {
        let Some(v) = b.get(&k)? else { continue };
        let p = Part::decode(&v.data)?;
        if let Some((_, d)) = &p.pending {
            out.extend(d.heads.iter().filter_map(|(_, _, n)| *n));
            out.extend(d.tags.iter().filter_map(|(_, _, n)| *n));
            out.extend(d.snapshots.iter().filter_map(|(_, _, n)| *n));
        }
    }
    Ok(out)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DecisionGcReport {
    pub committed_deleted: usize,
    pub aborted_deleted: usize,
    pub kept: usize,
}

/// Delete decision records that can no longer matter. A Committed record goes only once every
/// participant it lists has finalized it (no pending record of the txid, txid in its applied list);
/// an Aborted record only when it is older than `abort_grace_ms` on the backend clock and no
/// participant still holds a pending record of it.
pub fn gc_decisions(b: &dyn Backend, abort_grace_ms: u64) -> Result<DecisionGcReport, ShardError> {
    let eng = Engine::new(b);
    let mut rep = DecisionGcReport::default();
    let pending: BTreeSet<ObjectId> = {
        let mut s = BTreeSet::new();
        for n in eng.names()? {
            let (p, _) = eng.read(&n)?;
            if let Some((t, _)) = p.pending {
                s.insert(t);
            }
        }
        s
    };
    let now = b.now_ms().ok();
    for k in b.list(DECISION_PREFIX)? {
        let Some(hex) = k.strip_prefix(DECISION_PREFIX) else { continue };
        let Ok(txid) = ObjectId::from_hex(hex) else { continue };
        let Some(v) = b.get(&k)? else { continue };
        let (committed, parts) = decode_decision(&v.data)?;
        if pending.contains(&txid) {
            rep.kept += 1;
            continue;
        }
        if committed {
            let mut done = !parts.is_empty();
            for n in &parts {
                let (p, _) = eng.read(n)?;
                done &= p.pending.is_none() && p.state.applied.contains(&txid);
            }
            if done {
                b.delete(&k)?;
                rep.committed_deleted += 1;
            } else {
                rep.kept += 1;
            }
        } else {
            let old = match (now, b.modified_ms(&k).ok().flatten()) {
                (Some(n), Some(m)) => n.saturating_sub(m) > abort_grace_ms,
                _ => false,
            };
            if old {
                b.delete(&k)?;
                rep.aborted_deleted += 1;
            } else {
                rep.kept += 1;
            }
        }
    }
    Ok(rep)
}

// ---- the adapter ----

/// Wraps a backend so that a sharded repository looks like a single-manifest one (module docs).
/// Costs nothing for single-manifest repositories beyond recognising the marker on a manifest read.
pub struct ShardedBackend {
    inner: Arc<dyn Backend>,
    cache: Mutex<VecDeque<(String, Arc<Manifest>)>>,
    pub wait: Duration,
    /// Reads of `manifest` are served from the last composed snapshot for this long (0 = never).
    /// Only for the sole-writer deployment (all writes go through this instance, which keeps the snapshot
    /// current): a write by another process is seen after at most this long. A lost CAS drops the snapshot.
    read_ttl: Duration,
    snap: Mutex<Option<(Instant, Versioned, Arc<Manifest>)>>,
}

/// Apply to `m` the difference between `old` and `new` (key by key).
fn apply_change(m: &mut Manifest, old: &Manifest, new: &Manifest) {
    fn sync<K: Ord + Clone, V: Clone + PartialEq>(m: &mut BTreeMap<K, V>, old: &BTreeMap<K, V>, new: &BTreeMap<K, V>) {
        for (k, v) in old {
            match new.get(k) {
                Some(w) if w == v => {}
                Some(w) => {
                    m.insert(k.clone(), w.clone());
                }
                None => {
                    m.remove(k);
                }
            }
        }
        for (k, w) in new {
            if !old.contains_key(k) {
                m.insert(k.clone(), w.clone());
            }
        }
    }
    sync(&mut m.branches, &old.branches, &new.branches);
    sync(&mut m.tags, &old.tags, &new.tags);
    sync(&mut m.locks, &old.locks, &new.locks);
    sync(&mut m.zones, &old.zones, &new.zones);
    sync(&mut m.labels, &old.labels, &new.labels);
    sync(&mut m.snapshots, &old.snapshots, &new.snapshots);
    if (old.next_token, old.time_hw) != (new.next_token, new.time_hw) {
        m.next_token = new.next_token;
        m.time_hw = new.time_hw;
    }
    for a in &new.applied {
        if !old.applied.contains(a) && !m.applied.contains(a) {
            m.applied.push(*a);
        }
    }
    let n = m.applied.len();
    if n > crate::manifest::MAX_APPLIED {
        m.applied.drain(..n - crate::manifest::MAX_APPLIED);
    }
    m.generation = m.generation.wrapping_add(1);
}

const CACHE_LEN: usize = 128;

impl ShardedBackend {
    pub fn new(inner: Box<dyn Backend>) -> Self {
        Self::from_arc(Arc::from(inner))
    }
    pub fn from_arc(inner: Arc<dyn Backend>) -> Self {
        Self {
            inner,
            cache: Mutex::new(VecDeque::new()),
            wait: Duration::from_millis(250),
            read_ttl: Duration::ZERO,
            snap: Mutex::new(None),
        }
    }
    /// Serve manifest reads from the last snapshot for up to `ttl` (see the field's documentation).
    pub fn with_read_ttl(mut self, ttl: Duration) -> Self {
        self.read_ttl = ttl;
        self
    }
    pub fn inner(&self) -> &dyn Backend {
        &*self.inner
    }

    fn engine(&self) -> Engine<'_> {
        let mut e = Engine::new(&*self.inner);
        e.wait = self.wait;
        e
    }

    fn remember(&self, tag: &str, m: &Manifest) {
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        c.retain(|(t, _)| t != tag);
        c.push_back((tag.to_owned(), Arc::new(m.clone())));
        if c.len() > CACHE_LEN {
            c.pop_front();
        }
    }
    fn recall(&self, tag: &str) -> Option<Arc<Manifest>> {
        let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        c.iter().find(|(t, _)| t == tag).map(|(_, m)| m.clone())
    }

    fn composed(&self) -> Result<Versioned, BackendError> {
        if !self.read_ttl.is_zero() {
            let g = self.snap.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((at, v, _)) = g.as_ref() {
                if at.elapsed() < self.read_ttl {
                    return Ok(v.clone());
                }
            }
        }
        let (m, tag) = self.engine().snapshot().map_err(|e| BackendError::Other(e.to_string()))?;
        let data = m.encode();
        self.remember(&tag, &m);
        let v = Versioned { data, tag };
        if !self.read_ttl.is_zero() {
            *self.snap.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), v.clone(), Arc::new(m)));
        }
        Ok(v)
    }

    fn manifest_get(&self) -> Result<Option<Versioned>, BackendError> {
        let Some(v) = self.inner.get(KEY)? else { return Ok(None) };
        match marker(&v.data) {
            None => Ok(Some(v)),
            Some(Marker::Frozen(_)) => Err(BackendError::Unavailable(
                "the repository is being migrated to sharded manifests; retry shortly".into(),
            )),
            Some(Marker::Moved) => self.composed().map(Some),
        }
    }

    fn manifest_cas(&self, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        let Some(old) = self.recall(tag) else {
            return Ok(None); // unknown base: lose the CAS, the caller reloads
        };
        let new = Manifest::decode(data).map_err(|e| BackendError::Other(e.to_string()))?;
        let parts = diff(&old, &new);
        let txid = txid_of(&old, &new, &parts);
        let eng = self.engine();
        match eng.commit(txid, &parts).map_err(|e| BackendError::Other(e.to_string()))? {
            CommitOutcome::Conflict => {
                *self.snap.lock().unwrap_or_else(|e| e.into_inner()) = None;
                Ok(None)
            }
            CommitOutcome::Committed => {
                // the tag of what we wrote, computed from its content: no read-back. With a live snapshot the
                // change is applied to it, so the next read costs nothing.
                let mut g = self.snap.lock().unwrap_or_else(|e| e.into_inner());
                let mut keep_at = Instant::now();
                let after = match g.take() {
                    Some((at, _, cur)) if at.elapsed() < self.read_ttl => {
                        keep_at = at; // the snapshot ages from when it was READ, however many writes updated it
                        let mut m2 = (*cur).clone();
                        apply_change(&mut m2, &old, &new);
                        m2
                    }
                    _ => new.clone(),
                };
                let t = content_tag(&after);
                self.remember(&t, &after);
                if !self.read_ttl.is_zero() {
                    *g = Some((
                        keep_at,
                        Versioned {
                            data: after.encode(),
                            tag: t.clone(),
                        },
                        Arc::new(after),
                    ));
                }
                Ok(Some(t))
            }
        }
    }
}

impl Backend for ShardedBackend {
    fn get_stream(&self, key: &str, min_len: u64) -> Result<Option<crate::ObjectStream>, BackendError> {
        if key == KEY {
            return Ok(None);
        }
        self.inner.get_stream(key, min_len)
    }
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        if key == KEY {
            self.manifest_get()
        } else {
            self.inner.get(key)
        }
    }
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        if key != KEY {
            return self.inner.get_conditional(key, have);
        }
        if !have.starts_with(TAG_PREFIX) {
            return match self.inner.get_conditional(key, have)? {
                Conditional::Modified(v) if marker(&v.data).is_some() => Ok(match self.manifest_get()? {
                    Some(v) => Conditional::Modified(v),
                    None => Conditional::Missing,
                }),
                other => Ok(other),
            };
        }
        Ok(match self.manifest_get()? {
            None => Conditional::Missing,
            Some(v) if v.tag == have => Conditional::NotModified,
            Some(v) => Conditional::Modified(v),
        })
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        if key == KEY {
            return Ok(self.manifest_get()?.map(|v| {
                let s = (offset as usize).min(v.data.len());
                let e = s.saturating_add(len as usize).min(v.data.len());
                v.data[s..e].to_vec()
            }));
        }
        self.inner.get_range(key, offset, len)
    }
    fn get_range_shared(&self, key: &str, offset: u64, len: u64) -> Result<Option<SharedBytes>, BackendError> {
        if key == KEY {
            return Ok(self.get_range(key, offset, len)?.map(SharedBytes::from_vec));
        }
        self.inner.get_range_shared(key, offset, len)
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        self.inner.exists(key)
    }
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        self.inner.put_if_absent(key, data)
    }
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        if key == KEY && tag.starts_with(TAG_PREFIX) {
            return self.manifest_cas(data, tag);
        }
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
    fn local_flock(&self, name: &str) -> Result<LocalFlock, BackendError> {
        self.inner.local_flock(name)
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
