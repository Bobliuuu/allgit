//! Garbage collection. Design and safety argument: docs/gc.md.
//!
//! `gc_plan` computes what a sweep *would* delete and deletes nothing. `gc_sweep` is the
//! **destructive** local sweep: under the repository lock it removes unreachable objects older
//! than the grace period, rewrites packs without them and cleans crash leftovers.

use crate::error::Result;
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
        let orphans = self.store.clean_orphans(now, opts.grace)?;
        let first = self.gc_plan_at(now, opts.grace)?;
        // Fresh list + fresh roots + fresh ages, immediately before deleting.
        let second = self.gc_plan_at(now, opts.grace)?;
        let confirmed: BTreeSet<ObjectId> = second.would_delete.iter().map(|x| x.0).collect();
        let doomed: BTreeSet<ObjectId> = first.would_delete.iter().map(|x| x.0).filter(|i| confirmed.contains(i)).collect();
        let deleted: Vec<(ObjectId, Kind)> = second.would_delete.iter().filter(|x| doomed.contains(&x.0)).copied().collect();
        let sweep = self.store.sweep(&doomed)?;
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
        let ids = self.store.list()?;
        let roots = self.gc_roots()?;
        let mut missing = 0usize;
        let mut marked: BTreeSet<ObjectId> = BTreeSet::new();
        let mut stack: Vec<ObjectId> = roots;
        while let Some(id) = stack.pop() {
            if !marked.insert(id) {
                continue;
            }
            match self.store.get(&id) {
                Ok(o) => children(&o, &mut stack),
                Err(vcs_store::StoreError::NotFound(_)) => {
                    marked.remove(&id);
                    missing += 1;
                }
                Err(e) => return Err(e.into()),
            }
        }
        let mut rep = GcReport {
            total: ids.len(),
            missing_referenced: missing,
            ..Default::default()
        };
        for id in ids {
            if marked.contains(&id) {
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
            roots.extend(rs.todo.iter().copied());
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
    fn gc_roots(&self) -> Result<Vec<ObjectId>> {
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
        for id in self.store.list()? {
            if matches!(self.store.get(&id), Ok(Object::Transaction(_))) {
                roots.push(id);
            }
        }
        Ok(roots)
    }
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
        }
    }
}
