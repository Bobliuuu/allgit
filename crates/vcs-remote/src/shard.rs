//! PROTOTYPE (feature `sharded`, issue #252): per-zone shard records with a two-phase
//! cross-zone commit, the protocol checked in `tests/model_sharded.rs`. Not wired into
//! `Publisher`, fetch, GC or the CLI; the single manifest remains the authoritative state.
//!
//! Keys (all on a [`Backend`] with create-only PUT and CAS):
//! * `shard/<zone>`     = [`Shard`]: generation, branch heads, at most one pending record.
//! * `decision/<txid>`  = one byte, `C` or `A`, create-only. The create-only write that makes a
//!   transaction `C`ommitted is its linearization point; whoever creates the record first
//!   decides, and it never changes.
//!
//! Writer (rules named after the model's flags):
//! 1. Prepare every zone in ascending order: CAS `pending=(txid, updates)` iff every `old`
//!    matches and no pending exists (`respect_foreign_pending`). A foreign pending is resolved
//!    from its decision record first: Committed -> apply it, Aborted -> clear it, absent ->
//!    abort it (`undecided_means_abort`), except that a LOWER txid is waited for (bounded)
//!    before it is aborted (`wound_wait`: avoids two transactions aborting each other forever).
//! 2. Decide only after every zone is prepared (`decide_after_all_prepared`): create
//!    `decision/<txid>` = C. If it already says A, clean up and report an abort.
//! 3. Finalize every shard (`head=new, pending=None`).
//!
//! Reader: collect all requested shards (effective head = the pending update iff its transaction
//! is Committed, else the head) until two consecutive collects are identical
//! (`validate_reads` + `reader_uses_decision`): a linearizable multi-zone snapshot.
//!
//! Not modelled / not done here: garbage collection of decision records, a bound on the
//! number of branches per shard, integration with locks, tags, labels, snapshots and the
//! applied-txid idempotency list (a lost acknowledgement is recovered by re-running the same
//! txid, whose decision record answers), and any use of an object-store clock for the wait.

use crate::backend::{Backend, BackendError};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::ObjectId;

pub fn shard_key(zone: &str) -> String {
    format!("shard/{zone}")
}
pub fn decision_key(txid: &ObjectId) -> String {
    format!("decision/{}", txid.to_hex())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShardUpdate {
    pub zone: String,
    pub branch: String,
    pub old: Option<ObjectId>,
    pub new: ObjectId,
}

/// The updates of one transaction inside one zone.
type ZoneUps = Vec<(String, Option<ObjectId>, ObjectId)>;
type Collect = Vec<(u64, BTreeMap<String, ObjectId>)>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shard {
    pub generation: u64,
    pub heads: BTreeMap<String, ObjectId>,
    pub pending: Option<(ObjectId, ZoneUps)>,
}

#[derive(Debug, thiserror::Error)]
pub enum ShardError {
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error("malformed shard or decision record: {0}")]
    Malformed(String),
    #[error("{zone}/{branch} is at {found:?}, expected {expected:?}")]
    Diverged {
        zone: String,
        branch: String,
        expected: Option<ObjectId>,
        found: Option<ObjectId>,
    },
    #[error("gave up after {0} contention aborts")]
    Contended(u32),
}

fn opt_id(w: &mut Writer, o: &Option<ObjectId>) {
    match o {
        Some(i) => {
            w.u8(1).id(i);
        }
        None => {
            w.u8(0);
        }
    }
}
fn get_opt_id(r: &mut Reader) -> Result<Option<ObjectId>, String> {
    match r.u8().map_err(|e| e.to_string())? {
        0 => Ok(None),
        1 => Ok(Some(r.id().map_err(|e| e.to_string())?)),
        _ => Err("bad option tag".into()),
    }
}

impl Shard {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.raw(b"AGSH").u8(1).varint(self.generation).varint(self.heads.len() as u64);
        for (b, id) in &self.heads {
            w.str(b).id(id);
        }
        match &self.pending {
            None => {
                w.u8(0);
            }
            Some((t, ups)) => {
                w.u8(1).id(t).varint(ups.len() as u64);
                for (b, old, new) in ups {
                    w.str(b);
                    opt_id(&mut w, old);
                    w.id(new);
                }
            }
        }
        w.finish()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ShardError> {
        let m = |e: String| ShardError::Malformed(e);
        let e = |x: vcs_objects::DecodeError| ShardError::Malformed(x.to_string());
        let mut r = Reader::new(bytes);
        if r.raw(4).map_err(e)? != b"AGSH" || r.u8().map_err(e)? != 1 {
            return Err(m("bad magic or version".into()));
        }
        let mut s = Shard {
            generation: r.varint().map_err(e)?,
            ..Default::default()
        };
        for _ in 0..r.count(34).map_err(e)? {
            let b = r.str(200).map_err(e)?;
            vcs_objects::names::validate_branch_name(&b).map_err(|x| m(x.to_string()))?;
            if s.heads.last_key_value().is_some_and(|(k, _)| *k >= b) {
                return Err(m("branches not strictly ascending".into()));
            }
            s.heads.insert(b, r.id().map_err(e)?);
        }
        if r.u8().map_err(e)? == 1 {
            let t = r.id().map_err(e)?;
            let mut ups: ZoneUps = Vec::new();
            for _ in 0..r.count(34).map_err(e)? {
                let b = r.str(200).map_err(e)?;
                vcs_objects::names::validate_branch_name(&b).map_err(|x| m(x.to_string()))?;
                if ups.last().is_some_and(|(k, _, _)| *k >= b) {
                    return Err(m("pending branches not strictly ascending".into()));
                }
                let old = get_opt_id(&mut r).map_err(m)?;
                ups.push((b, old, r.id().map_err(e)?));
            }
            s.pending = Some((t, ups));
        }
        r.finish().map_err(e)?;
        Ok(s)
    }

