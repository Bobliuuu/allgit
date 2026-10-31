//! Publish and fetch.
//!
//! Publish step machine (every step idempotent; state lives only in the Publisher):
//!
//! ```text
//! Collect  walk local closure of new heads, stopping at objects the remote already has
//!          (invariant: a remote object's whole closure is present – upload is post-order)
//! Upload   put_if_absent each object, dependencies first
//! PutTx    put_if_absent tx/<txid>
//! Load     read manifest; if txid already applied -> done; check every update.old == current
//! Cas      conditional manifest write  <-- linearization point
//! ```
//! `Unavailable` at any step re-runs that step (bounded); after a CAS error we always go
//! back to `Load`, which detects "applied but ack lost". Exhausted retries yield
//! `PublishError::Unknown`: the outcome is undetermined and re-running the same transaction
//! is safe and converges.

use crate::backend::{Backend, BackendError};
use crate::manifest::{self, Manifest, MAX_APPLIED};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use vcs_objects::{id_of_encoded, Object, ObjectId, Transaction};
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
    #[error("zone definition conflict: {0}")]
    ZoneConflict(String),
}

fn fmt_id(i: &Option<ObjectId>) -> String {
    i.map(|i| i.short()).unwrap_or_else(|| "(none)".into())
}

pub fn object_key(id: &ObjectId) -> String {
    let h = id.to_hex();
    format!("objects/{}/{}", &h[..2], &h[2..])
}
fn tx_key(id: &ObjectId) -> String {
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
    PutTx,
    Load,
    Cas { expected_tag: Option<String>, new: Box<Manifest> },
    Finished,
}

pub struct Publisher<'a> {
    backend: &'a dyn Backend,
    local: &'a dyn ObjectStore,
    tx: Transaction,
    txid: ObjectId,
    state: State,
    to_upload: Vec<ObjectId>,
    retries: u32,
    zones: BTreeMap<String, String>,
}

/// Dependencies of an object (what must exist remotely before it may).
fn deps(o: &Object) -> Vec<ObjectId> {
    match o {
        Object::Blob(_) | Object::Transaction(_) | Object::Operation(_) => vec![],
        Object::Tree(t) => t.entries().iter().map(|e| e.id).collect(),
        Object::Commit(c) => c.parents.iter().copied().chain(std::iter::once(c.root)).collect(),
        Object::Snapshot(s) => s.zones.values().copied().collect(),
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
            retries: 0,
            zones: BTreeMap::new(),
        }
    }
    /// Zone definitions (name -> path) to register in the manifest atomically with this tx.
    pub fn with_zones(mut self, zones: BTreeMap<String, String>) -> Self {
        self.zones = zones;
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

    pub fn step(&mut self) -> Result<Step, PublishError> {
        match std::mem::replace(&mut self.state, State::Finished) {
            State::Finished => Ok(Step::Continue),
            State::Collect => {
                let mut list = Vec::new();
                let mut seen = HashSet::new();
                let heads: BTreeSet<ObjectId> = self.tx.updates.iter().map(|u| u.new).collect();
                for h in heads {
                    let (backend, local) = (self.backend, self.local);
                    let r = post_order(
                        h,
                        &mut |id| backend.exists(&object_key(id)).map_err(Into::into),
                        &mut |id| local.get(id).map_err(Into::into),
                        &mut |id, _| {
                            list.push(*id);
                            Ok(())
                        },
                        &mut seen,
                    );
                    match r {
                        Err(PublishError::Backend(e)) => {
                            self.state = State::Collect;
                            return self.retry(e);
                        }
                        Err(e) => return Err(e),
                        Ok(()) => {}
                    }
                }
                self.to_upload = list;
                self.state = State::Upload(0);
                Ok(Step::Continue)
            }
            State::Upload(i) => {
                if i >= self.to_upload.len() {
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
    let v = backend
        .get(&object_key(id))?
        .ok_or_else(|| PublishError::Remote(format!("object {id} missing on remote")))?;
    if id_of_encoded(&v.data) != *id {
        return Err(PublishError::Remote(format!("object {id} fails hash verification")));
    }
    Object::decode(&v.data).map_err(|e| PublishError::Remote(format!("object {id}: {e}")))
}

/// Fetch the closure of the heads of `zones` (None = all) into `local`, storing
/// dependencies first and verifying every object's hash. Returns the fetched heads.
/// Only objects reachable from the requested zones are read.
pub fn fetch_heads(
    backend: &dyn Backend,
    local: &dyn ObjectStore,
    zones: Option<&[String]>,
) -> Result<Vec<(String, String, ObjectId)>, PublishError> {
    let (m, _) = load_manifest(backend)?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for ((a, b), id) in &m.branches {
        if zones.is_some_and(|w| !w.iter().any(|x| x == a)) {
            continue;
        }
        post_order(
            *id,
            &mut |i| Ok(local.contains(i)?),
            &mut |i| {
                let v = backend
                    .get(&object_key(i))?
                    .ok_or_else(|| PublishError::Remote(format!("object {i} missing on remote")))?;
                if id_of_encoded(&v.data) != *i {
                    return Err(PublishError::Remote(format!("object {i} fails hash verification")));
                }
                Object::decode(&v.data).map_err(|e| PublishError::Remote(format!("object {i}: {e}")))
            },
            &mut |_, o| {
                local.put(o)?;
                Ok(())
            },
            &mut seen,
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
        )
        .unwrap();
        let pos: BTreeMap<_, _> = order.iter().enumerate().map(|(i, x)| (*x, i)).collect();
        assert_eq!(order.len(), 4, "each object exactly once");
        assert!(pos[&blob] < pos[&c] && pos[&c] < pos[&b] && pos[&b] < pos[&a]);
    }
}
