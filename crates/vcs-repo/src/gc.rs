//! Garbage collection. Design and safety argument: docs/gc.md.
//!
//! `gc_plan` computes what a sweep *would* delete and deletes nothing. `gc_sweep` is the
//! **destructive** local sweep: under the repository lock it removes unreachable objects older
//! than the grace period, rewrites packs without them and cleans crash leftovers.

use crate::error::Result;
use crate::reachcache::ReachCache;
use crate::repo::Repo;
use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};
use vcs_objects::{Kind, Object, ObjectId};
use vcs_store::{ObjectStore, OrphanStats, SweepStats};

#[derive(Debug, Default)]
pub struct GcReport {
    /// Objects present in the local store when the plan started.
    pub total: usize,
    /// Objects reachable from a root.
    pub reachable: usize,
    /// Unreachable, but written within the grace period (or age unknown): kept.
    pub kept_young: Vec<ObjectId>,
    /// Unreachable and older than the grace period: a sweep would delete these.
    pub would_delete: Vec<(ObjectId, Kind)>,
    /// Objects named by a root or another object that are not in the store.
    pub missing_referenced: usize,
    #[doc(hidden)]
    pub(crate) snapshot: Option<Snapshot>,
}

/// What a mark learned, for the reachability cache.
#[derive(Debug)]
pub(crate) struct Snapshot {
    ids: Vec<ObjectId>,
    marked: Vec<bool>,
    tx: Vec<bool>,
    roots: Vec<ObjectId>,
    missing: usize,
}

/// Options for the destructive sweep.
#[derive(Clone, Copy, Debug)]
pub struct GcOptions {
    /// Keep unreachable objects younger than this.
    pub grace: Duration,
    /// Allow a grace period at or below the store's touch-on-dedup threshold. UNSAFE: an
    /// old orphan re-adopted by a concurrent writer may then look old enough to delete.
    pub allow_short_grace: bool,
}

impl GcOptions {
    pub fn new(grace: Duration) -> Self {
        GcOptions {
            grace,
            allow_short_grace: false,
        }
    }
}

/// Result of a destructive sweep.
#[derive(Debug, Default)]
pub struct GcRun {
    /// The second (authoritative) plan: what was reachable and what was kept as young.
    pub plan: GcReport,
    /// Objects permanently deleted.
    pub deleted: Vec<(ObjectId, Kind)>,
    /// Candidates of the first pass that the fresh re-check dropped (became reachable or young).
    pub rechecked_out: usize,
    pub sweep: SweepStats,
    pub orphans: OrphanStats,
}

impl Repo {
    /// **DESTRUCTIVE.** Permanently delete unreachable objects older than `opts.grace`.
    ///
    /// Holds the repository lock throughout. Never deletes an object reachable from a branch,
    /// snapshot, tag, remote ref, the operation log, a stash, any stored transaction, the
    /// index, or merge / rebase / cherry-pick state (see `gc_roots`). Candidates are chosen
    /// twice (object list, then roots, each time) and only the intersection is deleted, so a
    /// writer that adopted an object between the passes saves it. Crash-safe: packs are
    /// rewritten before anything is unlinked (see `LocalStore::sweep`).
    pub fn gc_sweep(&self, opts: GcOptions) -> Result<GcRun> {
        self.gc_sweep_at(SystemTime::now(), opts)
    }

