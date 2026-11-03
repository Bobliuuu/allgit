//! Operation log (Jujutsu-style), distinct from the commit DAG.
//!
//! Every mutating `Repo` operation stores an immutable `Operation` object holding the local
//! view (branch heads + workspace subscriptions) *after* the change, then atomically moves
//! the `op_head` pointer file. Undo/restore only moves local refs/workspace.

use crate::error::{msg, Error, Result};
use crate::meta::{frame, unframe, write_atomic, Workspace};
use crate::repo::Repo;
use crate::treeops;
use crate::StatusOpts;
use std::collections::BTreeSet;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use vcs_objects::{Object, ObjectId, OpView, Operation, Signature};
use vcs_store::ObjectStore;

const OP_HEAD: &str = "op_head";

/// Author for operations recorded by methods that take no explicit author.
fn env_signature() -> Signature {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let name = env("VCS_AUTHOR_NAME").or_else(|| env("USER")).unwrap_or_else(|| "unknown".into());
    let email = env("VCS_AUTHOR_EMAIL").unwrap_or_else(|| format!("{name}@localhost"));
    let timestamp = env("VCS_TIMESTAMP")
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    Signature { name, email, timestamp }
}

impl Repo {
    /// The current operation id, or `None` for a repository that predates the log.
    pub fn op_head(&self) -> Result<Option<ObjectId>> {
        let p = self.common.join(OP_HEAD);
        let bytes = match fs::read(&p) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(Error::Io { path: p, source }),
        };
        let body = unframe(b"AGOH", &bytes, "op_head")?;
        if body.len() != 32 {
            return Err(Error::Corrupt("op_head: bad length".into()));
        }
        let mut a = [0u8; 32];
        a.copy_from_slice(body);
        Ok(Some(ObjectId::from_bytes(a)))
    }

    pub fn op_get(&self, id: &ObjectId) -> Result<Operation> {
        match self.store.get(id)? {
            Object::Operation(o) => Ok(o),
            _ => Err(Error::Corrupt(format!("{id} is not an operation"))),
        }
    }

    /// Local view (branch heads + tags + subscriptions) as currently on disk.
    pub fn current_view(&self) -> Result<OpView> {
        let refs = self.refs()?;
        let ws = self.workspace()?;
        Ok(OpView {
            branches: refs.branches,
            workspace: ws.subs,
            tags: Some(refs.tags),
        })
    }

    /// Store an Operation for the current on-disk view, THEN move `op_head` (atomic rename).
    /// A crash between the two leaves the previous `op_head` valid (the object is just an orphan).
    /// Caller holds the repository lock.
    pub(crate) fn record_op(&self, description: &str, author: Option<Signature>) -> Result<ObjectId> {
        let parents: Vec<ObjectId> = self.op_head()?.into_iter().collect();
        let op = Operation {
            parents,
            view: self.current_view()?,
            description: description.lines().next().unwrap_or("").to_owned(),
            author: author.unwrap_or_else(env_signature),
        };
        let id = self.store.put(&Object::Operation(op))?;
        self.write_op_head(&id)?;
        Ok(id)
    }

    pub(crate) fn write_op_head(&self, id: &ObjectId) -> Result<()> {
        self.store.sync()?; // the Operation object must be durable before op_head names it
        write_atomic(&self.common.join(OP_HEAD), &frame(b"AGOH", id.as_bytes().to_vec()))
    }

    /// Operations newest first, following all parents from `op_head`.
    pub fn op_log(&self, limit: usize) -> Result<Vec<(ObjectId, Operation)>> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        let mut queue: Vec<ObjectId> = self.op_head()?.into_iter().collect();
        while !queue.is_empty() {
            let id = queue.remove(0);
            if out.len() >= limit {
                break;
            }
            if !seen.insert(id) {
                continue;
            }
            let op = self.op_get(&id)?;
            queue.extend(op.parents.iter().copied());
            out.push((id, op));
        }
        Ok(out)
    }

    /// Resolve a hex id prefix among operations in the log.
    pub fn op_resolve(&self, prefix: &str) -> Result<ObjectId> {
        let p = prefix.to_ascii_lowercase();
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_hexdigit()) {
            return msg(format!("{prefix:?} is not a hex operation id prefix"));
        }
        let hits: Vec<ObjectId> = self
            .op_log(usize::MAX)?
            .into_iter()
            .map(|(i, _)| i)
            .filter(|i| i.to_hex().starts_with(&p))
            .collect();
        match hits.as_slice() {
            [one] => Ok(*one),
            [] => msg(format!("no operation matches {prefix:?}")),
            _ => msg(format!("operation prefix {prefix:?} is ambiguous ({} matches)", hits.len())),
        }
    }

    /// Restore the view of the parent of the current operation; recorded as a new operation.
    /// Repeating it toggles (it undoes the undo); use `op_restore` to go further back.
    pub fn op_undo(&self) -> Result<ObjectId> {
        let _l = self.lock()?;
        let Some(head) = self.op_head()? else {
            return msg("no operation log yet; nothing to undo");
        };
        let op = self.op_get(&head)?;
        if op.description.starts_with("zone rename ") {
            return msg(format!(
                "the last operation ({}) renamed a zone; renames are not part of the operation view and cannot be undone. Rename the zone back with `vcs zone rename`",
                op.description
            ));
        }
        let Some(parent) = op.parents.first().copied() else {
            return msg("the current operation is the first one; nothing to undo");
        };
        let target = self.op_get(&parent)?.view;
        self.restore_view(&target)?;
        self.record_op(&format!("undo operation {}", head.short()), None)
    }

    /// Restore the local view as it was right after operation `id`; recorded as a new operation.
    pub fn op_restore(&self, id: &ObjectId) -> Result<ObjectId> {
        let _l = self.lock()?;
        let target = self.op_get(id)?.view;
        self.restore_view(&target)?;
        self.record_op(&format!("restore to operation {}", id.short()), None)
    }

    /// Move local branch refs, subscriptions, index and worktree to `target`.
    /// Never touches remote-tracking refs, the remote, or any object.
    fn restore_view(&self, target: &OpView) -> Result<()> {
        // It rewrites every branch ref at once; another worktree's checked-out branch would move under it.
        self.require_no_linked_worktrees("op undo / op restore")?;
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut refs = self.refs()?;
        // 1. validate the target completely before touching anything
        for ((a, b), c) in &target.branches {
            if !zones.exists(a) {
                return msg(format!("cannot restore: zone {a:?} no longer exists"));
            }
            match self.store.get(c) {
                Ok(Object::Commit(cm)) if &cm.zone == a => {}
                Ok(_) => return msg(format!("cannot restore: {a}/{b} points to {c}, which is not a commit of that zone")),
                Err(_) => {
                    return msg(format!(
                        "cannot restore: commit {c} for {a}/{b} is missing locally (fetch it first); nothing was changed"
                    ))
                }
            }
        }
        for ((a, n), id) in target.tags.iter().flatten() {
            if !zones.exists(a) {
                return msg(format!("cannot restore: zone {a:?} no longer exists"));
            }
            match self.store.get(id) {
                Ok(Object::Commit(cm)) if &cm.zone == a => {}
                Ok(Object::Tag(t)) if &t.zone == a => {}
                Ok(_) => {
                    return msg(format!(
                        "cannot restore: tag {a}/{n} points to {id}, which is not a commit or tag of that zone"
                    ))
                }
                Err(_) => {
                    return msg(format!(
                        "cannot restore: object {id} for tag {a}/{n} is missing locally (fetch it first); nothing was changed"
                    ))
                }
            }
        }
        for a in target.workspace.keys() {
            if !zones.exists(a) {
                return msg(format!("cannot restore: zone {a:?} no longer exists"));
            }
        }
        // 2. which zones change what they materialize?
        let new_head = |a: &str| {
            target
                .workspace
                .get(a)
                .map(|b| (b.clone(), target.branches.get(&(a.to_owned(), b.clone())).copied()))
        };
        let mut affected: Vec<String> = Vec::new();
        for a in ws.subs.keys().chain(target.workspace.keys()).collect::<BTreeSet<_>>() {
            let cur = ws.subs.get(a).map(|b| (b.clone(), refs.head(a, b)));
            if cur != new_head(a) {
                affected.push(a.clone());
            }
        }
        // 3. refuse when an affected, currently subscribed zone is dirty. Only staged/unstaged/merge
        // state matters, so the untracked-file walk is skipped (as `checkout` does).
        let mut ix = self.index()?;
        if !affected.is_empty() {
            let st = self.status_inner(StatusOpts::default(), &ix, false, None)?;
            for a in &affected {
                if ws.subs.contains_key(a) {
                    self.require_clean(a, &st)?;
                }
            }
        }
        // 4. worktree + index first, refs/workspace last. Every zone is validated before any
        // file moves, so a refusal in one zone cannot leave another half-restored. Subtrees that
        // are identical on both sides are never read.
        let mut moves = Vec::new();
        for a in &affected {
            let prefix = zones.path_of(a).unwrap_or("");
            let old_root = if ws.subs.contains_key(a) {
                match self.current_head(a, &ws, &refs) {
                    Some(h) => Some(self.commit_obj(&h)?.root),
                    None => None,
                }
            } else {
                None
            };
            let new_root = match new_head(a) {
                Some((_, Some(h))) => Some(self.commit_obj(&h)?.root),
                _ => None,
            };
            let diff = treeops::diff_trees(&self.store, old_root, new_root, prefix)?;
            self.transition_diff_check(a, &zones, &diff)?;
            moves.push((a, diff));
        }
        for (a, diff) in &moves {
            self.transition_diff_apply(a, diff, &mut ix)?;
        }
        if !affected.is_empty() {
            self.save_index(&mut ix)?;
        }
        refs.branches = target.branches.clone();
        if let Some(tags) = &target.tags {
            refs.tags = tags.clone();
        }
        write_atomic(&self.common.join("refs"), &refs.encode())?;
        // A zone that stays subscribed keeps its sparse set.
        let mut sparse = ws.sparse.clone();
        sparse.retain(|z, _| target.workspace.contains_key(z));
        self.save_ws(&Workspace {
            subs: target.workspace.clone(),
            sparse,
        })
    }

    /// fsck: validate the operation chain.
    pub(crate) fn fsck_ops(&self, problems: &mut Vec<String>) {
        let head = match self.op_head() {
            Ok(h) => h,
            Err(e) => {
                problems.push(format!("op_head: {e}"));
                return;
            }
        };
        let mut seen = BTreeSet::new();
        let mut queue: Vec<ObjectId> = head.into_iter().collect();
        while let Some(id) = queue.pop() {
            if !seen.insert(id) {
                continue;
            }
            let op = match self.store.get(&id) {
                Ok(Object::Operation(o)) => o,
                Ok(_) => {
                    problems.push(format!("operation {id}: not an operation object"));
                    continue;
                }
                Err(e) => {
                    problems.push(format!("operation {id}: missing or corrupt: {e}"));
                    continue;
                }
            };
            queue.extend(op.parents.iter().copied());
            for ((a, b), c) in &op.view.branches {
                match self.store.get(c) {
                    Ok(Object::Commit(cm)) if &cm.zone == a => {}
                    _ => problems.push(format!(
                        "operation {id}: view {a}/{b} -> {c} is missing or not a commit of that zone"
                    )),
                }
            }
        }
    }
}
