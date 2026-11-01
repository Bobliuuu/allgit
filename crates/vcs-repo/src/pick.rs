//! `cherry-pick` and `revert`: re-apply or undo one commit with the merge engine.
//!
//! Cherry-pick is a three-way merge with base = the commit's parent, ours = the branch head,
//! theirs = the commit. Revert is the same with base and theirs swapped. A clean result becomes
//! one single-parent commit (one transaction, one operation, so `vcs op undo` restores the
//! previous head and worktree). Conflicts are persisted exactly like `vcs merge`
//! (`.vcs/conflicts`, markers in the worktree, `vcs resolve`/`vcs add`, `vcs merge --abort`);
//! the extra `.vcs/pick` marker tells `vcs commit` to create a single-parent commit instead of
//! a merge commit. Merge commits cannot be picked or reverted (no mainline selection yet).

use crate::error::{msg, Error, Result};
use crate::merge::{ConflictRec, MergeOutcome, MergeState};
use crate::meta::{frame, unframe, write_atomic};
use crate::repo::{fresh_change_id, Repo};
use std::collections::BTreeSet;
use std::fs;
use vcs_merge::{ConflictKind, MarkerLabels};
use vcs_objects::{Commit, Object, ObjectId, RefUpdate, Signature, Transaction};
use vcs_store::ObjectStore;

const FILE: &str = "pick";
const MAGIC: &[u8; 4] = b"AGPK";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickKind {
    CherryPick,
    Revert,
}