    /// [`Repo::gc_sweep`] as of `now` (injectable so tests need not sleep).
    pub fn gc_sweep_at(&self, now: SystemTime, opts: GcOptions) -> Result<GcRun> {
        let floor = self.store.touch_after();
        if opts.grace <= floor && !opts.allow_short_grace {
            return Err(crate::error::Error::Msg(format!(
                "refusing to delete with a grace period of {}s: it must exceed the touch-on-dedup threshold ({}s), \
                 otherwise a concurrent writer can re-adopt an object that this sweep then deletes (docs/gc.md)",
                opts.grace.as_secs(),
                floor.as_secs()
            )));
        }
        let _lock = self.lock()?;
        self.flush_refs()?; // deferred ref updates become durable before anything is deleted
        let orphans = self.store.clean_orphans(now, opts.grace)?;
        // Objects are immutable by id, so what the first pass learned (kind, children) is reused by the
        // second; its list, roots and ages are still taken fresh.
        let mut known = Known::new();
        let cache = if self.use_reach_cache { self.load_reach_cache() } else { None };
        let first = self.gc_plan_known(now, opts.grace, &mut known, cache.as_ref())?;
        // Fresh list + fresh roots + fresh ages, immediately before deleting.
        let mut second = self.gc_plan_known(now, opts.grace, &mut known, cache.as_ref())?;
        let confirmed: BTreeSet<ObjectId> = second.would_delete.iter().map(|x| x.0).collect();
        let doomed: BTreeSet<ObjectId> = first.would_delete.iter().map(|x| x.0).filter(|i| confirmed.contains(i)).collect();
        let deleted: Vec<(ObjectId, Kind)> = second.would_delete.iter().filter(|x| doomed.contains(&x.0)).copied().collect();
        let sweep = self.store.sweep(&doomed)?;
        if self.use_reach_cache {
            if let Some(snap) = second.snapshot.take() {
                if snap.missing == 0 {
                    // objects just deleted were unreachable: drop them from the scanned list
                    let keep: Vec<usize> = (0..snap.ids.len()).filter(|&i| !doomed.contains(&snap.ids[i])).collect();
                    let ids: Vec<ObjectId> = keep.iter().map(|&i| snap.ids[i]).collect();
                    let m: Vec<bool> = keep.iter().map(|&i| snap.marked[i]).collect();
                    let t: Vec<bool> = keep.iter().map(|&i| snap.tx[i]).collect();
                    self.save_reach_cache(&crate::reachcache::encode(&ids, &m, &t, &snap.roots));
                }
            }
        }
        self.warm_commit_graph();
        Ok(GcRun {
            rechecked_out: first.would_delete.len() - doomed.len(),
            plan: second,
            deleted,
            sweep,
            orphans,
        })
    }

    #[doc(hidden)]
    pub fn gc_set_sweep_crash(&self, c: Option<vcs_store::SweepCrash>) {
        self.store.set_sweep_crash(c);
    }

    /// Plan a sweep with the given grace period, as of now.
    pub fn gc_plan(&self, grace: Duration) -> Result<GcReport> {
        self.gc_plan_at(SystemTime::now(), grace)
    }

    /// Plan a sweep as of `now` (injectable so tests need not sleep).
    ///
    /// Order matters for a concurrent writer: the object list is taken BEFORE the roots are
    /// read. An object created after the listing is never a candidate; an object that is
    /// listed and becomes referenced after the roots were read was written recently, so the
    /// grace period keeps it.
    pub fn gc_plan_at(&self, now: SystemTime, grace: Duration) -> Result<GcReport> {
        let cache = if self.use_reach_cache { self.load_reach_cache() } else { None };
        let mut r = self.gc_plan_known(now, grace, &mut Known::new(), cache.as_ref())?;
        r.snapshot = None;
        Ok(r)
    }

    /// Turn the reachability cache (`gc-reach`) on or off for gc (default on). Results are identical either way.
    pub fn set_reach_cache(&mut self, on: bool) {
        self.use_reach_cache = on;
    }

