//! Day-to-day management: snapshot checkout, area rename/delete, branch delete.
//!
//! Every method takes the repository lock, validates everything before the first write, moves
//! the worktree/index before `refs`/`workspace`, and ends with one operation-log entry.
//! None of them deletes an object, and none touches staged or unstaged work: a dirty area makes
//! the command refuse.

use crate::error::{msg, Error, Result};
use crate::meta::{write_atomic, ROOT_AREA};
use crate::repo::Repo;
use crate::treeops;
use std::collections::BTreeMap;
use vcs_objects::names::validate_branch_name;
use vcs_objects::{Object, ObjectId, RefUpdate};
use vcs_store::ObjectStore;

/// Outcome of [`Repo::snapshot_checkout`].
#[derive(Debug, Default)]
pub struct SnapshotCheckoutReport {
    /// `(area, branch, commit)` areas that moved to the snapshot's commit.
    pub switched: Vec<(String, String, ObjectId)>,
    /// Areas whose checked-out branch already pointed at the pinned commit.
    pub unchanged: Vec<String>,
    /// Pinned areas that are not subscribed in this workspace; left untouched.
    pub skipped: Vec<String>,
}

/// Name of the branch a snapshot checkout creates in each area.
pub fn snapshot_branch(snapshot: &str) -> String {
    format!("snapshot/{snapshot}")
}

impl Repo {
    /// Restore every subscribed area to the commit pinned by snapshot `name`.
    ///
    /// Branches are never moved backwards: each moving area gets a branch `snapshot/<name>` at the
    /// pinned commit and that branch is checked out, so the previous branch keeps its commits.
    /// Refuses (changing nothing) if any area that would change is dirty or mid-merge, if a pinned
    /// area no longer exists or its commit is missing locally, or if `snapshot/<name>` already
    /// points somewhere else. Pinned areas that are not subscribed are reported, not materialized.
    pub fn snapshot_checkout(&self, name: &str) -> Result<SnapshotCheckoutReport> {
        let _l = self.lock()?;
        let areas = self.areas()?;
        let mut ws = self.workspace()?;
        let mut refs = self.refs()?;
        let (_, snap) = self.snapshot_get(name)?;
        let target_branch = snapshot_branch(name);
        validate_branch_name(&target_branch)?;
        let mut rep = SnapshotCheckoutReport::default();
        let mut plan: Vec<(String, ObjectId)> = Vec::new();
        for (a, c) in &snap.areas {
            if !areas.exists(a) {
                return msg(format!("cannot check out snapshot {name:?}: area {a:?} no longer exists"));
            }
            match self.store.get(c) {
                Ok(Object::Commit(cm)) if &cm.area == a => {}
                _ => {
                    return msg(format!(
                        "cannot check out snapshot {name:?}: commit {c} for area {a:?} is missing locally (fetch it first)"
                    ))
                }
            }
            let Some(cur) = ws.subs.get(a) else {
                rep.skipped.push(a.clone());
                continue;
            };
            if refs.head(a, cur) == Some(*c) {
                rep.unchanged.push(a.clone());
                continue;
            }
            match refs.head(a, &target_branch) {
                Some(h) if h != *c => {
                    return msg(format!(
                        "branch {a}/{target_branch} already exists at {}, not at the snapshot's {}; delete or rename it first",
                        h.short(),
                        c.short()
                    ))
                }
                _ => {}
            }
            plan.push((a.clone(), *c));
        }
        if !plan.is_empty() {
            let st = self.status()?;
            for (a, _) in &plan {
                self.require_clean(a, &st)?;
            }
        }
        let mut ix = self.index()?;
        for (a, c) in &plan {
            let old = self.head_flat(a, &areas, &ws, &refs)?;
            let new = treeops::flatten(&self.store, Some(self.commit_obj(c)?.root), areas.path_of(a).unwrap_or(""))?;
            self.transition(a, &areas, &old, &new, &mut ix)?;
        }
        if plan.is_empty() {
            return Ok(rep);
        }
        let updates: Vec<RefUpdate> = plan
            .iter()
            .filter(|(a, _)| refs.head(a, &target_branch).is_none())
            .map(|(a, c)| RefUpdate {
                area: a.clone(),
                branch: target_branch.clone(),
                old: None,
                new: *c,
            })
            .collect();
        refs.apply(&updates)?;
        for (a, c) in &plan {
            ws.subs.insert(a.clone(), target_branch.clone());
            rep.switched.push((a.clone(), target_branch.clone(), *c));
        }
        self.save_index(&mut ix)?;
        write_atomic(&self.vcs.join("refs"), &refs.encode())?;
        self.save_ws(&ws)?;
        self.record_op(&format!("snapshot checkout {name}"), None)?;
        Ok(rep)
    }

