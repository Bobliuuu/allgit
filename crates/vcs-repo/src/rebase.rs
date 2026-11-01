//! `rebase`: replay the commits of a branch onto another branch, one at a time, keeping each
//! commit's change id (new commit id, same logical change).
//!
//! Picks are computed in memory with the merge engine (`merge_flat`, base = the picked commit's
//! parent, ours = the new tip so far, theirs = the picked commit). Nothing but objects is written
//! until the rebase either finishes or stops on a conflict:
//!
//! * finished: working tree and index move, the branch ref moves once (one transaction) and ONE
//!   operation is recorded, so `vcs op undo` restores the previous head exactly;
//! * conflict: the branch ref moves to the last good tip, the structured conflict state is
//!   persisted in `.vcs/conflicts` (same format and workflow as `vcs merge`) and the replay
//!   cursor in `.vcs/rebase` (framed `AGRB`: branch, original head, onto label, picked commit,
//!   remaining commits). `rebase --continue` commits the resolved pick (original change id,
//!   author and message) and carries on; `rebase --abort` restores the original head, index
//!   and working tree. No operation is recorded for a stopped or aborted rebase.
//!
//! Only linear ranges are supported: a merge commit between the branch head and the merge base
//! is refused before anything changes.

use crate::error::{msg, Error, Result};
use crate::merge::{ConflictRec, MergeState};
use crate::meta::{frame, unframe, write_atomic};
use crate::repo::Repo;
use crate::treeops::{self, Flat};
use std::collections::BTreeSet;
use std::fs;
use vcs_merge::{ConflictKind, MarkerLabels};
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::{Commit, EntryMode, Object, ObjectId, RefUpdate, Signature, Transaction};
use vcs_store::ObjectStore;

const FILE: &str = "rebase";
const MAGIC: &[u8; 4] = b"AGRB";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebaseState {
    pub zone: String,
    pub branch: String,
    /// Branch head before the rebase started (what `--abort` and `op undo` restore).
    pub orig: ObjectId,
    pub onto_label: String,
    /// New tip built so far (the branch head while stopped).
    pub cursor: ObjectId,
    /// The original commit whose pick is stopped on conflicts.
    pub current: ObjectId,
    /// Original commits still to replay after `current`, oldest first.
    pub todo: Vec<ObjectId>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RebaseOutcome {
    UpToDate,
    /// Branch head moved; `replayed` commits were recreated (0 for a plain fast-forward).
    Done {
        from: ObjectId,
        to: ObjectId,
        replayed: usize,
    },
    /// Stopped with conflicts in `commit` (an original commit id).
    Conflicted {
        commit: ObjectId,
        paths: Vec<String>,
    },
}

fn dec(e: vcs_objects::DecodeError) -> Error {
    Error::Corrupt(format!("rebase: {e}"))
}

impl RebaseState {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.str(&self.zone)
            .str(&self.branch)
            .id(&self.orig)
            .str(&self.onto_label)
            .id(&self.cursor)
            .id(&self.current);
        w.varint(self.todo.len() as u64);
        for t in &self.todo {
            w.id(t);
        }
        frame(MAGIC, w.finish())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let body = unframe(MAGIC, bytes, "rebase")?;
        let mut r = Reader::new(body);
        let zone = r.str(64).map_err(dec)?;
        let branch = r.str(256).map_err(dec)?;
        let orig = r.id().map_err(dec)?;
        let onto_label = r.str(512).map_err(dec)?;
        let cursor = r.id().map_err(dec)?;
        let current = r.id().map_err(dec)?;
        let n = r.count(32).map_err(dec)?;
        let mut todo = Vec::new();
        for _ in 0..n {
            todo.push(r.id().map_err(dec)?);
        }
        r.finish().map_err(dec)?;
        Ok(RebaseState {
            zone,
            branch,
            orig,
            onto_label,
            cursor,
            current,
            todo,
        })
    }
}