    fn gc_plan_known(&self, now: SystemTime, grace: Duration, known: &mut Known, cache: Option<&ReachCache>) -> Result<GcReport> {
        let ids = self.store.list()?;
        let mut roots = self.gc_roots(&ids, known, cache)?;
        roots.sort_unstable();
        roots.dedup();
        // The cache is usable when every root of the sweep that wrote it is still a root: then its marked
        // set is the closure of a subset of today's roots, and only the other roots need walking.
        let cache = cache.filter(|c| c.roots_within(&roots));
        let is_cached = |id: &ObjectId| cache.is_some_and(|c| c.marked(id));
        let all_roots = roots.clone();
        let mut missing = 0usize;
        let mut marked: std::collections::HashSet<ObjectId> = std::collections::HashSet::new();
        // Mark level by level; each level's objects are read (and hash-verified) on several threads.
        let mut level: Vec<ObjectId> = roots;
        while !level.is_empty() {
            level.sort_unstable();
            level.dedup();
            level.retain(|id| !marked.contains(id) && !is_cached(id));
            let todo: Vec<ObjectId> = level.iter().filter(|id| !known.contains_key(*id)).copied().collect();
            let got = par_map(&todo, |id| match self.store.get(id) {
                Ok(o) => Ok(Some(Info::of(&o))),
                Err(vcs_store::StoreError::NotFound(_)) => Ok(None),
                Err(e) => Err(crate::Error::from(e)),
            });
            let mut absent = std::collections::HashSet::new();
            for (id, r) in todo.iter().zip(got) {
                match r? {
                    Some(info) => {
                        known.insert(*id, info);
                    }
                    None => {
                        absent.insert(*id);
                    }
                }
            }
            let mut next = Vec::new();
            for id in &level {
                match known.get(id) {
                    Some(info) => {
                        marked.insert(*id);
                        next.extend_from_slice(&info.kids);
                    }
                    None => {
                        debug_assert!(absent.contains(id));
                        missing += 1;
                    }
                }
            }
            level = next;
        }
        let mut rep = GcReport {
            total: ids.len(),
            missing_referenced: missing,
            ..Default::default()
        };
        let mut snap = Snapshot {
            ids: Vec::with_capacity(ids.len()),
            marked: Vec::with_capacity(ids.len()),
            tx: Vec::with_capacity(ids.len()),
            roots: all_roots,
            missing,
        };
        let mut sorted = ids;
        sorted.sort_unstable();
        for id in sorted {
            let is_marked = marked.contains(&id) || is_cached(&id);
            snap.ids.push(id);
            snap.marked.push(is_marked);
            snap.tx
                .push(known.get(&id).map_or_else(|| cache.is_some_and(|c| c.is_tx(&id)), |i| i.tx));
            if is_marked {
                rep.reachable += 1;
                continue;
            }
            let old_enough = match self.store.modified(&id)? {
                Some(t) => now.duration_since(t).map(|age| age >= grace).unwrap_or(false),
                None => false, // unknown age: treat as new
            };
            if old_enough {
                rep.would_delete.push((id, self.store.get(&id)?.kind()));
            } else {
                rep.kept_young.push(id);
            }
        }
        rep.snapshot = Some(snap);
        Ok(rep)
    }

    /// The roots that live in one worktree's own state directory.
    fn worktree_roots(&self) -> Result<Vec<ObjectId>> {
        let mut roots = Vec::new();
        for e in self.index()?.entries.values() {
            roots.push(e.id);
        }
        roots.extend(self.stash_roots()?);
        // A stopped rebase / cherry-pick / revert names commits that no ref or log may reach yet.
        // Read the raw files even when `rebase_state` would call them stale: keeping is free.
        if let Some(rs) = self.rebase_state_raw()? {
            roots.extend([rs.orig, rs.cursor, rs.current]);
            roots.extend(rs.todo_ids());
        }
        if let Some((_, src)) = self.pick_marker()? {
            roots.push(src);
        }
        if let Some(m) = self.merge_state()? {
            roots.push(m.ours);
            roots.push(m.theirs);
            for c in &m.conflicts {
                roots.extend([c.base, c.left, c.right].into_iter().flatten());
                roots.push(c.placeholder);
            }
        }
        Ok(roots)
    }