    /// Rename an area. Commits embed their area name and are immutable, so only an area with no
    /// history (no local or remote-tracking branches, not pinned by any snapshot) can be renamed.
    /// Its path, and therefore every workspace file, stays where it is.
    pub fn area_rename(&self, old: &str, new: &str) -> Result<()> {
        let _l = self.lock()?;
        let mut areas = self.areas()?;
        let mut ws = self.workspace()?;
        let refs = self.refs()?;
        if !areas.exists(old) {
            return msg(format!("no such area {old:?}"));
        }
        areas.rename(old, new)?;
        if !refs.branches_of(old).is_empty() || !self.remote_refs()?.branches_of(old).is_empty() {
            return msg(format!(
                "area {old:?} has branches; commits record their area name and are immutable, so only an area without history can be renamed"
            ));
        }
        if let Some(s) = self.snapshots_pinning(&refs, old)?.first() {
            return msg(format!("area {old:?} is pinned by snapshot {s:?}; it cannot be renamed"));
        }
        if self.merge_state()?.is_some_and(|m| m.area == old) {
            return msg(format!("area {old:?} is in the middle of a merge"));
        }
        write_atomic(&self.vcs.join("areas"), &areas.encode())?;
        if let Some(b) = ws.subs.remove(old) {
            ws.subs.insert(new.to_owned(), b);
        }
        self.save_ws(&ws)?;
        self.record_op(&format!("area rename {old} {new}"), None)?;
        Ok(())
    }

    /// Delete an area definition. Its paths fall back to the root area.
    ///
    /// Refuses while the area has uncommitted changes. If it is subscribed its tracked files are
    /// removed from the worktree (they are committed, so nothing is lost; untracked files stay).
    /// An area that has branches or is pinned by a snapshot needs `force`: the refs are dropped,
    /// the commits stay in the object store and in the operation log.
    pub fn area_delete(&self, area: &str, force: bool) -> Result<()> {
        let _l = self.lock()?;
        let mut areas = self.areas()?;
        let mut ws = self.workspace()?;
        let mut refs = self.refs()?;
        if area == ROOT_AREA {
            return msg("the root area cannot be deleted");
        }
        if !areas.exists(area) {
            return msg(format!("no such area {area:?}"));
        }
        let branches = refs.branches_of(area);
        let pins = self.snapshots_pinning(&refs, area)?;
        if !force && (!branches.is_empty() || !pins.is_empty()) {
            return msg(format!(
                "area {area:?} has {} branch(es) and is pinned by {} snapshot(s); pass --force to drop its refs (commits are kept in the object store and operation log)",
                branches.len(),
                pins.len()
            ));
        }
        let mut ix = self.index()?;
        if ws.subs.contains_key(area) {
            let st = self.status()?;
            self.require_clean(area, &st)?;
            let old = self.head_flat(area, &areas, &ws, &refs)?;
            self.transition(area, &areas, &old, &treeops::Flat::new(), &mut ix)?;
            self.save_index(&mut ix)?;
        }
        areas.remove(area)?;
        refs.branches.retain(|(a, _), _| a != area);
        ws.subs.remove(area);
        write_atomic(&self.vcs.join("refs"), &refs.encode())?;
        write_atomic(&self.vcs.join("areas"), &areas.encode())?;
        self.save_ws(&ws)?;
        self.record_op(&format!("area delete {area}"), None)?;
        Ok(())
    }

    /// Delete a local branch. Refuses the checked-out branch, and (without `force`) a branch whose
    /// head is not reachable from another branch of the area or from a snapshot, since deleting it
    /// would leave its commits without a name. Returns the commit the branch pointed at.
    pub fn branch_delete(&self, area: &str, branch: &str, force: bool) -> Result<ObjectId> {
        validate_branch_name(branch)?;
        let _l = self.lock()?;
        let areas = self.areas()?;
        let ws = self.workspace()?;
        let mut refs = self.refs()?;
        if !areas.exists(area) {
            return msg(format!("no such area {area:?}"));
        }
        let Some(head) = refs.head(area, branch) else {
            return msg(format!("no such branch {area}/{branch}"));
        };
        if ws.subs.get(area).map(String::as_str) == Some(branch) {
            return msg(format!("cannot delete {area}/{branch}: it is the checked-out branch; switch first"));
        }
        if self.merge_state()?.is_some_and(|m| m.area == area) {
            return msg(format!(
                "area {area:?} is in the middle of a merge; resolve or `vcs merge --abort` first"
            ));
        }
        if !force {
            let mut kept = false;
            for (b, h) in refs.branches_of(area) {
                if b != branch && self.is_ancestor(head, h)? {
                    kept = true;
                    break;
                }
            }
            if !kept {
                for sid in refs.snapshots.values() {
                    if let Object::Snapshot(s) = self.store.get(sid)? {
                        if let Some(c) = s.areas.get(area) {
                            if self.is_ancestor(head, *c)? {
                                kept = true;
                                break;
                            }
                        }
                    }
                }
            }
            if !kept {
                return msg(format!(
                    "branch {area}/{branch} ({}) has commits not reachable from any other branch or snapshot; merge it, or pass --force to delete anyway (the commit stays in the operation log)",
                    head.short()
                ));
            }
        }
        refs.branches.remove(&(area.to_owned(), branch.to_owned()));
        write_atomic(&self.vcs.join("refs"), &refs.encode())?;
        self.record_op(&format!("delete branch {area}/{branch}"), None)?;
        Ok(head)
    }

    /// Names of snapshots that pin a commit of `area`.
    fn snapshots_pinning(&self, refs: &crate::meta::Refs, area: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let ids: BTreeMap<&String, &ObjectId> = refs.snapshots.iter().collect();
        for (n, id) in ids {
            match self.store.get(id)? {
                Object::Snapshot(s) if s.areas.contains_key(area) => out.push(n.clone()),
                Object::Snapshot(_) => {}
                _ => return Err(Error::Corrupt(format!("{id} is not a snapshot"))),
            }
        }
        Ok(out)
    }
}
