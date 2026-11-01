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
//! Interactive rebase (`rebase -i`) shares this engine: the todo is a list of [`Step`]s
//! (`pick`/`reword`/`edit`/`squash`/`fixup`/`exec`; `drop` is just a missing step). A pick whose
//! parent tree equals the new tip's tree reuses the picked tree without a merge, so reordering or
//! rewording in place costs no tree merge. `edit` and a failing `exec` stop WITHOUT conflicts
//! (`Stop::Edit`/`Stop::Exec`): the branch and working tree are at the new tip, commits are
//! allowed, and `--continue` resumes from whatever the branch head is. `--skip` drops the
//! stopped pick; `--abort` restores the original head. One operation is recorded when the whole
//! rebase finishes, so `vcs op undo` restores the pre-rebase state.
//!
//! Only linear ranges are supported: a merge commit between the branch head and the merge base
//! is refused before anything changes.

use crate::error::{msg, Error, Result};
use crate::merge::{ConflictRec, MergeState};
use crate::meta::{frame, unframe, write_atomic};
use crate::repo::Repo;
use crate::treeops::{self, Flat};
use std::collections::{BTreeSet, VecDeque};
use std::fs;
use vcs_merge::{ConflictKind, MarkerLabels};
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::{Commit, EntryMode, Object, ObjectId, RefUpdate, Signature, Transaction};
use vcs_store::ObjectStore;

const FILE: &str = "rebase";
const MAGIC: &[u8; 4] = b"AGRB";
const MAGIC2: &[u8; 4] = b"AGR2";

/// What a todo line does with its commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Pick,
    /// Pick, then edit the message.
    Reword,
    /// Pick, then stop so the commit can be amended or followed by more commits.
    Edit,
    /// Fold into the previous commit; messages are joined and edited once at the end of the chain.
    Squash,
    /// Fold into the previous commit and keep its message.
    Fixup,
}

impl Act {
    fn tag(self) -> u8 {
        match self {
            Act::Pick => 0,
            Act::Reword => 1,
            Act::Edit => 2,
            Act::Squash => 3,
            Act::Fixup => 4,
        }
    }
    fn from_tag(t: u8) -> Result<Self> {
        Ok(match t {
            0 => Act::Pick,
            1 => Act::Reword,
            2 => Act::Edit,
            3 => Act::Squash,
            4 => Act::Fixup,
            _ => return Err(Error::Corrupt("rebase: bad action".into())),
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Act::Pick => "pick",
            Act::Reword => "reword",
            Act::Edit => "edit",
            Act::Squash => "squash",
            Act::Fixup => "fixup",
        }
    }
    fn folds(self) -> bool {
        matches!(self, Act::Squash | Act::Fixup)
    }
}

/// One live todo entry (a `drop` is simply not a step).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Pick(Act, ObjectId),
    /// Shell command run in the working tree root; a non-zero exit stops the rebase.
    Exec(String),
}

/// Why a rebase is stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// Merge conflicts (state in `.vcs/conflicts`).
    Conflict,
    /// An `edit` step, or a message editor that failed: nothing is conflicted.
    Edit,
    /// An `exec` command failed.
    Exec,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebaseState {
    pub zone: String,
    pub branch: String,
    /// Branch head before the rebase started (what `--abort` and `op undo` restore).
    pub orig: ObjectId,
    pub onto_label: String,
    /// New tip built so far (the branch head while stopped).
    pub cursor: ObjectId,
    /// The original commit whose pick is stopped (conflicts or `edit`).
    pub current: ObjectId,
    /// Steps still to replay after `current`, in order.
    pub todo: Vec<Step>,
    /// Action of the stopped pick (conflict stops).
    pub cur_act: Act,
    pub stop: Stop,
    /// A `squash` is in the current fold chain: its message is edited when the chain ends.
    pub chain_squash: bool,
    pub interactive: bool,
}

impl RebaseState {
    /// Every commit id the state names (GC roots).
    pub fn todo_ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.todo.iter().filter_map(|s| match s {
            Step::Pick(_, id) => Some(*id),
            Step::Exec(_) => None,
        })
    }
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
    /// Stopped without conflicts: an `edit` step (`detail` empty), a failed `exec` or a failed
    /// message editor (`detail` says what). The branch is at `commit`, the new tip so far.
    Stopped {
        kind: Stop,
        commit: ObjectId,
        detail: String,
    },
}