    /// Everything that pins objects locally. See docs/gc.md for why each one is a root.
    fn gc_roots(&self, all: &[ObjectId], known: &mut Known, cache: Option<&ReachCache>) -> Result<Vec<ObjectId>> {
        let mut roots = Vec::new();
        let refs = self.refs()?;
        roots.extend(refs.branches.values().copied());
        roots.extend(refs.snapshots.values().copied());
        roots.extend(refs.tags.values().copied());
        let rrefs = self.remote_refs()?;
        roots.extend(rrefs.branches.values().copied());
        roots.extend(rrefs.snapshots.values().copied());
        roots.extend(self.op_head()?); // the operation chain (and every view in it) hangs off this
                                       // Every worktree's own state pins objects: its index, stash and unfinished merge / rebase.
        for wt in self.worktree_views()? {
            roots.extend(wt.worktree_roots()?);
        }
        // Transactions are not reachable from commits and there is no local transaction log yet,
        // so every stored Transaction is a root until the remote/tx-log design lands.
        // Reading each object here also records its children, so the mark phase need not read it again.
        // Objects the cache has scanned before need no read: it knows whether they are transactions.
        let todo: Vec<ObjectId> = all
            .iter()
            .filter(|id| !known.contains_key(*id) && !cache.is_some_and(|c| c.scanned(id)))
            .copied()
            .collect();
        let got = par_map(&todo, |id| self.store.get(id).ok().map(|o| Info::of(&o)));
        for (id, info) in todo.into_iter().zip(got) {
            if let Some(info) = info {
                known.insert(id, info);
            }
        }
        roots.extend(all.iter().filter(|id| known.get(*id).is_some_and(|i| i.tx)).copied());
        if let Some(c) = cache {
            let present: std::collections::HashSet<&ObjectId> = all.iter().collect();
            roots.extend(c.txs().into_iter().filter(|t| present.contains(t) && !known.contains_key(t)));
        }
        Ok(roots)
    }
}

/// What one read of an object tells gc: whether it is a transaction and which objects it names.
struct Info {
    tx: bool,
    kids: Box<[ObjectId]>,
}

impl Info {
    fn of(o: &Object) -> Info {
        let mut kids = Vec::new();
        children(o, &mut kids);
        Info {
            tx: matches!(o, Object::Transaction(_)),
            kids: kids.into_boxed_slice(),
        }
    }
}

/// Objects already read during one gc run (content-addressed, so a result never goes stale).
type Known = std::collections::HashMap<ObjectId, Info>;

/// `f` over `items` on several threads (small inputs stay on the caller), results in order.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = if items.len() < 256 {
        1
    } else {
        std::thread::available_parallelism().map_or(1, |n| n.get()).min(16)
    };
    if threads == 1 {
        return items.iter().map(f).collect();
    }
    let per = items.len().div_ceil(threads);
    std::thread::scope(|sc| {
        let f = &f;
        let hs: Vec<_> = items
            .chunks(per)
            .map(|ch| sc.spawn(move || ch.iter().map(f).collect::<Vec<_>>()))
            .collect();
        hs.into_iter().flat_map(|h| h.join().expect("gc worker panicked")).collect()
    })
}

pub(crate) fn children(o: &Object, out: &mut Vec<ObjectId>) {
    match o {
        Object::Blob(_) => {}
        Object::Chunked(m) => out.extend(m.chunks.iter().map(|c| c.id)),
        Object::Tree(t) => out.extend(t.entries().iter().map(|e| e.id)),
        Object::Commit(c) => {
            out.extend(c.parents.iter().copied());
            out.push(c.root);
        }
        Object::Snapshot(s) => out.extend(s.zones.values().copied()),
        Object::Transaction(t) => {
            for u in &t.updates {
                out.extend(u.old);
                out.push(u.new);
            }
        }
        Object::Tag(t) => out.push(t.target),
        Object::Operation(op) => {
            out.extend(op.parents.iter().copied());
            out.extend(op.view.branches.values().copied());
            out.extend(op.view.tags.iter().flat_map(|t| t.values().copied()));
            out.extend(op.view.tags_delta.iter().flatten().filter_map(|(_, v)| *v));
        }
    }
}