impl Repo {
    /// Kind and source commit of the stopped cherry-pick/revert, if the marker file exists.
    pub fn pick_marker(&self) -> Result<Option<(PickKind, ObjectId)>> {
        match fs::read(self.vcs.join(FILE)) {
            Ok(b) => {
                let body = unframe(MAGIC, &b, "pick")?;
                let Some((k, id)) = body.split_first() else {
                    return Err(Error::Corrupt("pick: empty".into()));
                };
                let id = ObjectId::from_hex(&String::from_utf8_lossy(id)).map_err(|_| Error::Corrupt("pick: bad id".into()))?;
                match k {
                    0 => Ok(Some((PickKind::CherryPick, id))),
                    1 => Ok(Some((PickKind::Revert, id))),
                    _ => Err(Error::Corrupt("pick: bad kind".into())),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(Error::Io {
                path: self.vcs.join(FILE),
                source,
            }),
        }
    }

    pub(crate) fn clear_pick_marker(&self) -> Result<()> {
        match fs::remove_file(self.vcs.join(FILE)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(Error::Io {
                path: self.vcs.join(FILE),
                source,
            }),
        }
    }

    /// Resolve `rev` to a commit of `zone`: a branch name, `HEAD`, or a (prefix of a) commit id
    /// reachable from any branch of the zone.
    pub fn resolve_commit(&self, zone: &str, rev: &str) -> Result<ObjectId> {
        let zone = self.resolve_zone(zone)?;
        let refs = self.refs()?;
        let ws = self.workspace()?;
        if rev == "HEAD" {
            return self
                .current_head(&zone, &ws, &refs)
                .ok_or_else(|| Error::Msg(format!("zone {zone:?} has no commits")));
        }
        if let Some(h) = refs.head(&zone, rev) {
            return Ok(h);
        }
        if rev.len() < 4 || !rev.bytes().all(|b| b.is_ascii_hexdigit()) {
            return msg(format!("unknown revision {rev:?}"));
        }
        let rev = rev.to_ascii_lowercase();
        let mut seen = BTreeSet::new();
        let mut stack: Vec<ObjectId> = ws
            .subs
            .get(&zone)
            .and_then(|b| refs.head(&zone, b))
            .into_iter()
            .chain(self.branches(&zone)?.into_iter().filter_map(|(_, id, _)| id))
            .collect();
        let mut hits = BTreeSet::new();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if id.to_string().starts_with(&rev) {
                hits.insert(id);
            }
            stack.extend(self.commit_obj(&id)?.parents);
        }
        let mut it = hits.into_iter();
        match (it.next(), it.next()) {
            (Some(h), None) => Ok(h),
            (None, _) => msg(format!("no commit in zone {zone:?} matches {rev:?}")),
            _ => msg(format!("revision {rev:?} is ambiguous")),
        }
    }

    /// Apply the change introduced by commit `rev` on top of the checked-out branch of `zone`.
    pub fn cherry_pick(&self, zone: &str, rev: &str, author: Signature) -> Result<MergeOutcome> {
        self.pick(zone, rev, PickKind::CherryPick, author)
    }

    /// Create a commit that undoes commit `rev` on the checked-out branch of `zone`.
    pub fn revert(&self, zone: &str, rev: &str, author: Signature) -> Result<MergeOutcome> {
        self.pick(zone, rev, PickKind::Revert, author)
    }

    fn pick(&self, zone: &str, rev: &str, kind: PickKind, author: Signature) -> Result<MergeOutcome> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        let target = self.resolve_commit(zone, rev)?;
        let _l = self.lock()?;
        let verb = if kind == PickKind::Revert { "revert" } else { "cherry-pick" };
        self.rebase_guard(verb)?;
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut refs = self.refs()?;
        let Some(branch) = ws.subs.get(zone).cloned() else {
            return msg(format!("zone {zone:?} is not subscribed in this workspace"));
        };
        if let Some(st) = self.merge_state()? {
            return msg(format!(
                "a merge of {} into {}/{} is already in progress\n\nOptions:\n  vcs conflict list\n  vcs merge --abort",
                st.theirs_label, st.zone, st.branch
            ));
        }
        let status = self.status()?;
        self.require_clean(zone, &status)?;
        let c = self.commit_obj(&target)?;
        if c.zone != zone {
            return msg(format!("{} belongs to zone {:?}, not {zone:?}", target.short(), c.zone));
        }
        if c.parents.len() > 1 {
            return msg(format!(
                "{} is a merge commit; {verb} of merge commits is not supported",
                target.short()
            ));
        }
        let Some(ours) = refs.head(zone, &branch) else {
            return msg(format!("{zone}/{branch} has no commits to {verb} onto"));
        };
        let prefix = zones.path_of(zone).unwrap_or("").to_owned();
        let this = c.root;
        let parent = match c.parents.first() {
            Some(p) => Some(self.commit_obj(p)?.root),
            None => None,
        };
        let (base, theirs) = match kind {
            PickKind::CherryPick => (parent, Some(this)),
            PickKind::Revert => (Some(this), parent),
        };
        let ours_root = self.commit_obj(&ours)?.root;
        let mut ix = self.index()?;
        let mut tm = self.merge3_trees(&prefix, base, Some(ours_root), theirs)?;
        if let Some(x) = tm.conflicts.iter().find(|x| x.kind == ConflictKind::FileDirectory) {
            return msg(format!(
                "{:?} is a file on one side and a directory on the other; file/directory conflicts are not supported yet (nothing was changed)",
                x.path
            ));
        }
        if !tm.conflicts.is_empty() {
            self.require_conflicts_materialized(zone, tm.conflicts.iter().map(|c| c.path.as_str()), verb)?;
        }
        for (id, data) in &tm.new_blobs {
            if self.store.put_blob(data)? != *id {
                return Err(Error::Corrupt("merged blob id mismatch".into()));
            }
        }
        let first = c.message.lines().next().unwrap_or("").to_owned();
        let (label, message) = match kind {
            PickKind::CherryPick => (
                format!("cherry-pick {}", target.short()),
                format!("{}\n\n(cherry picked from commit {target})", c.message.trim_end()),
            ),
            PickKind::Revert => (
                format!("revert of {}", target.short()),
                format!("Revert \"{first}\"\n\nThis reverts commit {target}."),
            ),
        };
        let mut recs = Vec::new();
        let left_label = format!("ours ({zone}/{branch})");
        let labels = MarkerLabels {
            left: &left_label,
            base: "base",
            right: &label,
        };
        let conflicts = std::mem::take(&mut tm.conflicts);
        for x in &conflicts {
            let (mode, id, markers) = self.placeholder(x, &labels)?;
            tm.set_placeholder(&x.path, (mode, id));
            recs.push(ConflictRec {
                path: x.path.clone(),
                kind: x.kind,
                base: x.base,
                left: x.left,
                right: x.right,
                base_mode: x.base_mode,
                left_mode: x.left_mode,
                right_mode: x.right_mode,
                placeholder: id,
                has_markers: markers,
                resolved: false,
            });
        }
        let (new_root, diff) = tm.apply(&self.store, Some(ours_root), recs.is_empty())?;
        if recs.is_empty() {
            if new_root == ours_root {
                return msg(format!("{verb} of {} changes nothing here (already applied?)", target.short()));
            }
            let root = new_root;
            let commit = Commit {
                zone: zone.to_owned(),
                parents: vec![ours],
                root,
                change_id: fresh_change_id(zone, &root, Some(&ours), &author),
                author: author.clone(),
                message: message.clone(),
            };
            let cid = self.store.put(&Object::Commit(commit))?;
            let update = RefUpdate {
                zone: zone.into(),
                branch: branch.clone(),
                old: Some(ours),
                new: cid,
            };
            self.store.put(&Object::Transaction(Transaction {
                updates: vec![update.clone()],
                author,
                message: message.clone(),
                lock_ops: vec![],
            }))?;
            self.transition_diff(zone, &zones, &diff, &mut ix)?;
            self.save_index(&mut ix)?;
            refs.apply(&[update])?;
            self.store.sync()?;
            write_atomic(&self.common.join("refs"), &refs.encode())?;
            self.record_op(&format!("{verb} {} onto {zone}/{branch}", target.short()), None)?;
            return Ok(MergeOutcome::Merged { commit: cid });
        }
        let touched: BTreeSet<String> = diff
            .old
            .keys()
            .chain(diff.new.keys())
            .cloned()
            .chain(recs.iter().map(|r| r.path.clone()))
            .collect();
        let state = MergeState {
            zone: zone.to_owned(),
            branch: branch.clone(),
            ours,
            theirs: target,
            theirs_label: label,
            touched: touched.into_iter().collect(),
            conflicts: recs,
        };
        self.save_merge_state(&state)?;
        let mut body = vec![(kind == PickKind::Revert) as u8];
        body.extend_from_slice(target.to_string().as_bytes());
        let mark = frame(MAGIC, body);
        write_atomic(&self.vcs.join(FILE), &mark)?;
        if let Err(e) = self.transition_diff(zone, &zones, &diff, &mut ix) {
            self.clear_merge_state()?;
            return Err(e);
        }
        self.save_index(&mut ix)?;
        Ok(MergeOutcome::Conflicted {
            paths: state.conflicts.iter().map(|c| c.path.clone()).collect(),
        })
    }
}