/// Edits a commit message (reword, squash). Receives the proposed message, returns the final one.
pub type MsgEditor<'a> = &'a mut dyn FnMut(&str) -> Result<String>;

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
            match t {
                Step::Pick(a, id) => {
                    w.u8(a.tag()).id(id);
                }
                Step::Exec(c) => {
                    w.u8(9).str(c);
                }
            }
        }
        w.u8(self.cur_act.tag());
        w.u8(match self.stop {
            Stop::Conflict => 0,
            Stop::Edit => 1,
            Stop::Exec => 2,
        });
        w.u8(u8::from(self.chain_squash));
        w.u8(u8::from(self.interactive));
        frame(MAGIC2, w.finish())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let v2 = bytes.get(..4) == Some(MAGIC2.as_slice());
        let body = unframe(if v2 { MAGIC2 } else { MAGIC }, bytes, "rebase")?;
        let mut r = Reader::new(body);
        let zone = r.str(64).map_err(dec)?;
        let branch = r.str(256).map_err(dec)?;
        let orig = r.id().map_err(dec)?;
        let onto_label = r.str(512).map_err(dec)?;
        let cursor = r.id().map_err(dec)?;
        let current = r.id().map_err(dec)?;
        let n = r.count(2).map_err(dec)?;
        let mut todo = Vec::new();
        for _ in 0..n {
            if !v2 {
                todo.push(Step::Pick(Act::Pick, r.id().map_err(dec)?));
                continue;
            }
            let t = r.u8().map_err(dec)?;
            todo.push(if t == 9 {
                Step::Exec(r.str(1 << 16).map_err(dec)?)
            } else {
                Step::Pick(Act::from_tag(t)?, r.id().map_err(dec)?)
            });
        }
        let (mut cur_act, mut stop, mut chain_squash, mut interactive) = (Act::Pick, Stop::Conflict, false, false);
        if v2 {
            cur_act = Act::from_tag(r.u8().map_err(dec)?)?;
            stop = match r.u8().map_err(dec)? {
                0 => Stop::Conflict,
                1 => Stop::Edit,
                2 => Stop::Exec,
                _ => return Err(Error::Corrupt("rebase: bad stop".into())),
            };
            chain_squash = r.u8().map_err(dec)? != 0;
            interactive = r.u8().map_err(dec)? != 0;
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
            cur_act,
            stop,
            chain_squash,
            interactive,
        })
    }
}

/// A commit in the range an interactive rebase would replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanCommit {
    pub id: ObjectId,
    pub subject: String,
}

/// The range an interactive rebase replays, oldest commit first.
#[derive(Clone, Debug)]
pub struct RebasePlan {
    pub zone: String,
    pub branch: String,
    pub onto_label: String,
    pub onto: ObjectId,
    pub head: ObjectId,
    pub commits: Vec<PlanCommit>,
}

struct Prepared {
    branch: String,
    head: ObjectId,
    onto_id: ObjectId,
    /// Commits to replay, oldest first (linear).
    todo: Vec<ObjectId>,
    up_to_date: bool,
}

impl Repo {
    /// The in-progress rebase, if any. A conflict stop is ignored (stale) unless the matching
    /// merge state is live.
    pub fn rebase_state(&self) -> Result<Option<RebaseState>> {
        let p = self.vcs.join(FILE);
        let bytes = match fs::read(&p) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(Error::Io { path: p, source }),
        };
        let st = RebaseState::decode(&bytes)?;
        if st.stop != Stop::Conflict {
            return Ok(Some(st));
        }
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

    /// Refuse (with a pointer to the rebase commands) while a rebase is stopped. A rebase that is
    /// stopped at an `edit` or a failed `exec` has no conflicts, so commits, resets and the like
    /// stay possible; only starting another rebase is refused.
    pub(crate) fn rebase_guard(&self, what: &str) -> Result<()> {
        if let Some(rs) = self.rebase_state()? {
            if rs.stop != Stop::Conflict && what != "start a rebase" {
                return Ok(());
            }
            return msg(format!(
                "cannot {what}: a rebase of {}/{} onto {} is in progress\n\nOptions:\n  vcs rebase --continue\n  vcs rebase --skip\n  vcs rebase --abort",
                rs.zone, rs.branch, rs.onto_label
            ));
        }
        Ok(())
    }

