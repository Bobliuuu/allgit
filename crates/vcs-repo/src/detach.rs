//! Detached HEAD, branch rename and branch creation at a given revision.
//!
//! A zone is *detached* when its workspace entry is the internal name [`DETACHED_BRANCH`] and
//! `refs[(zone, DETACHED_BRANCH)]` holds the commit. Because that is an ordinary ref, everything
//! that already works on "the checked-out branch" works unchanged: `commit` advances it, `merge`,
//! `reset`, `cherry-pick`, `status` and `diff` read it, `gc` keeps its commits, and the operation
//! log (`op log`, `op undo`, `op restore`) records and restores it together with the other refs.
//! The ref is never published, bundled or listed as a branch.
//!
//! Per zone: each zone is detached independently (`switch --detach` acts on one zone, like
//! `switch`). In one repository with several worktrees only one worktree can be detached in a
//! given zone at a time (the second gets the usual "already checked out" refusal).
//!
//! Leaving the detached HEAD (`switch <branch>`) drops the ref; commits that no branch or tag
//! reaches are then only reachable through the operation log, so the CLI lists them first and
//! names the rescue (`vcs branch <name> <id>` before leaving, or `vcs op undo` after).

use crate::error::{msg, Result};
use crate::meta::write_atomic;
use crate::repo::{Repo, StatusOpts};
use crate::treeops;
use vcs_objects::names::{validate_branch_name, DETACHED_BRANCH};
use vcs_objects::{ObjectId, RefUpdate};

impl Repo {
    /// The commit `zone`'s detached HEAD is at, or `None` when the zone is on a branch.
    pub fn detached_head(&self, zone: &str) -> Result<Option<ObjectId>> {
        let zone = self.resolve_zone(zone)?;
        let ws = self.workspace()?;
        if ws.subs.get(&zone).map(String::as_str) != Some(DETACHED_BRANCH) {
            return Ok(None);
        }
        Ok(self.refs()?.detached(&zone))
    }