impl Repo {
    /// The in-progress rebase, if any. Ignored (stale) unless the matching merge state is live.
    pub fn rebase_state(&self) -> Result<Option<RebaseState>> {
        let p = self.vcs.join(FILE);
        let bytes = match fs::read(&p) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(Error::Io { path: p, source }),
        };
        let st = RebaseState::decode(&bytes)?;
        match self.merge_state()? {
            Some(m) if m.zone == st.zone && m.branch == st.branch && m.ours == st.cursor => Ok(Some(st)),
            _ => Ok(None),
        }
    }

    /// The rebase file regardless of whether a matching merge state is live (GC roots).
    pub(crate) fn rebase_state_raw(&self) -> Result<Option<RebaseState>> {
        let p = self.vcs.join(FILE);
        match fs::read(&p) {
            Ok(b) => Ok(Some(RebaseState::decode(&b)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(Error::Io { path: p, source }),
        }
    }

    fn clear_rebase_state(&self) -> Result<()> {
        let p = self.vcs.join(FILE);
        match fs::remove_file(&p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(Error::Io { path: p, source }),
        }
    }

    /// Refuse (with a pointer to the rebase commands) while a rebase is stopped.
    pub(crate) fn rebase_guard(&self, what: &str) -> Result<()> {
        if let Some(rs) = self.rebase_state()? {
            return msg(format!(
                "cannot {what}: a rebase of {}/{} onto {} is in progress\n\nOptions:\n  vcs rebase --continue\n  vcs rebase --abort",
                rs.zone, rs.branch, rs.onto_label
            ));
        }
        Ok(())
    }

    /// Replay the checked-out branch of `zone` onto local branch `onto`.
    pub fn rebase(&self, zone: &str, onto: &str, author: Signature) -> Result<RebaseOutcome> {
        let _l = self.lock()?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let Some(branch) = ws.subs.get(zone).cloned() else {
            return msg(format!("zone {zone:?} is not subscribed in this workspace"));
        };
        self.rebase_guard("start a rebase")?;
        if let Some(st) = self.merge_state()? {
            return msg(format!(
                "a merge of {} into {}/{} is already in progress\n\nOptions:\n  vcs conflict list\n  vcs merge --abort",
                st.theirs_label, st.zone, st.branch
            ));
        }
        if branch == onto {
            return msg("cannot rebase a branch onto itself");
        }
        let Some(onto_id) = refs.head(zone, onto) else {
            return msg(format!("no such branch {zone}/{onto}"));
        };
        if self.commit_obj(&onto_id)?.zone != zone {
            return msg(format!("{onto} belongs to another zone"));
        }
        let status = self.status()?;
        self.require_clean(zone, &status)?;
        let Some(head) = refs.head(zone, &branch) else {
            return msg(format!("{zone}/{branch} has no commits to rebase"));
        };
        let base = match self.merge_bases(&head, &onto_id)?.as_slice() {
            [] => {
                return msg(format!(
                    "refusing to rebase unrelated histories ({onto} shares no commit with {branch})"
                ))
            }
            [b] => *b,
            many => {
                return msg(format!(
                    "criss-cross history: {branch} and {onto} have {} merge bases; not supported",
                    many.len()
                ))
            }
        };
        if base == onto_id {
            return Ok(RebaseOutcome::UpToDate);
        }
        // Commits to replay, newest first while walking, then reversed. Linear only.
        let mut todo = Vec::new();
        let mut c = head;
        while c != base {
            let cm = self.commit_obj(&c)?;
            if cm.parents.len() != 1 {
                return msg(format!(
                    "cannot rebase: commit {} has {} parents; only linear history can be replayed (nothing was changed)",
                    c.short(),
                    cm.parents.len()
                ));
            }
            todo.push(c);
            c = cm.parents[0];
        }
        todo.reverse();
        let old = self.commit_obj(&head)?.root;
        let cursor_root = self.commit_obj(&onto_id)?.root;
        let run = Run {
            zone: zone.to_owned(),
            branch,
            orig: head,
            onto_label: onto.to_owned(),
            ref_head: head,
            worktree: old,
            replayed: 0,
            author,
        };
        self.replay(run, onto_id, cursor_root, todo)
    }

    /// Commit the resolved pick and carry on with the remaining commits.
    pub fn rebase_continue(&self, author: Signature) -> Result<RebaseOutcome> {
        let _l = self.lock()?;
        let Some(rs) = self.rebase_state()? else {
            return msg("no rebase in progress");
        };
        let Some(ms) = self.merge_state()? else {
            return msg("no rebase in progress");
        };
        let un = ms.unresolved();
        if !un.is_empty() {
            let list: Vec<String> = un
                .iter()
                .map(|c| format!("  {} ({})", c.path, crate::merge::kind_name(c.kind)))
                .collect();
            return msg(format!(
                "cannot continue: {} unresolved conflict(s):\n{}\n\nEdit the files, then `vcs add <path>`; or `vcs rebase --abort`.",
                un.len(),
                list.join("\n")
            ));
        }
        let zones = self.zones()?;
        let status = self.status()?;
        if let Some(a) = status.zones.iter().find(|a| a.zone == rs.zone) {
            if !a.unstaged.is_empty() {
                return msg(format!(
                    "cannot continue: {} unstaged change(s); `vcs add` them first",
                    a.unstaged.len()
                ));
            }
        }
        let ix = self.index()?;
        let flat: Flat = ix
            .entries
            .iter()
            .filter(|(p, _)| zones.owner(p) == rs.zone)
            .map(|(p, e)| (p.clone(), (e.mode, e.id)))
            .collect();
        let orig_commit = self.commit_obj(&rs.current)?;
        let entries: Vec<(&str, EntryMode, ObjectId)> = flat.iter().map(|(p, (m, i))| (zones.relative(&rs.zone, p), *m, *i)).collect();
        let root = treeops::build(&self.store, entries)?;
        let cid = self.store.put(&Object::Commit(Commit {
            zone: rs.zone.clone(),
            parents: vec![rs.cursor],
            root,
            change_id: orig_commit.change_id,
            author: orig_commit.author,
            message: orig_commit.message,
        }))?;
        let run = Run {
            zone: rs.zone.clone(),
            branch: rs.branch.clone(),
            orig: rs.orig,
            onto_label: rs.onto_label.clone(),
            ref_head: rs.cursor,
            worktree: root,
            replayed: 1,
            author,
        };
        // The old state files stay until the replay finishes or writes a new stop state, so a
        // refused pick (file/directory) leaves the rebase resumable or abortable.
        self.replay(run, cid, root, rs.todo.clone())
    }

    /// Give up: restore the original head, index and working tree. No operation is recorded.
    pub fn rebase_abort(&self) -> Result<()> {
        let _l = self.lock()?;
        let Some(rs) = self.rebase_state()? else {
            return msg("no rebase in progress");
        };
        let Some(ms) = self.merge_state()? else {
            return msg("no rebase in progress");
        };
        self.restore_pre_merge(&ms, rs.orig)?;
        let mut refs = self.refs()?;
        refs.apply(&[RefUpdate {
            zone: rs.zone.clone(),
            branch: rs.branch.clone(),
            old: Some(rs.cursor),
            new: rs.orig,
        }])?;
        write_atomic(&self.common.join("refs"), &refs.encode())?;
        self.clear_rebase_state()?;
        Ok(())
    }

    fn replay(&self, mut run: Run, mut cursor: ObjectId, mut cursor_root: ObjectId, todo: Vec<ObjectId>) -> Result<RebaseOutcome> {
        let zones = self.zones()?;
        let prefix = zones.path_of(&run.zone).unwrap_or("").to_owned();
        let mut it = todo.iter().copied();
        while let Some(pick) = it.next() {
            let pc = self.commit_obj(&pick)?;
            let parent = pc
                .parents
                .first()
                .copied()
                .ok_or_else(|| Error::Corrupt("rebase: root commit in range".into()))?;
            let base_root = self.commit_obj(&parent)?.root;
            let tm = self.merge3_trees(&prefix, Some(base_root), Some(cursor_root), Some(pc.root))?;
            if let Some(c) = tm.conflicts.iter().find(|c| c.kind == ConflictKind::FileDirectory) {
                return msg(format!(
                    "{:?} is a file on one side and a directory on the other while replaying {}; file/directory conflicts are not supported yet (the working tree was not changed)",
                    c.path,
                    pick.short()
                ));
            }
            if !tm.conflicts.is_empty() {
                self.require_conflicts_materialized(&run.zone, tm.conflicts.iter().map(|c| c.path.as_str()), "rebase")?;
            }
            for (id, data) in &tm.new_blobs {
                if self.store.put_content(data)? != *id {
                    return Err(Error::Corrupt("merged blob id mismatch".into()));
                }
            }
            if tm.conflicts.is_empty() {
                let (root, _) = tm.apply(&self.store, Some(cursor_root), true)?;
                cursor = self.store.put(&Object::Commit(Commit {
                    zone: run.zone.clone(),
                    parents: vec![cursor],
                    root,
                    change_id: pc.change_id,
                    author: pc.author,
                    message: pc.message,
                }))?;
                cursor_root = root;
                run.replayed += 1;
                continue;
            }
            // Stop on conflicts: land the tip so far, materialize the conflicted state.
            let rest: Vec<ObjectId> = it.collect();
            let label = format!("{} ({})", pick.short(), pc.message.lines().next().unwrap_or(""));
            return self.stop_on_conflicts(run, cursor, cursor_root, pick, rest, tm, &label);
        }
        self.finish(run, cursor, cursor_root)
    }

    #[allow(clippy::too_many_arguments)]
    fn stop_on_conflicts(
        &self,
        run: Run,
        cursor: ObjectId,
        cursor_root: ObjectId,
        pick: ObjectId,
        rest: Vec<ObjectId>,
        mut tm: crate::treemerge::Merge3,
        label: &str,
    ) -> Result<RebaseOutcome> {
        let zones = self.zones()?;
        let left_label = format!("onto ({}/{})", run.zone, run.onto_label);
        let labels = MarkerLabels {
            left: &left_label,
            base: "base",
            right: label,
        };
        let mut recs = Vec::new();
        let conflicts = std::mem::take(&mut tm.conflicts);
        for c in &conflicts {
            let (mode, id, markers) = self.placeholder(c, &labels)?;
            tm.set_placeholder(&c.path, (mode, id));
            recs.push(ConflictRec {
                path: c.path.clone(),
                kind: c.kind,
                base: c.base,
                left: c.left,
                right: c.right,
                base_mode: c.base_mode,
                left_mode: c.left_mode,
                right_mode: c.right_mode,
                placeholder: id,
                has_markers: markers,
                resolved: false,
            });
        }
        let prefix = zones.path_of(&run.zone).unwrap_or("").to_owned();
        // The conflicted tree is never committed, so it is not stored: the change from the working
        // tree is the (stored) worktree -> cursor difference chained with the patch difference.
        let (_, from_cursor) = tm.apply(&self.store, Some(cursor_root), false)?;
        let touched: BTreeSet<String> = from_cursor
            .old
            .keys()
            .chain(from_cursor.new.keys())
            .cloned()
            .chain(recs.iter().map(|r| r.path.clone()))
            .collect();
        let to_cursor = treeops::diff_trees(&self.store, Some(run.worktree), Some(cursor_root), &prefix)?;
        let from_worktree = crate::treemerge::compose(to_cursor, from_cursor);
        let touched: BTreeSet<String> = touched
            .into_iter()
            .chain(from_worktree.old.keys().chain(from_worktree.new.keys()).cloned())
            .collect();
        // The merge-state "ours" is the cursor, which is what the worktree must look like if aborted.
        let ms = MergeState {
            zone: run.zone.clone(),
            branch: run.branch.clone(),
            ours: cursor,
            theirs: pick,
            theirs_label: label.to_owned(),
            touched: touched.into_iter().collect(),
            conflicts: recs,
        };
        let rs = RebaseState {
            zone: run.zone.clone(),
            branch: run.branch.clone(),
            orig: run.orig,
            onto_label: run.onto_label.clone(),
            cursor,
            current: pick,
            todo: rest,
        };
        // `touched` covers both the paths that differ from the old working tree and those that
        // differ from the new tip (the branch head `--abort` compares against).
        let mut ix = self.index()?;
        let mut refs = self.refs()?;
        self.save_rebase_files(&ms, &rs)?;
        if let Err(e) = self.transition_diff(&run.zone, &zones, &from_worktree, &mut ix) {
            self.clear_merge_state()?;
            self.clear_rebase_state()?;
            return Err(e);
        }
        self.save_index(&mut ix)?;
        if cursor != run.ref_head {
            refs.apply(&[RefUpdate {
                zone: run.zone.clone(),
                branch: run.branch.clone(),
                old: Some(run.ref_head),
                new: cursor,
            }])?;
            self.store.sync()?;
            write_atomic(&self.common.join("refs"), &refs.encode())?;
        }
        Ok(RebaseOutcome::Conflicted {
            commit: pick,
            paths: ms.conflicts.iter().map(|c| c.path.clone()).collect(),
        })
    }

    fn save_rebase_files(&self, ms: &MergeState, rs: &RebaseState) -> Result<()> {
        self.clear_pick_marker()?;
        self.store.sync()?;
        write_atomic(&self.vcs.join("conflicts"), &ms.encode())?;
        write_atomic(&self.vcs.join(FILE), &rs.encode())
    }

    fn finish(&self, run: Run, cursor: ObjectId, cursor_root: ObjectId) -> Result<RebaseOutcome> {
        let zones = self.zones()?;
        let mut ix = self.index()?;
        let mut refs = self.refs()?;
        let update = RefUpdate {
            zone: run.zone.clone(),
            branch: run.branch.clone(),
            old: Some(run.ref_head),
            new: cursor,
        };
        let message = format!("rebase {}/{} onto {}", run.zone, run.branch, run.onto_label);
        self.store.put(&Object::Transaction(Transaction {
            updates: vec![update.clone()],
            author: run.author.clone(),
            message: message.clone(),
            lock_ops: vec![],
        }))?;
        let diff = treeops::diff_trees(
            &self.store,
            Some(run.worktree),
            Some(cursor_root),
            zones.path_of(&run.zone).unwrap_or(""),
        )?;
        self.transition_diff(&run.zone, &zones, &diff, &mut ix)?;
        self.save_index(&mut ix)?;
        refs.apply(&[update])?;
        self.store.sync()?; // rebuilt commits + transaction durable before refs name them
        write_atomic(&self.common.join("refs"), &refs.encode())?;
        self.clear_merge_state()?;
        self.clear_rebase_state()?;
        self.record_op(&message, Some(run.author))?;
        Ok(RebaseOutcome::Done {
            from: run.orig,
            to: cursor,
            replayed: run.replayed,
        })
    }
}

struct Run {
    zone: String,
    branch: String,
    /// Branch head before the whole rebase.
    orig: ObjectId,
    onto_label: String,
    /// What the branch ref currently names (differs from `orig` after `--continue`).
    ref_head: ObjectId,
    /// Tree of what the working tree currently materializes for the zone.
    worktree: ObjectId,
    replayed: usize,
    author: Signature,
}
