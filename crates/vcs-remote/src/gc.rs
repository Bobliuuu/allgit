//! **DESTRUCTIVE** remote garbage collection: protocol G1 of docs/gc.md.
//!
//! Time is the backend's clock (`Backend::now_ms`, `modified_ms`), never the client's. `G` is
//! the grace period and `H` the publishers' hold time ([`crate::protocol::DEFAULT_HOLD_MS`]);
//! safety needs `G > H` (assumption A1), which [`RemoteGcOptions`] enforces.
//!
//! One sweep, by a single sweeper at a time (`gc/lock`):
//!
//! 1. **Candidates**: objects older than `G` that are not in the closure of any root.
//! 2. **Tomb** every candidate (create-only `tomb/<id>`); all tombs exist before step 3.
//! 3. **Remark**: re-read the manifest and re-list; `keep` = closure(roots) plus the closure of
//!    every object younger than `G`; `delete` = candidates minus `keep`.
//! 4. Delete `delete`, parents before children, removing each tombstone after its object.
//!
//! A publisher (see [`crate::protocol::Publisher`]) touches every object it deduplicated
//! against, then looks for a tombstone, then re-checks existence, then CASes within `H`. That
//! Dekker-style handshake is what makes an *unconditional* delete safe (S3 has no conditional
//! delete). Roots: every branch head, every named snapshot, every tag, and the old/new heads of every
//! transaction in the manifest's applied window (the retention window). `tx/` records, the
//! manifest and `packs/` are never deleted by this module.
//!
//! Not covered (also in docs/gc.md): packs on the remote are never rewritten or deleted;
//! a sweeper that outlives its lock TTL is not detected; readers slower than `G` can lose
//! objects of a head that has since been replaced.

use crate::backend::{Backend, BackendError};
use crate::packfetch::PACK_PREFIX;
use crate::protocol::{deps, load_manifest, object_key, pack_tomb_key, tomb_key, tx_key, GcQuirks, PublishError, DEFAULT_HOLD_MS};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use vcs_objects::{id_of_encoded, Object, ObjectId};
use vcs_store::pack::PackIndex;

pub const LOCK_KEY: &str = "gc/lock";
const TOMB_PREFIX: &str = "tomb/";
const OBJECT_PREFIX: &str = "objects/";
/// Default grace: 7 days (must exceed the publishers' hold time plus rate skew, A1).
pub const DEFAULT_GRACE_MS: u64 = 7 * 24 * 3600 * 1000;

