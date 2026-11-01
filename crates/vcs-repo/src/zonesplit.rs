//! `Repo::zone_split`: turn a directory of the root zone into a new zone without rewriting
//! history. See docs/zones-autodetect.md ("What apply does").
//!
//! Two new commits are made and nothing existing is changed or deleted:
//!
//! * the root zone's checked-out branch gets one commit whose tree is the old tree minus the
//!   directory (parent = the old head, so the old history, including the directory's, stays
//!   reachable there);
//! * the new zone's `main` gets a root commit whose tree is the directory's tree. File and
//!   tree objects are shared with the old commit, so no file data is copied or re-hashed.
//!
//! The working tree is not touched (the same files stay on disk, now owned by the new zone).
//! The zone's history starts at the split: `vcs log <new zone>` shows one commit.

use crate::error::{msg, Result};
use crate::meta::{write_atomic, DEFAULT_BRANCH, ROOT_ZONE};
use crate::repo::{fresh_change_id, Repo};
use crate::treeops;
use vcs_objects::names::validate_zone_name;
use vcs_objects::{Commit, Object, ObjectId, RefUpdate, Signature, Transaction};
use vcs_store::ObjectStore;

/// What a split did (or, with `dry_run`, would do).
#[derive(Debug)]
pub struct SplitReport {
    pub zone: String,
    pub path: String,
    pub files: usize,
    pub root_commit: Option<ObjectId>,
    pub zone_commit: Option<ObjectId>,
}

impl Repo {
    /// Make `path` (a directory of the root zone) a new zone `name`. Refuses unless the root zone
    /// is clean with a checked-out branch, has no sparse set, no merge/rebase is in progress, the
    /// directory holds committed files, and no other zone overlaps it.
    pub fn zone_split(&self, name: &str, path: &str, author: Signature, dry_run: bool) -> Result<SplitReport> {
        let mut v = self.zone_split_many(&[(name.to_owned(), path.to_owned())], author, dry_run)?;
        Ok(v.remove(0))
    }

    /// Split several directories out of the root zone in one go: one lock, one status check, one
    /// flatten of the root tree, one root commit that removes all of them, one commit per new zone,
    /// one transaction and one write of the zone, workspace and ref files. With a single entry the
    /// result is exactly what [`Repo::zone_split`] always produced.
    pub fn zone_split_many(&self, splits: &[(String, String)], author: Signature, dry_run: bool) -> Result<Vec<SplitReport>> {
        if splits.is_empty() {
            return msg("nothing to split");
        }
        for (name, _) in splits {
            validate_zone_name(name)?;
        }
        let _l = self.lock()?;
        self.rebase_guard("zone split")?;
        let mut zones = self.zones()?;
        let mut ws = self.workspace()?;
        let mut refs = self.refs()?;
        let Some(branch) = ws.subs.get(ROOT_ZONE).cloned() else {
            return msg("the root zone is not subscribed in this workspace; subscribe it first");
        };
        if ws.sparse.contains_key(ROOT_ZONE) {
            return msg("the root zone has a sparse set; run `vcs sparse disable root` first");
        }
        if self.merge_state()?.is_some() {
            return msg("a merge is in progress; finish or abort it first");
        }
        let st = self.status_tracked()?;
        self.require_clean(ROOT_ZONE, &st)?;
        let Some(head) = refs.head(ROOT_ZONE, &branch) else {
            return msg("the root zone has no commits yet");
        };
        let items: Vec<(&str, String)> = splits.iter().map(|(n, p)| (n.as_str(), p.trim_matches('/').to_owned())).collect();
        for (name, path) in &items {
            zones.create(name, path)?; // validates the path and non-overlap (also against earlier entries)
        }
        let root_tree = self.commit_obj(&head)?.root;
        let flat = treeops::flatten(&self.store, Some(root_tree), "")?;
        let prefixes: Vec<String> = items.iter().map(|(_, p)| format!("{p}/")).collect();
        let mut reps = Vec::new();
        let mut moved: Vec<Vec<(&str, _, _)>> = Vec::new();
        for ((name, path), prefix) in items.iter().zip(&prefixes) {
            let m: Vec<(&str, _, _)> = flat
                .iter()
                .filter(|(p, _)| p.starts_with(prefix.as_str()))
                .map(|(p, (m, id))| (&p[prefix.len()..], *m, *id))
                .collect();
            if m.is_empty() {
                return msg(format!(
                    "no committed files under {path:?} in the root zone's branch {branch}; nothing to split"
                ));
            }
            reps.push(SplitReport {
                zone: (*name).to_owned(),
                path: path.clone(),
                files: m.len(),
                root_commit: None,
                zone_commit: None,
            });
            moved.push(m);
        }
        if dry_run {
            return Ok(reps);
        }
        let new_root_tree = treeops::build(
            &self.store,
            flat.iter()
                .filter(|(p, _)| !prefixes.iter().any(|x| p.starts_with(x.as_str())))
                .map(|(p, (m, id))| (p.as_str(), *m, *id)),
        )?;
        let msg_root = match items.as_slice() {
            [(name, path)] => format!("split {path}/ out of the root zone into zone {name} (history is not rewritten)"),
            many => format!(
                "split {} directories out of the root zone into zones {} (history is not rewritten)",
                many.len(),
                many.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
            ),
        };
        let rc = Commit {
            zone: ROOT_ZONE.to_owned(),
            parents: vec![head],
            root: new_root_tree,
            change_id: fresh_change_id(ROOT_ZONE, &new_root_tree, Some(&head), &author),
            author: author.clone(),
            message: msg_root.clone(),
        };
        let rid = self.store.put(&Object::Commit(rc))?;
        let mut updates = vec![RefUpdate {
            zone: ROOT_ZONE.to_owned(),
            branch: branch.clone(),
            old: Some(head),
            new: rid,
        }];
        for (((name, path), m), rep) in items.iter().zip(moved).zip(reps.iter_mut()) {
            let zone_tree = treeops::build(&self.store, m)?;
            let zc = Commit {
                zone: (*name).to_owned(),
                parents: vec![],
                root: zone_tree,
                change_id: fresh_change_id(name, &zone_tree, None, &author),
                author: author.clone(),
                message: format!(
                    "start of zone {name}: {path}/ as of root commit {} (earlier history stays in the root zone)",
                    head.short()
                ),
            };
            let zid = self.store.put(&Object::Commit(zc))?;
            updates.push(RefUpdate {
                zone: (*name).to_owned(),
                branch: DEFAULT_BRANCH.to_owned(),
                old: None,
                new: zid,
            });
            rep.root_commit = Some(rid);
            rep.zone_commit = Some(zid);
        }
        updates.sort();
        self.store.put(&Object::Transaction(Transaction {
            updates: updates.clone(),
            author: author.clone(),
            message: msg_root.clone(),
            lock_ops: vec![],
        }))?;
        refs.apply(&updates)?;
        self.store.sync()?;
        // Order: zones and workspace first, refs last (the linearization point). A crash before
        // the refs write leaves the new zones subscribed but empty, so the files show as staged
        // additions of the new zones; `vcs commit` then reaches the intended state.
        write_atomic(&self.common.join("zones"), &zones.encode())?;
        for (name, _) in &items {
            ws.subs.insert((*name).to_owned(), DEFAULT_BRANCH.to_owned());
        }
        self.save_ws(&ws)?;
        write_atomic(&self.common.join("refs"), &refs.encode())?;
        let what = items.iter().map(|(n, p)| format!("{n} at {p}")).collect::<Vec<_>>().join(", ");
        self.record_op(&format!("zone split {what}"), Some(author))?;
        Ok(reps)
    }
}
