//! Day-to-day management: snapshot checkout, zone rename/delete, branch delete.
//!
//! Every method takes the repository lock, validates everything before the first write, moves
//! the worktree/index before `refs`/`workspace`, and ends with one operation-log entry.
//! None of them deletes an object, and none touches staged or unstaged work: a dirty zone makes
//! the command refuse.

use crate::error::{msg, Error, Result};
use crate::meta::{write_atomic, ROOT_ZONE};
use crate::repo::Repo;
use crate::treeops;
use std::collections::BTreeMap;
use vcs_objects::names::validate_branch_name;
use vcs_objects::{Object, ObjectId, RefUpdate};
use vcs_store::ObjectStore;

/// Outcome of [`Repo::snapshot_checkout`].
#[derive(Debug, Default)]
pub struct SnapshotCheckoutReport {
    /// `(zone, branch, commit)` zones that moved to the snapshot's commit.
    pub switched: Vec<(String, String, ObjectId)>,
    /// Zones whose checked-out branch already pointed at the pinned commit.
    pub unchanged: Vec<String>,
    /// Pinned zones that are not subscribed in this workspace; left untouched.
    pub skipped: Vec<String>,
}

/// Name of the branch a snapshot checkout creates in each zone.
pub fn snapshot_branch(snapshot: &str) -> String {
    format!("snapshot/{snapshot}")
}