    /// Heads after applying the pending record (used when its transaction is Committed).
    fn applied(&self) -> BTreeMap<String, ObjectId> {
        let mut h = self.heads.clone();
        if let Some((_, ups)) = &self.pending {
            for (b, _, new) in ups {
                h.insert(b.clone(), *new);
            }
        }
        h
    }
}

/// Where a writer stops, to simulate a crash (tests and measurements only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashAt {
    AfterPrepare(usize),
    AfterDecide,
    AfterFinalize(usize),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ShardOutcome {
    Committed,
    /// The transaction was aborted by a conflicting helper or writer (retry with a NEW txid), or
    /// the simulated crash point was reached.
    Aborted,
    Crashed,
}

pub struct ShardedPublisher<'a> {
    backend: &'a dyn Backend,
    /// How long to wait for a lower-txid pending record before aborting it.
    pub wait: Duration,
    pub crash_at: Option<CrashAt>,
    /// Counters for measurements.
    pub cas_attempts: u64,
    pub cas_lost: u64,
}

impl<'a> ShardedPublisher<'a> {
    pub fn new(backend: &'a dyn Backend) -> Self {
        Self {
            backend,
            wait: Duration::from_millis(250),
            crash_at: None,
            cas_attempts: 0,
            cas_lost: 0,
        }
    }

    fn read(&self, zone: &str) -> Result<(Shard, Option<String>), ShardError> {
        match self.backend.get(&shard_key(zone))? {
            None => Ok((Shard::default(), None)),
            Some(v) => Ok((Shard::decode(&v.data)?, Some(v.tag))),
        }
    }

    fn cas(&mut self, zone: &str, s: &Shard, tag: &Option<String>) -> Result<bool, ShardError> {
        self.cas_attempts += 1;
        let key = shard_key(zone);
        let ok = match tag {
            None => self.backend.put_if_absent(&key, &s.encode())?,
            Some(t) => self.backend.put_if_match(&key, &s.encode(), t)?.is_some(),
        };
        if !ok {
            self.cas_lost += 1;
        }
        Ok(ok)
    }

    /// `Some(true)` Committed, `Some(false)` Aborted, `None` undecided.
    pub fn decision(&self, txid: &ObjectId) -> Result<Option<bool>, ShardError> {
        match self.backend.get(&decision_key(txid))? {
            None => Ok(None),
            Some(v) => match v.data.as_slice() {
                b"C" => Ok(Some(true)),
                b"A" => Ok(Some(false)),
                _ => Err(ShardError::Malformed("decision record".into())),
            },
        }
    }

    /// Create the decision if nobody has; return the winning decision either way.
    fn decide(&self, txid: &ObjectId, commit: bool) -> Result<bool, ShardError> {
        self.backend.put_if_absent(&decision_key(txid), if commit { b"C" } else { b"A" })?;
        self.decision(txid)?
            .ok_or_else(|| ShardError::Malformed("decision vanished".into()))
    }

    /// Clear (or apply) a foreign pending record whose decision is known.
    fn resolve(&mut self, zone: &str, mut s: Shard, tag: &Option<String>, commit: bool) -> Result<(), ShardError> {
        if commit {
            s.heads = s.applied();
        }
        s.pending = None;
        s.generation += 1;
        self.cas(zone, &s, tag)?; // a lost race means someone else resolved it; the caller re-reads
        Ok(())
    }