    /// Checks shared by every way of starting a rebase; resolves the commits to replay.
    fn rebase_prepare(&self, zone: &str, onto: &str, interactive: bool) -> Result<Prepared> {
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
        if !interactive && branch == onto {
            return msg("cannot rebase a branch onto itself");
        }
        let onto_id = match refs.head(zone, onto) {
            Some(id) => id,
            None if interactive => self
                .resolve_rev(zone, onto)
                .map_err(|_| Error::Msg(format!("no such branch or revision {zone}/{onto}")))?,
            None => return msg(format!("no such branch {zone}/{onto}")),
        };
        if self.commit_obj(&onto_id)?.zone != zone {
            return msg(format!("{onto} belongs to another zone"));
        }
        let status = self.status_tracked()?;
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
        if base == onto_id && !interactive {
            return Ok(Prepared {
                branch,
                head,
                onto_id,
                todo: Vec::new(),
                up_to_date: true,
            });
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
        Ok(Prepared {
            branch,
            head,
            onto_id,
            todo,
            up_to_date: false,
        })
    }

    /// Replay the checked-out branch of `zone` onto local branch `onto`.
    pub fn rebase(&self, zone: &str, onto: &str, author: Signature) -> Result<RebaseOutcome> {
        let _l = self.lock()?;
        let p = self.rebase_prepare(zone, onto, false)?;
        if p.up_to_date {
            return Ok(RebaseOutcome::UpToDate);
        }
        let steps = p.todo.iter().map(|id| Step::Pick(Act::Pick, *id)).collect();
        self.rebase_launch(zone, onto, p, steps, false, author, &mut |m: &str| Ok(m.to_owned()))
    }

    /// The commits `rebase -i <onto>` would replay (`onto` is a branch or any revision).
    pub fn rebase_plan(&self, zone: &str, onto: &str) -> Result<RebasePlan> {
        let _l = self.lock()?;
        let p = self.rebase_prepare(zone, onto, true)?;
        if p.todo.is_empty() {
            return msg(format!("nothing to do: {} has no commits that {onto} lacks", p.branch));
        }
        let mut commits = Vec::with_capacity(p.todo.len());
        for id in &p.todo {
            let c = self.commit_obj(id)?;
            commits.push(PlanCommit {
                id: *id,
                subject: c.message.lines().next().unwrap_or("").to_owned(),
            });
        }
        Ok(RebasePlan {
            zone: zone.to_owned(),
            branch: p.branch,
            onto_label: onto.to_owned(),
            onto: p.onto_id,
            head: p.head,
            commits,
        })
    }

    /// Run an interactive rebase with an edited todo (`steps`; commits not named are dropped).
    pub fn rebase_run(&self, plan: &RebasePlan, steps: Vec<Step>, author: Signature, ed: MsgEditor) -> Result<RebaseOutcome> {
        let _l = self.lock()?;
        let p = self.rebase_prepare(&plan.zone, &plan.onto_label, true)?;
        if p.head != plan.head || p.onto_id != plan.onto {
            return msg("the branch changed while the todo list was being edited; nothing was done");
        }
        let mut first = true;
        for s in &steps {
            if let Step::Pick(a, id) = s {
                if !plan.commits.iter().any(|c| c.id == *id) {
                    return msg(format!("commit {} is not in the range being rebased", id.short()));
                }
                if a.folds() && first {
                    return msg(format!("cannot '{}' without a previous commit", a.name()));
                }
                first = false;
            }
        }
        self.rebase_launch(&plan.zone, &plan.onto_label, p, steps, true, author, ed)
    }

    #[allow(clippy::too_many_arguments)]
    fn rebase_launch(
        &self,
        zone: &str,
        onto: &str,
        p: Prepared,
        steps: Vec<Step>,
        interactive: bool,
        author: Signature,
        ed: MsgEditor,
    ) -> Result<RebaseOutcome> {
        let old = self.commit_obj(&p.head)?.root;
        let cursor_root = self.commit_obj(&p.onto_id)?.root;
        let run = Run {
            zone: zone.to_owned(),
            branch: p.branch,
            orig: p.head,
            onto_label: onto.to_owned(),
            ref_head: p.head,
            worktree: old,
            replayed: 0,
            author,
            chain_squash: false,
            interactive,
        };
        self.replay(run, p.onto_id, cursor_root, steps, ed)
    }

    /// [`Repo::rebase_continue_with`] without a message editor (fails only if one is needed).
    pub fn rebase_continue(&self, author: Signature) -> Result<RebaseOutcome> {
        self.rebase_continue_with(author, &mut |_: &str| msg("no message editor available"))
    }

    /// Commit the resolved pick (or resume after an `edit`/failed `exec`) and carry on.
    pub fn rebase_continue_with(&self, author: Signature, ed: MsgEditor) -> Result<RebaseOutcome> {
        let _l = self.lock()?;
        let Some(rs) = self.rebase_state()? else {
            return msg("no rebase in progress");
        };
        if rs.stop != Stop::Conflict {
            return self.resume_plain(rs, author, ed);
        }
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
        let status = self.status_tracked()?;
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
        let pc = self.commit_obj(&rs.current)?;
        let entries: Vec<(&str, EntryMode, ObjectId)> = flat.iter().map(|(p, (m, i))| (zones.relative(&rs.zone, p), *m, *i)).collect();
        let root = treeops::build(&self.store, entries)?;
        let mut run = Run {
            zone: rs.zone.clone(),
            branch: rs.branch.clone(),
            orig: rs.orig,
            onto_label: rs.onto_label.clone(),
            ref_head: rs.cursor,
            worktree: root,
            replayed: 1,
            author,
            chain_squash: rs.chain_squash,
            interactive: rs.interactive,
        };
        let next_folds = matches!(rs.todo.first(), Some(Step::Pick(a, _)) if a.folds());
        let cid = match self.build_pick(&mut run, rs.cur_act, rs.cursor, &pc, root, next_folds, ed)? {
            Built::Ok(c) => c,
            Built::EditorFailed(e) => return msg(format!("cannot continue: {e}")),
        };
        // The old state files stay until the replay finishes or writes a new stop state, so a
        // refused pick (file/directory) leaves the rebase resumable or abortable.
        self.replay(run, cid, root, rs.todo.clone(), ed)
    }

    /// Resume a rebase stopped at an `edit` or a failed `exec`: carry on from the branch head.
    fn resume_plain(&self, rs: RebaseState, author: Signature, ed: MsgEditor) -> Result<RebaseOutcome> {
        let ws = self.workspace()?;
        if ws.subs.get(&rs.zone) != Some(&rs.branch) {
            return msg(format!(
                "cannot continue: {}/{} is not checked out any more; `vcs switch {}` or `vcs rebase --abort`",
                rs.zone, rs.branch, rs.branch
            ));
        }
        let status = self.status()?;
        self.require_clean(&rs.zone, &status)?;
        let head = self
            .refs()?
            .head(&rs.zone, &rs.branch)
            .ok_or_else(|| Error::Msg("branch has no commits".into()))?;
        let root = self.commit_obj(&head)?.root;
        let run = Run {
            zone: rs.zone,
            branch: rs.branch,
            orig: rs.orig,
            onto_label: rs.onto_label,
            ref_head: head,
            worktree: root,
            replayed: 0,
            author,
            chain_squash: rs.chain_squash,
            interactive: rs.interactive,
        };
        self.replay(run, head, root, rs.todo, ed)
    }

    /// Drop the stopped pick (restoring the working tree to the new tip) and carry on.
    pub fn rebase_skip(&self, author: Signature, ed: MsgEditor) -> Result<RebaseOutcome> {
        let _l = self.lock()?;
        let Some(rs) = self.rebase_state()? else {
            return msg("no rebase in progress");
        };
        if rs.stop != Stop::Conflict {
            return msg("nothing to skip: the rebase is not stopped on a pick with conflicts; use `vcs rebase --continue` or `--abort`");
        }
        let Some(ms) = self.merge_state()? else {
            return msg("no rebase in progress");
        };
        self.restore_pre_merge(&ms, rs.cursor)?;
        self.clear_merge_state()?;
        let root = self.commit_obj(&rs.cursor)?.root;
        let run = Run {
            zone: rs.zone,
            branch: rs.branch,
            orig: rs.orig,
            onto_label: rs.onto_label,
            ref_head: rs.cursor,
            worktree: root,
            replayed: 0,
            author,
            chain_squash: rs.chain_squash,
            interactive: rs.interactive,
        };
        self.replay(run, rs.cursor, root, rs.todo, ed)
    }

    /// Give up: restore the original head, index and working tree. No operation is recorded.
    pub fn rebase_abort(&self) -> Result<()> {
        let _l = self.lock()?;
        let Some(rs) = self.rebase_state()? else {
            return msg("no rebase in progress");
        };
        if rs.stop != Stop::Conflict {
            self.clear_rebase_state()?;
            self.reset_nolock(&rs.zone, &rs.orig)?;
            return Ok(());
        }
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

    /// Hard-reset the zone's checked-out branch to `target` (lock already held, no rebase state).
    fn reset_nolock(&self, zone: &str, target: &ObjectId) -> Result<()> {
        // `reset` takes the lock itself; release ours by re-entering through a plain call is not
        // possible, so this mirrors it for the one case needed here.
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut refs = self.refs()?;
        let Some(branch) = ws.subs.get(zone).cloned() else {
            return msg(format!("zone {zone:?} is not subscribed in this workspace"));
        };
        let old = refs.head(zone, &branch);
        let tc = self.commit_obj(target)?;
        let from = match old {
            Some(h) => Some(self.commit_obj(&h)?.root),
            None => None,
        };
        let diff = treeops::diff_trees(&self.store, from, Some(tc.root), zones.path_of(zone).unwrap_or(""))?;
        let mut ix = self.index()?;
        self.transition_diff(zone, &zones, &diff, &mut ix)?;
        self.save_index(&mut ix)?;
        if old != Some(*target) {
            refs.apply(&[RefUpdate {
                zone: zone.to_owned(),
                branch,
                old,
                new: *target,
            }])?;
            self.store.sync()?;
            write_atomic(&self.common.join("refs"), &refs.encode())?;
        }
        Ok(())
    }

    /// Make the working tree, index and branch ref match the new tip (no operation recorded).
    fn land(&self, run: &mut Run, cursor: ObjectId, cursor_root: ObjectId) -> Result<()> {
        if cursor == run.ref_head && cursor_root == run.worktree {
            return Ok(());
        }
        let zones = self.zones()?;
        let mut ix = self.index()?;
        let diff = treeops::diff_trees(
            &self.store,
            Some(run.worktree),
            Some(cursor_root),
            zones.path_of(&run.zone).unwrap_or(""),
        )?;
        self.transition_diff(&run.zone, &zones, &diff, &mut ix)?;
        self.save_index(&mut ix)?;
        if cursor != run.ref_head {
            let mut refs = self.refs()?;
            refs.apply(&[RefUpdate {
                zone: run.zone.clone(),
                branch: run.branch.clone(),
                old: Some(run.ref_head),
                new: cursor,
            }])?;
            self.store.sync()?;
            write_atomic(&self.common.join("refs"), &refs.encode())?;
        }
        run.ref_head = cursor;
        run.worktree = cursor_root;
        Ok(())
    }

    /// Persist a stop that has no merge state (`edit`, failed `exec`, failed editor).
    #[allow(clippy::too_many_arguments)]
    fn stop_plain(&self, run: &Run, cursor: ObjectId, current: ObjectId, cur_act: Act, stop: Stop, rest: VecDeque<Step>) -> Result<()> {
        let rs = RebaseState {
            zone: run.zone.clone(),
            branch: run.branch.clone(),
            orig: run.orig,
            onto_label: run.onto_label.clone(),
            cursor,
            current,
            todo: rest.into(),
            cur_act,
            stop,
            chain_squash: run.chain_squash,
            interactive: run.interactive,
        };
        self.store.sync()?;
        write_atomic(&self.vcs.join(FILE), &rs.encode())
    }

    /// Build the commit for a clean pick whose merged tree is `root`.
    #[allow(clippy::too_many_arguments)]
    fn build_pick(
        &self,
        run: &mut Run,
        act: Act,
        cursor: ObjectId,
        pc: &Commit,
        root: ObjectId,
        next_folds: bool,
        ed: MsgEditor,
    ) -> Result<Built> {
        let (parents, change_id, author, message) = if act.folds() {
            let cc = self.commit_obj(&cursor)?;
            let mut message = if act == Act::Squash {
                run.chain_squash = true;
                format!("{}\n\n{}", cc.message.trim_end(), pc.message.trim())
            } else {
                cc.message.clone()
            };
            if !next_folds && run.chain_squash {
                match ed(&message) {
                    Ok(m) => message = m,
                    Err(e) => return Ok(Built::EditorFailed(e.to_string())),
                }
                run.chain_squash = false;
            }
            (cc.parents, cc.change_id, cc.author, message)
        } else {
            run.chain_squash = false;
            let message = if act == Act::Reword {
                match ed(&pc.message) {
                    Ok(m) => m,
                    Err(e) => return Ok(Built::EditorFailed(e.to_string())),
                }
            } else {
                pc.message.clone()
            };
            (vec![cursor], pc.change_id, pc.author.clone(), message)
        };
        let id = self.store.put(&Object::Commit(Commit {
            zone: run.zone.clone(),
            parents,
            root,
            change_id,
            author,
            message,
        }))?;
        Ok(Built::Ok(id))
    }

    fn replay(
        &self,
        mut run: Run,
        mut cursor: ObjectId,
        mut cursor_root: ObjectId,
        todo: Vec<Step>,
        ed: MsgEditor,
    ) -> Result<RebaseOutcome> {
        let zones = self.zones()?;
        let prefix = zones.path_of(&run.zone).unwrap_or("").to_owned();
        let mut q: VecDeque<Step> = todo.into();
        while let Some(step) = q.pop_front() {
            let (act, pick) = match step {
                Step::Exec(cmd) => {
                    self.land(&mut run, cursor, cursor_root)?;
                    let st = std::process::Command::new("sh")
                        .arg("-c")
                        .arg(&cmd)
                        .current_dir(&self.worktree)
                        .status()
                        .map_err(|e| Error::Msg(format!("could not run {cmd:?}: {e}")))?;
                    if !st.success() {
                        let detail = format!("exec {cmd:?} failed ({st}); fix it, commit, then `vcs rebase --continue`");
                        self.stop_plain(&run, cursor, cursor, Act::Pick, Stop::Exec, q)?;
                        return Ok(RebaseOutcome::Stopped {
                            kind: Stop::Exec,
                            commit: cursor,
                            detail,
                        });
                    }
                    continue;
                }
                Step::Pick(a, id) => (a, id),
            };
            let pc = self.commit_obj(&pick)?;
            let parent = pc
                .parents
                .first()
                .copied()
                .ok_or_else(|| Error::Corrupt("rebase: root commit in range".into()))?;
            let base_root = self.commit_obj(&parent)?.root;
            let next_folds = matches!(q.front(), Some(Step::Pick(a, _)) if a.folds());
            let root = if cursor_root == base_root {
                // the new tip has the picked commit's parent tree: the result IS the picked tree
                pc.root
            } else {
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
                if !tm.conflicts.is_empty() {
                    // Stop on conflicts: land the tip so far, materialize the conflicted state.
                    let rest: Vec<Step> = q.into();
                    let label = format!("{} ({})", pick.short(), pc.message.lines().next().unwrap_or(""));
                    return self.stop_on_conflicts(run, cursor, cursor_root, pick, act, rest, tm, &label);
                }
                tm.apply(&self.store, Some(cursor_root), true)?.0
            };
            match self.build_pick(&mut run, act, cursor, &pc, root, next_folds, ed)? {
                Built::Ok(c) => cursor = c,
                Built::EditorFailed(e) => {
                    // keep what is done, retry this step on --continue
                    self.land(&mut run, cursor, cursor_root)?;
                    q.push_front(Step::Pick(act, pick));
                    self.stop_plain(&run, cursor, pick, act, Stop::Edit, q)?;
                    return Ok(RebaseOutcome::Stopped {
                        kind: Stop::Edit,
                        commit: cursor,
                        detail: format!(
                            "could not edit the message of {}: {e}; fix the editor and `vcs rebase --continue`",
                            pick.short()
                        ),
                    });
                }
            }
            cursor_root = root;
            run.replayed += 1;
            if act == Act::Edit {
                self.land(&mut run, cursor, cursor_root)?;
                self.stop_plain(&run, cursor, pick, act, Stop::Edit, q)?;
                return Ok(RebaseOutcome::Stopped {
                    kind: Stop::Edit,
                    commit: cursor,
                    detail: String::new(),
                });
            }
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
        act: Act,
        rest: Vec<Step>,
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
            cur_act: act,
            stop: Stop::Conflict,
            chain_squash: run.chain_squash,
            interactive: run.interactive,
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
        let message = format!(
            "rebase{} {}/{} onto {}",
            if run.interactive { " -i" } else { "" },
            run.zone,
            run.branch,
            run.onto_label
        );
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
    /// A `squash` is in the current fold chain.
    chain_squash: bool,
    interactive: bool,
}

enum Built {
    Ok(ObjectId),
    EditorFailed(String),
}
