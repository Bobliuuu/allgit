//! Garbage collection, dry-run only. Design and safety argument: docs/gc.md.
//!
//! `gc_plan` computes what a sweep *would* delete and deletes nothing. A destructive sweep must
//! not be added before the lease/touch prerequisites in docs/gc.md exist.

use crate::error::Result;
use crate::repo::Repo;
use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};
use vcs_objects::{Kind, Object, ObjectId};
use vcs_store::ObjectStore;

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

impl Repo {
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
        for e in self.index()?.entries.values() {
            roots.push(e.id);
        }
        if let Some(m) = self.merge_state()? {
            roots.push(m.ours);
            roots.push(m.theirs);
            for c in &m.conflicts {
                roots.extend([c.base, c.left, c.right].into_iter().flatten());
                roots.push(c.placeholder);
            }
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
        }
    }
}