    /// Check out the commit `rev` names (a branch, tag, commit-id prefix, `HEAD~2`, ...) as a
    /// detached HEAD of `zone`: the working tree and index move to that commit, no branch is
    /// involved. Same safety as `checkout`: refuses with uncommitted changes or a merge in
    /// progress. Returns the commit. Moving from one detached commit to another is allowed.
    pub fn checkout_detached(&self, zone: &str, rev: &str) -> Result<ObjectId> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        let _l = self.lock()?;
        let zones = self.zones()?;
        let mut ws = self.workspace()?;
        let mut refs = self.refs()?;
        let Some(cur) = ws.subs.get(zone).cloned() else {
            return msg(format!("zone {zone:?} is not subscribed in this workspace"));
        };
        let target = self.resolve_rev(zone, rev)?;
        let was_detached = cur == DETACHED_BRANCH;
        if was_detached && refs.detached(zone) == Some(target) {
            return Ok(target);
        }
        if !was_detached {
            self.require_branch_free(zone, DETACHED_BRANCH)?;
        }
        let mut ix = self.index()?;
        let st = self.status_inner(StatusOpts::default(), &ix, false, None)?;
        self.require_clean(zone, &st)?;
        let prefix = zones.path_of(zone).unwrap_or("");
        let old_root = match self.current_head(zone, &ws, &refs) {
            Some(h) => Some(self.commit_obj(&h)?.root),
            None => None,
        };
        let new_root = self.commit_obj(&target)?.root;
        let diff = treeops::diff_trees(&self.store, old_root, Some(new_root), prefix)?;
        self.transition_diff(zone, &zones, &diff, &mut ix)?;
        refs.branches.insert((zone.to_owned(), DETACHED_BRANCH.to_owned()), target);
        ws.subs.insert(zone.to_owned(), DETACHED_BRANCH.to_owned());
        self.save_index(&mut ix)?;
        write_atomic(&self.common.join("refs"), &refs.encode())?;
        self.save_ws(&ws)?;
        self.record_op(&format!("checkout {zone} detached at {}", target.short()), None)?;
        Ok(target)
    }

    /// Create `branch` in `zone` at the commit `rev` names (default: the checked-out head).
    /// Works on a detached HEAD too, which is the way to keep commits made there.
    pub fn create_branch_from(&self, zone: &str, branch: &str, rev: Option<&str>) -> Result<ObjectId> {
        let Some(rev) = rev else {
            return self.create_branch(zone, branch);
        };
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        validate_branch_name(branch)?;
        let target = self.resolve_rev(zone, rev)?;
        let _l = self.lock()?;
        let mut refs = self.refs()?;
        if !self.zones()?.exists(zone) {
            return msg(format!("no such zone {zone:?}"));
        }
        if refs.head(zone, branch).is_some() {
            return msg(format!("branch {zone}/{branch} already exists"));
        }
        refs.apply(&[RefUpdate {
            zone: zone.into(),
            branch: branch.into(),
            old: None,
            new: target,
        }])?;
        write_atomic(&self.common.join("refs"), &refs.encode())?;
        self.record_op(&format!("create branch {zone}/{branch} at {}", target.short()), None)?;
        Ok(target)
    }

    /// Rename branch `old` of `zone` to `new` (git `branch -m`). The ref moves, and the workspace
    /// follows when `old` is the checked-out branch; no file is touched. Refuses a name that
    /// exists (unless `force`, which replaces it; never the checked-out branch), a branch checked
    /// out in another worktree, and a zone with a merge or rebase in progress. Returns the commit.
    pub fn branch_rename(&self, zone: &str, old: &str, new: &str, force: bool) -> Result<Option<ObjectId>> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        validate_branch_name(old)?;
        validate_branch_name(new)?;
        let _l = self.lock()?;
        let mut ws = self.workspace()?;
        let mut refs = self.refs()?;
        if !self.zones()?.exists(zone) {
            return msg(format!("no such zone {zone:?}"));
        }
        // a fresh repository has a checked-out `main` without a commit; renaming it is allowed
        let checked_out = ws.subs.get(zone).map(String::as_str) == Some(old);
        let Some(head) = refs.head(zone, old) else {
            if checked_out && refs.head(zone, new).is_none() {
                ws.subs.insert(zone.to_owned(), new.to_owned());
                self.save_ws(&ws)?;
                self.record_op(&format!("rename branch {zone}/{old} -> {new}"), None)?;
                return Ok(None);
            }
            return msg(format!("no such branch {zone}/{old}"));
        };
        if old == new {
            return Ok(Some(head));
        }
        if self.merge_state()?.is_some_and(|m| m.zone == zone) || self.rebase_state()?.is_some_and(|r| r.zone == zone) {
            return msg(format!(
                "zone {zone:?} is in the middle of a merge or rebase; finish or abort it before renaming a branch"
            ));
        }
        if let Some(wt) = self.branch_holder(zone, old)? {
            if !checked_out {
                return msg(format!(
                    "cannot rename {zone}/{old}: it is checked out in the worktree {}",
                    wt.display()
                ));
            }
        }
        if refs.head(zone, new).is_some() {
            if !force {
                return msg(format!("branch {zone}/{new} already exists; use -M to replace it"));
            }
            if ws.subs.get(zone).map(String::as_str) == Some(new) {
                return msg(format!("cannot replace {zone}/{new}: it is the checked-out branch"));
            }
            if let Some(wt) = self.branch_holder(zone, new)? {
                return msg(format!(
                    "cannot replace {zone}/{new}: it is checked out in the worktree {}",
                    wt.display()
                ));
            }
        }
        refs.branches.remove(&(zone.to_owned(), old.to_owned()));
        refs.branches.insert((zone.to_owned(), new.to_owned()), head);
        write_atomic(&self.common.join("refs"), &refs.encode())?;
        if checked_out {
            ws.subs.insert(zone.to_owned(), new.to_owned());
            self.save_ws(&ws)?;
        }
        self.record_op(&format!("rename branch {zone}/{old} -> {new}"), None)?;
        Ok(Some(head))
    }

    /// Forget the detached HEAD ref of `zone` once the zone is back on a branch. Called with the
    /// lock held by `checkout`.
    pub(crate) fn drop_detached_ref(&self, zone: &str) -> Result<()> {
        let mut refs = self.refs()?;
        if refs.branches.remove(&(zone.to_owned(), DETACHED_BRANCH.to_owned())).is_some() {
            write_atomic(&self.common.join("refs"), &refs.encode())?;
        }
        Ok(())
    }
}