#[derive(Debug, thiserror::Error)]
pub enum GcError {
    #[error("this backend cannot be garbage collected (needs delete, a trusted clock, object ages and touch)")]
    Unsupported,
    #[error("grace period {grace_ms} ms must exceed twice the publisher hold time ({hold_ms} ms); a shorter grace can delete objects a slow publisher still needs")]
    UnsafeGrace { grace_ms: u64, hold_ms: u64 },
    #[error("another gc holds {LOCK_KEY} until {0} (backend clock, unix ms)")]
    Locked(u64),
    #[error("remote is damaged, refusing to collect: {0}")]
    Damaged(String),
    #[error(transparent)]
    Backend(#[from] BackendError),
}

impl From<PublishError> for GcError {
    fn from(e: PublishError) -> Self {
        match e {
            PublishError::Backend(b) => GcError::Backend(b),
            other => GcError::Damaged(other.to_string()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RemoteGcOptions {
    pub grace_ms: u64,
    /// The hold time publishers use (`Publisher::with_hold_ms`); grace must exceed twice it.
    pub hold_ms: u64,
    /// Report only: no lock, no tombstones, no deletes.
    pub dry_run: bool,
    /// Lock lifetime. A crashed sweeper's lock can be taken over after this.
    pub lock_ttl_ms: u64,
    /// Written into the lock and tombstones (diagnostics).
    pub owner: String,
    /// Skip the `grace > 2 * hold` check. UNSAFE; tests only.
    pub allow_short_grace: bool,
}

impl Default for RemoteGcOptions {
    fn default() -> Self {
        RemoteGcOptions {
            grace_ms: DEFAULT_GRACE_MS,
            hold_ms: DEFAULT_HOLD_MS,
            dry_run: true,
            lock_ttl_ms: 6 * 3600 * 1000,
            owner: "gc".into(),
            allow_short_grace: false,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct RemoteGcReport {
    pub dry_run: bool,
    /// Objects found (loose plus inside packs; an object in both counts twice).
    pub objects: usize,
    /// Packs found under `packs/`.
    pub packs: usize,
    /// Objects in the closure of a root.
    pub reachable: usize,
    /// Units (loose objects or packs) unreachable but younger than the grace period.
    pub kept_young: usize,
    /// Units old and wholly unreachable at the first look.
    pub candidates: usize,
    /// Candidates the remark saved (became reachable, were touched, or a survivor needs them).
    pub saved_by_remark: usize,
    /// Objects removed (loose, plus every object of a deleted pack). Dry run: upper bound.
    pub deleted: Vec<ObjectId>,
    /// Packs removed (stems). Dry run: upper bound.
    pub deleted_packs: Vec<String>,
    pub stale_tombs_cleared: usize,
    /// Packs kept although they contain some unreachable objects (no remote pack rewriting).
    pub packs_with_garbage_kept: usize,
}

/// Points at which a test can interleave other actors with the sweeper.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GcPoint {
    Candidates,
    Tombed,
    Remarked,
    BeforeDelete,
    AfterDelete,
}

/// A deletable unit.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Unit {
    Loose(ObjectId),
    Pack(String),
}

/// A consistent-enough view of what the remote holds, rebuilt for every phase.
struct View<'a> {
    b: &'a dyn Backend,
    /// stem -> index (None: a `.pack` with no usable `.idx`)
    packs: BTreeMap<String, Option<PackIndex>>,
    loose: Vec<ObjectId>,
    /// object -> first pack holding it
    loc: HashMap<ObjectId, String>,
}

fn parse_key(k: &str) -> Option<ObjectId> {
    let rest = k.strip_prefix(OBJECT_PREFIX)?;
    let (a, b) = rest.split_once('/')?;
    ObjectId::from_hex(&format!("{a}{b}")).ok()
}

impl<'a> View<'a> {
    fn load(b: &'a dyn Backend) -> Result<View<'a>, GcError> {
        let loose: Vec<ObjectId> = b.list(OBJECT_PREFIX)?.iter().filter_map(|k| parse_key(k)).collect();
        let mut packs: BTreeMap<String, Option<PackIndex>> = BTreeMap::new();
        for k in b.list(PACK_PREFIX)? {
            if let Some(stem) = k.strip_suffix(".idx") {
                let idx = match b.get(&k)? {
                    Some(v) => Some(PackIndex::parse(&v.data, Path::new(stem)).map_err(|e| GcError::Damaged(format!("{k}: {e}")))?),
                    None => None, // deleted since the listing
                };
                packs.insert(stem.to_owned(), idx);
            } else if let Some(stem) = k.strip_suffix(".pack") {
                packs.entry(stem.to_owned()).or_insert(None);
            }
        }
        let mut loc = HashMap::new();
        for (stem, idx) in &packs {
            for (id, _, _) in idx.iter().flat_map(|i| i.iter()) {
                loc.entry(id).or_insert_with(|| stem.clone());
            }
        }
        Ok(View { b, packs, loose, loc })
    }

    fn pack_ids(&self, stem: &str) -> Vec<ObjectId> {
        self.packs
            .get(stem)
            .and_then(|i| i.as_ref())
            .map(|i| i.iter().map(|e| e.0).collect())
            .unwrap_or_default()
    }

    fn read(&self, id: &ObjectId) -> Result<Option<Object>, GcError> {
        let bytes = if let Some(stem) = self.loc.get(id) {
            let idx = self.packs[stem]
                .as_ref()
                .ok_or_else(|| GcError::Damaged(format!("{id}: not in {stem}")))?;
            let (off, len) = idx.find(id).ok_or_else(|| GcError::Damaged(format!("{id}: not in {stem}")))?;
            match self.b.get_range(&format!("{stem}.pack"), off, len as u64)? {
                Some(d) if d.len() == len as usize => vcs_store::pack::decode_payload(idx.version(), &d)
                    .map_err(|e| GcError::Damaged(format!("{id}: pack {stem}: {e}")))?
                    .into_owned(),
                _ => return Err(GcError::Damaged(format!("{id}: pack {stem} is truncated or missing"))),
            }
        } else {
            match self.b.get(&object_key(id))? {
                Some(v) => v.data,
                None => return Ok(None),
            }
        };
        if id_of_encoded(&bytes) != *id {
            return Err(GcError::Damaged(format!("{id}: fails hash verification")));
        }
        Object::decode(&bytes).map(Some).map_err(|e| GcError::Damaged(format!("{id}: {e}")))
    }

    /// Closure of `starts` into `into`. With `strict`, a missing object is a damaged remote.
    fn closure(&self, starts: Vec<ObjectId>, strict: bool, into: &mut HashSet<ObjectId>) -> Result<(), GcError> {
        let mut stack = starts;
        while let Some(id) = stack.pop() {
            if !into.insert(id) {
                continue;
            }
            match self.read(&id)? {
                Some(o) => stack.extend(deps(&o)),
                None if strict => return Err(GcError::Damaged(format!("{id} is referenced but missing"))),
                None => {}
            }
        }
        Ok(())
    }

    /// Age of a unit on the backend clock; `None` if it is gone. A pack is as young as its
    /// youngest key (a touch that landed on one of the two still protects it).
    fn age(&self, u: &Unit, at: u64) -> Result<Option<u64>, GcError> {
        let keys: Vec<String> = match u {
            Unit::Loose(id) => vec![object_key(id)],
            Unit::Pack(stem) => vec![format!("{stem}.idx"), format!("{stem}.pack")],
        };
        let mut youngest: Option<u64> = None;
        for k in keys {
            if let Some(m) = self.b.modified_ms(&k)? {
                let a = at.saturating_sub(m);
                youngest = Some(youngest.map_or(a, |y| y.min(a)));
            }
        }
        Ok(youngest)
    }

    fn all_units(&self) -> Vec<Unit> {
        let mut v: Vec<Unit> = self.loose.iter().map(|i| Unit::Loose(*i)).collect();
        v.extend(self.packs.keys().map(|s| Unit::Pack(s.clone())));
        v
    }

    fn objects_of(&self, u: &Unit) -> Vec<ObjectId> {
        match u {
            Unit::Loose(id) => vec![*id],
            Unit::Pack(s) => self.pack_ids(s),
        }
    }
}

fn tomb_of(u: &Unit) -> String {
    match u {
        Unit::Loose(id) => tomb_key(id),
        Unit::Pack(s) => pack_tomb_key(s),
    }
}

/// Roots from the manifest: branch heads, snapshots, and the old/new heads of every transaction
/// in the applied (retention) window.
fn roots(b: &dyn Backend, quirks: &GcQuirks) -> Result<Vec<ObjectId>, GcError> {
    if quirks.sweeper_ignores_manifest {
        return Ok(vec![]);
    }
    let (m, _) = load_manifest(b)?;
    let mut r: Vec<ObjectId> = m
        .branches
        .values()
        .copied()
        .chain(m.snapshots.values().copied())
        .chain(m.tags.values().copied())
        .collect();
    for txid in &m.applied {
        if let Some(v) = b.get(&tx_key(txid))? {
            if let Ok(Object::Transaction(t)) = Object::decode(&v.data) {
                for u in &t.updates {
                    r.extend(u.old);
                    r.push(u.new);
                }
            }
        }
    }
    Ok(r)
}

struct LockGuard<'a> {
    b: &'a dyn Backend,
    mine: Vec<u8>,
}
impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        // Only remove the lock if it is still ours (it may have been taken over after expiry).
        if let Ok(Some(v)) = self.b.get(LOCK_KEY) {
            if v.data == self.mine {
                let _ = self.b.delete(LOCK_KEY);
            }
        }
    }
}

fn acquire<'a>(b: &'a dyn Backend, owner: &str, ttl_ms: u64, now: u64) -> Result<LockGuard<'a>, GcError> {
    let mine = format!("{owner} {}", now + ttl_ms).into_bytes();
    if b.put_if_absent(LOCK_KEY, &mine)? {
        return Ok(LockGuard { b, mine });
    }
    let Some(cur) = b.get(LOCK_KEY)? else {
        return Err(GcError::Locked(0)); // vanished between the two calls: let the caller retry
    };
    let exp = String::from_utf8_lossy(&cur.data)
        .rsplit(' ')
        .next()
        .and_then(|x| x.parse::<u64>().ok())
        .unwrap_or(u64::MAX);
    if exp > now {
        return Err(GcError::Locked(exp));
    }
    // Expired: take over with a CAS so two takers cannot both win.
    match b.put_if_match(LOCK_KEY, &mine, &cur.tag)? {
        Some(_) => Ok(LockGuard { b, mine }),
        None => Err(GcError::Locked(now)),
    }
}

/// **DESTRUCTIVE unless `opts.dry_run`.** Run one remote GC pass.
pub fn remote_gc(b: &dyn Backend, opts: &RemoteGcOptions) -> Result<RemoteGcReport, GcError> {
    remote_gc_with(b, opts, GcQuirks::default(), &mut |_| {})
}

/// [`remote_gc`] with protocol quirks and an interleaving hook (tests).
#[doc(hidden)]
pub fn remote_gc_with(
    b: &dyn Backend,
    opts: &RemoteGcOptions,
    quirks: GcQuirks,
    hook: &mut dyn FnMut(GcPoint),
) -> Result<RemoteGcReport, GcError> {
    if !b.supports_gc() {
        return Err(GcError::Unsupported);
    }
    if !opts.allow_short_grace && opts.grace_ms <= opts.hold_ms.saturating_mul(2) {
        return Err(GcError::UnsafeGrace {
            grace_ms: opts.grace_ms,
            hold_ms: opts.hold_ms,
        });
    }
    let mut rep = RemoteGcReport {
        dry_run: opts.dry_run,
        ..Default::default()
    };
    let now = b.now_ms()?;
    let _lock = if opts.dry_run {
        None
    } else {
        let g = acquire(b, &opts.owner, opts.lock_ttl_ms, now)?;
        // We are the only sweeper: any tombstone now present is left over from a crashed one.
        for k in b.list(TOMB_PREFIX)? {
            b.delete(&k)?;
            rep.stale_tombs_cleared += 1;
        }
        Some(g)
    };
    let grace = opts.grace_ms;

    // 1. Candidates: units that are old and contain nothing reachable.
    let view = View::load(b)?;
    rep.packs = view.packs.len();
    let mut marked = HashSet::new();
    view.closure(roots(b, &quirks)?, true, &mut marked)?;
    let mut candidates: Vec<Unit> = Vec::new();
    for u in view.all_units() {
        let objs = view.objects_of(&u);
        rep.objects += objs.len();
        let reach = objs.iter().filter(|o| marked.contains(o)).count();
        rep.reachable += reach;
        if reach > 0 {
            if matches!(u, Unit::Pack(_)) && reach < objs.len() {
                rep.packs_with_garbage_kept += 1;
            }
            continue;
        }
        match view.age(&u, now)? {
            Some(a) if a > grace => candidates.push(u),
            Some(_) => rep.kept_young += 1,
            None => {} // vanished
        }
    }
    rep.candidates = candidates.len();
    hook(GcPoint::Candidates);
    if opts.dry_run {
        for u in &candidates {
            match u {
                Unit::Loose(id) => rep.deleted.push(*id),
                Unit::Pack(s) => {
                    rep.deleted.extend(view.pack_ids(s));
                    rep.deleted_packs.push(s.clone());
                }
            }
        }
        return Ok(rep);
    }

    // 2/3. Tombstones, then Remark (or the broken order, for the mutation test).
    let cand_set: HashSet<Unit> = candidates.iter().cloned().collect();
    let tomb_all = || -> Result<(), GcError> {
        for u in &candidates {
            b.put_if_absent(&tomb_of(u), opts.owner.as_bytes())?;
        }
        Ok(())
    };
    // keep = closure(roots) + closure(every object not in a candidate unit) + closure(candidates
    // touched since the first look). The middle part is "young objects are roots" and, more
    // generally, I1: a survivor must not lose a dependency. Candidates were chosen at one
    // instant; if time moves on (a stalled sweeper, a clock step) a parent can stop being young
    // while its child is a candidate, and deleting only the child would leave the parent dangling.
    let keep_set = |v: &View| -> Result<(HashSet<ObjectId>, HashSet<Unit>), GcError> {
        let mut keep = HashSet::new();
        let mut touched_units = HashSet::new();
        v.closure(roots(b, &quirks)?, true, &mut keep)?;
        if !quirks.sweeper_young_not_roots {
            let mut survivors = Vec::new();
            let at = b.now_ms()?;
            for u in v.all_units() {
                let touched = cand_set.contains(&u) && matches!(v.age(&u, at)?, Some(a) if a <= grace);
                if touched {
                    touched_units.insert(u.clone());
                }
                if !cand_set.contains(&u) || touched {
                    survivors.extend(v.objects_of(&u));
                }
            }
            v.closure(survivors, false, &mut keep)?;
        }
        Ok((keep, touched_units))
    };
    // The decision uses exactly what `keep_set` saw: a touch that lands after it is the
    // publisher's handshake to handle (it will find the tombstone or the missing object), and
    // consulting fresh ages here would let a child be deleted under a parent that was just spared.
    let decide = |v: &View, keep: &HashSet<ObjectId>, touched: &HashSet<Unit>| -> Vec<Unit> {
        candidates
            .iter()
            .filter(|u| !touched.contains(*u) && !v.objects_of(u).iter().any(|o| keep.contains(o)))
            .cloned()
            .collect()
    };
    let (delete, view2) = if quirks.sweeper_tombs_after_deciding {
        let (keep, touched) = keep_set(&view)?;
        let delete = decide(&view, &keep, &touched);
        hook(GcPoint::Remarked);
        tomb_all()?;
        hook(GcPoint::Tombed);
        (delete, view)
    } else {
        tomb_all()?;
        hook(GcPoint::Tombed);
        let v2 = View::load(b)?;
        let (keep, touched) = keep_set(&v2)?;
        let delete = decide(&v2, &keep, &touched);
        hook(GcPoint::Remarked);
        (delete, v2)
    };
    let doomed: HashSet<Unit> = delete.iter().cloned().collect();
    rep.saved_by_remark = candidates.len() - doomed.len();
    // Decided to keep: remove those tombstones now (I3: tombs go only after the decision).
    for u in candidates.iter().filter(|u| !doomed.contains(*u)) {
        b.delete(&tomb_of(u))?;
    }

    // 4. Order: parents before children. Edge parent -> child between doomed units.
    let mut unit_of: HashMap<ObjectId, Vec<Unit>> = HashMap::new();
    for u in &doomed {
        for o in view2.objects_of(u) {
            unit_of.entry(o).or_default().push(u.clone());
        }
    }
    let mut children_of: HashMap<Unit, Vec<Unit>> = HashMap::new();
    let mut indeg: BTreeMap<Unit, usize> = doomed.iter().map(|d| (d.clone(), 0)).collect();
    for p in &doomed {
        for o in view2.objects_of(p) {
            let Some(obj) = view2.read(&o)? else { continue };
            for c in deps(&obj) {
                for cu in unit_of.get(&c).into_iter().flatten() {
                    if cu != p {
                        children_of.entry(p.clone()).or_default().push(cu.clone());
                        *indeg.get_mut(cu).unwrap() += 1;
                    }
                }
            }
        }
    }
    let mut ready: Vec<Unit> = indeg.iter().filter(|(_, n)| **n == 0).map(|(u, _)| u.clone()).collect();
    let mut order: Vec<Unit> = Vec::new();
    let mut emitted: HashSet<Unit> = HashSet::new();
    while let Some(p) = ready.pop() {
        if !emitted.insert(p.clone()) {
            continue;
        }
        order.push(p.clone());
        for c in children_of.get(&p).into_iter().flatten() {
            let n = indeg.get_mut(c).unwrap();
            *n = n.saturating_sub(1);
            if *n == 0 {
                ready.push(c.clone());
            }
        }
    }
    // Units on a dependency cycle (unreachable packs referring to each other) go last, in key order.
    for u in indeg.keys() {
        if !emitted.contains(u) {
            order.push(u.clone());
        }
    }
    for u in order {
        hook(GcPoint::BeforeDelete);
        match &u {
            Unit::Loose(id) => {
                b.delete(&object_key(id))?;
                rep.deleted.push(*id);
            }
            Unit::Pack(stem) => {
                // Index first: readers enumerate `.idx`, so nobody is pointed at a vanishing pack.
                b.delete(&format!("{stem}.idx"))?;
                b.delete(&format!("{stem}.pack"))?;
                rep.deleted.extend(view2.pack_ids(stem));
                rep.deleted_packs.push(stem.clone());
            }
        }
        b.delete(&tomb_of(&u))?;
        hook(GcPoint::AfterDelete);
    }
    Ok(rep)
}