impl Repo {
    /// Restore every subscribed zone to the commit pinned by snapshot `name`.
    ///
    /// Branches are never moved backwards: each moving zone gets a branch `snapshot/<name>` at the
    /// pinned commit and that branch is checked out, so the previous branch keeps its commits.
    /// Refuses (changing nothing) if any zone that would change is dirty or mid-merge, if a pinned
    /// zone no longer exists or its commit is missing locally, or if `snapshot/<name>` already
    /// points somewhere else. Pinned zones that are not subscribed are reported, not materialized.
    pub fn snapshot_checkout(&self, name: &str) -> Result<SnapshotCheckoutReport> {
        let _l = self.lock()?;
        let zones = self.zones()?;
        let mut ws = self.workspace()?;
        let mut refs = self.refs()?;
        let (_, snap) = self.snapshot_get(name)?;
        let target_branch = snapshot_branch(name);
        validate_branch_name(&target_branch)?;
        let mut rep = SnapshotCheckoutReport::default();
        let mut plan: Vec<(String, ObjectId)> = Vec::new();
        for (a, c) in &snap.zones {
            if !zones.exists(a) {
                return msg(format!("cannot check out snapshot {name:?}: zone {a:?} no longer exists"));
            }
            match self.store.get(c) {
                Ok(Object::Commit(cm)) if &cm.zone == a => {}
                _ => {
                    return msg(format!(
                        "cannot check out snapshot {name:?}: commit {c} for zone {a:?} is missing locally (fetch it first)"
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
            let old = self.head_flat(a, &zones, &ws, &refs)?;
            let new = treeops::flatten(&self.store, Some(self.commit_obj(c)?.root), zones.path_of(a).unwrap_or(""))?;
            self.transition(a, &zones, &old, &new, &mut ix)?;
        }
        if plan.is_empty() {
            return Ok(rep);
        }
        let updates: Vec<RefUpdate> = plan
            .iter()
            .filter(|(a, _)| refs.head(a, &target_branch).is_none())
            .map(|(a, c)| RefUpdate {
                zone: a.clone(),
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

    /// Rename a zone, with or without history.
    ///
    /// Commits embed their zone name and are immutable, so the name a commit carries becomes the
    /// zone's permanent **id**; a rename only changes the zone's display name (a label stored in
    /// the `zones` file). Branches, snapshots, the workspace, the operation log, the index and
    /// the remote manifest stay keyed by id, so nothing else is rewritten and no commit id changes.
    /// Either the old display name or the id may be used to refer to the zone afterwards.
    /// Refuses a dirty or mid-merge zone, the root zone and names already in use.
    pub fn zone_rename(&self, old: &str, new: &str) -> Result<()> {
        let _l = self.lock()?;
        let mut zones = self.zones()?;
        let id = zones.rename(old, new)?;
        if self.merge_state()?.is_some_and(|m| m.zone == id) {
            return msg(format!("zone {old:?} is in the middle of a merge"));
        }
        if self.workspace()?.subs.contains_key(&id) {
            let st = self.status()?;
            self.require_clean(&id, &st)?;
        }
        write_atomic(&self.vcs.join("zones"), &zones.encode())?;
        self.record_op(&format!("zone rename {old} {new}"), None)?;
        Ok(())
    }

    /// Delete a zone definition. Its paths fall back to the root zone.
    ///
    /// Refuses while the zone has uncommitted changes. If it is subscribed its tracked files are
    /// removed from the worktree (they are committed, so nothing is lost; untracked files stay).
    /// A zone that has branches or is pinned by a snapshot needs `force`: the refs are dropped,
    /// the commits stay in the object store and in the operation log.
    pub fn zone_delete(&self, zone: &str, force: bool) -> Result<()> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        let _l = self.lock()?;
        let mut zones = self.zones()?;
        let mut ws = self.workspace()?;
        let mut refs = self.refs()?;
        if zone == ROOT_ZONE {
            return msg("the root zone cannot be deleted");
        }
        if !zones.exists(zone) {
            return msg(format!("no such zone {zone:?}"));
        }
        let branches = refs.branches_of(zone);
        let pins = self.snapshots_pinning(&refs, zone)?;
        let ntags = refs.tags.keys().filter(|(a, _)| a == zone).count();
        if !force && (!branches.is_empty() || !pins.is_empty() || ntags > 0) {
            return msg(format!(
                "zone {zone:?} has {} branch(es), {} tag(s) and is pinned by {} snapshot(s); pass --force to drop its refs (commits are kept in the object store and operation log)",
                branches.len(),
                ntags,
                pins.len()
            ));
        }
        let mut ix = self.index()?;
        if ws.subs.contains_key(zone) {
            let st = self.status()?;
            self.require_clean(zone, &st)?;
            let old = self.head_flat(zone, &zones, &ws, &refs)?;
            self.transition(zone, &zones, &old, &treeops::Flat::new(), &mut ix)?;
            self.save_index(&mut ix)?;
        }
        zones.remove(zone)?;
        refs.branches.retain(|(a, _), _| a != zone);
        refs.tags.retain(|(a, _), _| a != zone);
        ws.subs.remove(zone);
        ws.sparse.remove(zone);
        write_atomic(&self.vcs.join("refs"), &refs.encode())?;
        write_atomic(&self.vcs.join("zones"), &zones.encode())?;
        self.save_ws(&ws)?;
        self.record_op(&format!("zone delete {zone}"), None)?;
        Ok(())
    }

    /// Delete a local branch. Refuses the checked-out branch, and (without `force`) a branch whose
    /// head is not reachable from another branch of the zone or from a snapshot, since deleting it
    /// would leave its commits without a name. Returns the commit the branch pointed at.
    pub fn branch_delete(&self, zone: &str, branch: &str, force: bool) -> Result<ObjectId> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        validate_branch_name(branch)?;
        let _l = self.lock()?;
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut refs = self.refs()?;
        if !zones.exists(zone) {
            return msg(format!("no such zone {zone:?}"));
        }
        let Some(head) = refs.head(zone, branch) else {
            return msg(format!("no such branch {zone}/{branch}"));
        };
        if ws.subs.get(zone).map(String::as_str) == Some(branch) {
            return msg(format!("cannot delete {zone}/{branch}: it is the checked-out branch; switch first"));
        }
        if self.merge_state()?.is_some_and(|m| m.zone == zone) {
            return msg(format!(
                "zone {zone:?} is in the middle of a merge; resolve or `vcs merge --abort` first"
            ));
        }
        if !force {
            let mut kept = false;
            for (b, h) in refs.branches_of(zone) {
                if b != branch && self.is_ancestor(head, h)? {
                    kept = true;
                    break;
                }
            }
            if !kept {
                for sid in refs.snapshots.values() {
                    if let Object::Snapshot(s) = self.store.get(sid)? {
                        if let Some(c) = s.zones.get(zone) {
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
                    "branch {zone}/{branch} ({}) has commits not reachable from any other branch or snapshot; merge it, or pass --force to delete anyway (the commit stays in the operation log)",
                    head.short()
                ));
            }
        }
        refs.branches.remove(&(zone.to_owned(), branch.to_owned()));
        write_atomic(&self.vcs.join("refs"), &refs.encode())?;
        self.record_op(&format!("delete branch {zone}/{branch}"), None)?;
        Ok(head)
    }

    /// Names of snapshots that pin a commit of `zone`.
    fn snapshots_pinning(&self, refs: &crate::meta::Refs, zone: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let ids: BTreeMap<&String, &ObjectId> = refs.snapshots.iter().collect();
        for (n, id) in ids {
            match self.store.get(id)? {
                Object::Snapshot(s) if s.zones.contains_key(zone) => out.push(n.clone()),
                Object::Snapshot(_) => {}
                _ => return Err(Error::Corrupt(format!("{id} is not a snapshot"))),
            }
        }
        Ok(out)
    }
}
