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
        validate_zone_name(name)?;
        let _l = self.lock()?;
        self.rebase_guard("zone split")?;
        let mut zones = self.zones()?;
        let mut ws = self.workspace()?;
        let mut refs = self.refs()?;
        let path = path.trim_matches('/');
        let Some(branch) = ws.subs.get(ROOT_ZONE).cloned() else {
            return msg("the root zone is not subscribed in this workspace; subscribe it first");
        };
        if ws.sparse.contains_key(ROOT_ZONE) {
            return msg("the root zone has a sparse set; run `vcs sparse disable root` first");
        }
        if self.merge_state()?.is_some() {
            return msg("a merge is in progress; finish or abort it first");
        }
        let st = self.status()?;
        self.require_clean(ROOT_ZONE, &st)?;
        let Some(head) = refs.head(ROOT_ZONE, &branch) else {
            return msg("the root zone has no commits yet");
        };
        zones.create(name, path)?; // validates the path and non-overlap
        let root_tree = self.commit_obj(&head)?.root;
        let flat = treeops::flatten(&self.store, Some(root_tree), "")?;
        let prefix = format!("{path}/");
        let moved: Vec<(&str, _, _)> = flat
            .iter()
            .filter(|(p, _)| p.starts_with(&prefix))
            .map(|(p, (m, id))| (&p[prefix.len()..], *m, *id))
            .collect();
        if moved.is_empty() {
            return msg(format!(
                "no committed files under {path:?} in the root zone's branch {branch}; nothing to split"
            ));
        }
        let mut rep = SplitReport {
            zone: name.to_owned(),
            path: path.to_owned(),
            files: moved.len(),
            root_commit: None,
            zone_commit: None,
        };
        if dry_run {
            return Ok(rep);
        }
        let new_root_tree = treeops::build(
            &self.store,
            flat.iter()
                .filter(|(p, _)| !p.starts_with(&prefix))
                .map(|(p, (m, id))| (p.as_str(), *m, *id)),
        )?;
        let zone_tree = treeops::build(&self.store, moved)?;
        let msg_root = format!("split {path}/ out of the root zone into zone {name} (history is not rewritten)");
        let rc = Commit {
            zone: ROOT_ZONE.to_owned(),
            parents: vec![head],
            root: new_root_tree,
            change_id: fresh_change_id(ROOT_ZONE, &new_root_tree, Some(&head), &author),
            author: author.clone(),
            message: msg_root.clone(),
        };
        let zc = Commit {
            zone: name.to_owned(),
            parents: vec![],
            root: zone_tree,
            change_id: fresh_change_id(name, &zone_tree, None, &author),
            author: author.clone(),
            message: format!(
                "start of zone {name}: {path}/ as of root commit {} (earlier history stays in the root zone)",
                head.short()
            ),
        };
        let rid = self.store.put(&Object::Commit(rc))?;
        let zid = self.store.put(&Object::Commit(zc))?;
        let mut updates = vec![
            RefUpdate {
                zone: ROOT_ZONE.to_owned(),
                branch: branch.clone(),
                old: Some(head),
                new: rid,
            },
            RefUpdate {
                zone: name.to_owned(),
                branch: DEFAULT_BRANCH.to_owned(),
                old: None,
                new: zid,
            },
        ];
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
        // the refs write leaves the new zone subscribed but empty, so the files show as staged
        // additions of the new zone; `vcs commit` then reaches the intended state.
        write_atomic(&self.common.join("zones"), &zones.encode())?;
        ws.subs.insert(name.to_owned(), DEFAULT_BRANCH.to_owned());
        self.save_ws(&ws)?;
        write_atomic(&self.common.join("refs"), &refs.encode())?;
        self.record_op(&format!("zone split {name} at {path}"), Some(author))?;
        rep.root_commit = Some(rid);
        rep.zone_commit = Some(zid);
        Ok(rep)
    }
}