    /// Run one transaction to completion under `txid`. Idempotent: re-running a crashed
    /// or unacknowledged txid finishes it (its decision record, if any, is authoritative).
    pub fn publish(&mut self, txid: ObjectId, ups: &[ShardUpdate]) -> Result<ShardOutcome, ShardError> {
        let mut by_zone: BTreeMap<&str, ZoneUps> = BTreeMap::new();
        for u in ups {
            by_zone.entry(&u.zone).or_default().push((u.branch.clone(), u.old, u.new));
        }
        for v in by_zone.values_mut() {
            v.sort();
        }
        let zones: Vec<(&str, ZoneUps)> = by_zone.into_iter().collect();
        match self.decision(&txid)? {
            Some(true) => return self.finalize(txid, &zones, true),
            Some(false) => return self.cleanup(txid, &zones, ShardOutcome::Aborted),
            None => {}
        }
        // Phase 1: prepare, ascending zone order.
        for (k, (zone, zups)) in zones.iter().enumerate() {
            let started = Instant::now();
            loop {
                let (s, tag) = self.read(zone)?;
                match &s.pending {
                    Some((t, _)) if *t == txid => break,
                    Some((t, _)) => {
                        let t = *t;
                        match self.decision(&t)? {
                            Some(c) => self.resolve(zone, s, &tag, c)?,
                            None if t < txid && started.elapsed() < self.wait => {
                                std::thread::sleep(Duration::from_micros(500));
                            }
                            None => {
                                let c = self.decide(&t, false)?;
                                self.resolve(zone, s, &tag, c)?;
                            }
                        }
                    }
                    None => {
                        for (b, old, _) in zups {
                            let found = s.heads.get(b).copied();
                            if found != *old {
                                self.decide(&txid, false)?;
                                self.cleanup(txid, &zones, ShardOutcome::Aborted)?;
                                return Err(ShardError::Diverged {
                                    zone: (*zone).into(),
                                    branch: b.clone(),
                                    expected: *old,
                                    found,
                                });
                            }
                        }
                        let mut n = s.clone();
                        n.pending = Some((txid, zups.clone()));
                        n.generation += 1;
                        if self.cas(zone, &n, &tag)? {
                            break;
                        }
                    }
                }
            }
            if self.crash_at == Some(CrashAt::AfterPrepare(k)) {
                return Ok(ShardOutcome::Crashed);
            }
        }
        // Phase 2: decide (the linearization point), then finalize.
        let committed = self.decide(&txid, true)?;
        if self.crash_at == Some(CrashAt::AfterDecide) {
            return Ok(ShardOutcome::Crashed);
        }
        if committed {
            self.finalize(txid, &zones, true)
        } else {
            self.cleanup(txid, &zones, ShardOutcome::Aborted)
        }
    }

    fn finalize(&mut self, txid: ObjectId, zones: &[(&str, ZoneUps)], commit: bool) -> Result<ShardOutcome, ShardError> {
        for (k, (zone, _)) in zones.iter().enumerate() {
            loop {
                let (s, tag) = self.read(zone)?;
                match &s.pending {
                    Some((t, _)) if *t == txid => {
                        self.resolve(zone, s, &tag, commit)?;
                    }
                    _ => break,
                }
            }
            if self.crash_at == Some(CrashAt::AfterFinalize(k)) {
                return Ok(ShardOutcome::Crashed);
            }
        }
        Ok(if commit { ShardOutcome::Committed } else { ShardOutcome::Aborted })
    }

    fn cleanup(&mut self, txid: ObjectId, zones: &[(&str, ZoneUps)], out: ShardOutcome) -> Result<ShardOutcome, ShardError> {
        self.finalize(txid, zones, false)?;
        Ok(out)
    }

    /// Publish with a fresh txid per contention abort: `base` is hashed with the attempt number.
    /// Returns the number of attempts used.
    pub fn publish_retrying(&mut self, base: &ObjectId, ups: &[ShardUpdate], max: u32) -> Result<u32, ShardError> {
        for attempt in 0..max {
            let txid = ObjectId::from_bytes(*blake3::hash(&[base.as_bytes().as_slice(), &attempt.to_le_bytes()].concat()).as_bytes());
            if self.publish(txid, ups)? == ShardOutcome::Committed {
                return Ok(attempt + 1);
            }
        }
        Err(ShardError::Contended(max))
    }

    /// Linearizable snapshot of the heads of `zones` (double collect).
    pub fn snapshot(&self, zones: &[&str]) -> Result<BTreeMap<(String, String), ObjectId>, ShardError> {
        let collect = || -> Result<Collect, ShardError> {
            zones
                .iter()
                .map(|z| {
                    let (s, _) = self.read(z)?;
                    let heads = match &s.pending {
                        Some((t, _)) if self.decision(t)? == Some(true) => s.applied(),
                        _ => s.heads.clone(),
                    };
                    Ok((s.generation, heads))
                })
                .collect()
        };
        let mut prev = collect()?;
        loop {
            let cur = collect()?;
            if cur == prev {
                let mut out = BTreeMap::new();
                for (z, (_, heads)) in zones.iter().zip(cur) {
                    for (b, id) in heads {
                        out.insert(((*z).to_owned(), b), id);
                    }
                }
                return Ok(out);
            }
            prev = cur;
        }
    }
}
