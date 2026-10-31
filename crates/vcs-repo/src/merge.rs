//! `merge`: fast-forward or three-way merge of one zone, with a persisted conflict state.
//!
//! In-progress merges live in `.vcs/conflicts` (framed, atomic). It records the zone, the
//! branch, both heads (the second head becomes the second parent of the merge commit), the
//! paths the merge touched (for `--abort`) and every conflict with its base/left/right ids.
//! The working tree holds merged files plus conflicted files with diff3 markers, so no data
//! is ever dropped. A conflict is resolved when the user `vcs add`s the path (for marker
//! files the staged content must differ from the marker file). The state file is ignored
//! when the branch head no longer equals the recorded "ours" head (stale after a crash
//! between the ref update and its removal).

use crate::error::{io, msg, Error, Result};
use crate::index::Index;
use crate::meta::{frame, unframe, write_atomic, Refs};
use crate::repo::{fresh_change_id, Repo};
use crate::treeops::{self, Flat};
use std::collections::BTreeSet;
use std::fs;
use vcs_merge::{
    merge3, merge_base, merge_flat_with, render_markers, Algorithm, Conflict, ConflictKind, MarkerLabels, StoreBlobs, StoreGraph, TextMerge,
};
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::names::validate_path;
use vcs_objects::{Commit, EntryMode, Object, ObjectId, RefUpdate, Signature, Transaction};
use vcs_store::ObjectStore;

const FILE: &str = "conflicts";
const MAGIC: &[u8; 4] = b"AGMC";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictRec {
    pub path: String,
    pub kind: ConflictKind,
    pub base: Option<ObjectId>,
    pub left: Option<ObjectId>,
    pub right: Option<ObjectId>,
    pub base_mode: Option<EntryMode>,
    pub left_mode: Option<EntryMode>,
    pub right_mode: Option<EntryMode>,
    /// Blob materialized in the working tree (and staged) when the merge stopped.
    pub placeholder: ObjectId,
    /// The placeholder contains conflict markers; committing it unchanged is refused.
    pub has_markers: bool,
    pub resolved: bool,
}

/// How `Repo::resolve` settles one conflicted path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    Ours,
    Theirs,
    Base,
    /// Take the content of this file.
    File(std::path::PathBuf),
    /// Accept the working-tree file as the hand-edited result.
    Mark,
}

/// Outcome of `Repo::resolve_check`; both lists empty means ready to finish.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolveCheck {
    pub unresolved: Vec<String>,
    pub with_markers: Vec<String>,
}

impl ResolveCheck {
    pub fn is_clean(&self) -> bool {
        self.unresolved.is_empty() && self.with_markers.is_empty()
    }
}

/// True when text contains a `<<<<<<<` line followed later by a `>>>>>>>` line (binary: false).
pub fn has_conflict_markers(data: &[u8]) -> bool {
    if data[..data.len().min(8000)].contains(&0) {
        return false;
    }
    let is = |l: &[u8], m: &[u8]| l.starts_with(m) && matches!(l.get(m.len()), None | Some(b' ' | b'\r' | b'\n'));
    let mut open = false;
    for l in data.split(|b| *b == b'\n') {
        if is(l, b"<<<<<<<") {
            open = true;
        } else if open && is(l, b">>>>>>>") {
            return true;
        }
    }
    false
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeState {
    pub zone: String,
    pub branch: String,
    /// Branch head when the merge started.
    pub ours: ObjectId,
    /// Head being merged in (second parent).
    pub theirs: ObjectId,
    pub theirs_label: String,
    /// Every path the merge wrote, removed or conflicted (what `--abort` may discard).
    pub touched: Vec<String>,
    pub conflicts: Vec<ConflictRec>,
}

impl MergeState {
    pub fn unresolved(&self) -> Vec<&ConflictRec> {
        self.conflicts.iter().filter(|c| !c.resolved).collect()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    UpToDate,
    FastForward {
        from: Option<ObjectId>,
        to: ObjectId,
    },
    Merged {
        commit: ObjectId,
    },
    /// Stopped with unresolved conflicts (paths); the repo is in the merging state.
    Conflicted {
        paths: Vec<String>,
    },
}

fn kind_u8(k: ConflictKind) -> u8 {
    match k {
        ConflictKind::AddAdd => 0,
        ConflictKind::ModifyModify => 1,
        ConflictKind::DeleteModify => 2,
        ConflictKind::ModeConflict => 3,
        ConflictKind::FileDirectory => 4,
    }
}
fn kind_from(b: u8) -> Result<ConflictKind> {
    Ok(match b {
        0 => ConflictKind::AddAdd,
        1 => ConflictKind::ModifyModify,
        2 => ConflictKind::DeleteModify,
        3 => ConflictKind::ModeConflict,
        4 => ConflictKind::FileDirectory,
        _ => return Err(Error::Corrupt("conflicts: bad kind".into())),
    })
}
pub fn kind_name(k: ConflictKind) -> &'static str {
    match k {
        ConflictKind::AddAdd => "add/add",
        ConflictKind::ModifyModify => "modify/modify",
        ConflictKind::DeleteModify => "delete/modify",
        ConflictKind::ModeConflict => "mode",
        ConflictKind::FileDirectory => "file/directory",
    }
}
fn mode_u8(m: Option<EntryMode>) -> u8 {
    m.map_or(0, |m| m as u8)
}
fn mode_from(b: u8) -> Result<Option<EntryMode>> {
    Ok(match b {
        0 => None,
        1 => Some(EntryMode::File),
        2 => Some(EntryMode::Executable),
        _ => return Err(Error::Corrupt("conflicts: bad mode".into())),
    })
}
fn put_opt_id(w: &mut Writer, i: &Option<ObjectId>) {
    match i {
        Some(i) => {
            w.u8(1).id(i);
        }
        None => {
            w.u8(0);
        }
    }
}
fn dec(e: vcs_objects::DecodeError) -> Error {
    Error::Corrupt(format!("conflicts: {e}"))
}
fn get_opt_id(r: &mut Reader) -> Result<Option<ObjectId>> {
    match r.u8().map_err(dec)? {
        0 => Ok(None),
        1 => Ok(Some(r.id().map_err(dec)?)),
        _ => Err(Error::Corrupt("conflicts: bad option tag".into())),
    }
}

impl MergeState {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.str(&self.zone)
            .str(&self.branch)
            .id(&self.ours)
            .id(&self.theirs)
            .str(&self.theirs_label);
        w.varint(self.touched.len() as u64);
        for t in &self.touched {
            w.str(t);
        }
        w.varint(self.conflicts.len() as u64);
        for c in &self.conflicts {
            w.str(&c.path).u8(kind_u8(c.kind));
            put_opt_id(&mut w, &c.base);
            put_opt_id(&mut w, &c.left);
            put_opt_id(&mut w, &c.right);
            w.u8(mode_u8(c.base_mode)).u8(mode_u8(c.left_mode)).u8(mode_u8(c.right_mode));
            w.id(&c.placeholder).u8(c.has_markers as u8).u8(c.resolved as u8);
        }
        frame(MAGIC, w.finish())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let body = unframe(MAGIC, bytes, "conflicts")?;
        let mut r = Reader::new(body);
        let zone = r.str(64).map_err(dec)?;
        let branch = r.str(256).map_err(dec)?;
        let ours = r.id().map_err(dec)?;
        let theirs = r.id().map_err(dec)?;
        let theirs_label = r.str(512).map_err(dec)?;
        let n = r.count(1).map_err(dec)?;
        let mut touched = Vec::new();
        for _ in 0..n {
            touched.push(r.str(4096).map_err(dec)?);
        }
        let n = r.count(8).map_err(dec)?;
        let mut conflicts = Vec::new();
        for _ in 0..n {
            let path = r.str(4096).map_err(dec)?;
            let kind = kind_from(r.u8().map_err(dec)?)?;
            let (base, left, right) = (get_opt_id(&mut r)?, get_opt_id(&mut r)?, get_opt_id(&mut r)?);
            let (bm, lm, rm) = (r.u8().map_err(dec)?, r.u8().map_err(dec)?, r.u8().map_err(dec)?);
            let placeholder = r.id().map_err(dec)?;
            let (has_markers, resolved) = (r.u8().map_err(dec)? != 0, r.u8().map_err(dec)? != 0);
            conflicts.push(ConflictRec {
                path,
                kind,
                base,
                left,
                right,
                base_mode: mode_from(bm)?,
                left_mode: mode_from(lm)?,
                right_mode: mode_from(rm)?,
                placeholder,
                has_markers,
                resolved,
            });
        }
        r.finish().map_err(dec)?;
        Ok(MergeState {
            zone,
            branch,
            ours,
            theirs,
            theirs_label,
            touched,
            conflicts,
        })
    }
}

pub(crate) fn me(e: vcs_merge::MergeError) -> Error {
    Error::Msg(format!("merge: {e}"))
}

impl Repo {
    /// The in-progress merge, if any. A state file whose "ours" head is no longer the branch
    /// head is stale (the merge commit landed but cleanup was interrupted) and ignored.
    pub fn merge_state(&self) -> Result<Option<MergeState>> {
        let p = self.vcs.join(FILE);
        let bytes = match fs::read(&p) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(Error::Io { path: p, source }),
        };
        let st = MergeState::decode(&bytes)?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        if ws.subs.get(&st.zone) != Some(&st.branch) || refs.head(&st.zone, &st.branch) != Some(st.ours) {
            return Ok(None);
        }
        Ok(Some(st))
    }

    pub(crate) fn save_merge_state(&self, st: &MergeState) -> Result<()> {
        self.store.sync()?;
        write_atomic(&self.vcs.join(FILE), &st.encode())
    }

    pub(crate) fn clear_merge_state(&self) -> Result<()> {
        self.clear_pick_marker()?;
        let p = self.vcs.join(FILE);
        match fs::remove_file(&p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(Error::Io { path: p, source }),
        }
    }

    /// Called by `commit` (lock held): refuse while conflicts remain unresolved.
    pub(crate) fn merge_commit_guard(&self) -> Result<Option<MergeState>> {
        let Some(st) = self.merge_state()? else { return Ok(None) };
        let un = st.unresolved();
        if !un.is_empty() {
            let list: Vec<String> = un.iter().map(|c| format!("  {} ({})", c.path, kind_name(c.kind))).collect();
            return msg(format!(
                "cannot commit: merge of {} into {}/{} has {} unresolved conflict(s):\n{}\n\nOptions:\n  edit the files, then `vcs add <path>`\n  vcs merge --abort",
                st.theirs_label,
                st.zone,
                st.branch,
                un.len(),
                list.join("\n")
            ));
        }
        Ok(Some(st))
    }

    /// Called by `add` (lock held, before the index is saved): mark conflicts resolved.
    pub(crate) fn note_resolved(&self, rel_paths: &[String], ix: &Index) -> Result<()> {
        let Some(mut st) = self.merge_state()? else { return Ok(()) };
        let mut changed = false;
        for c in st.conflicts.iter_mut().filter(|c| !c.resolved) {
            let hit = rel_paths
                .iter()
                .any(|p| p.is_empty() || *p == c.path || c.path.starts_with(&format!("{p}/")));
            if !hit {
                continue;
            }
            let ok = match ix.entries.get(&c.path) {
                None => true,
                Some(e) => !(c.has_markers && e.id == c.placeholder),
            };
            if ok {
                c.resolved = true;
                changed = true;
            }
        }
        if changed {
            self.save_merge_state(&st)?;
        }
        Ok(())
    }

    /// Merge local branch `zone/branch` into the checked-out branch of `zone`.
    pub fn merge(&self, zone: &str, branch: &str, author: Signature) -> Result<MergeOutcome> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        let refs = self.refs()?;
        let Some(theirs) = refs.head(zone, branch) else {
            return msg(format!("no such branch {zone}/{branch}"));
        };
        self.merge_commit(zone, theirs, branch, &format!("Merge branch '{branch}'"), author)
    }

    /// Merge the remote-tracking branch `zone/branch` (as last fetched) into `zone`.
    pub fn merge_remote(&self, zone: &str, branch: &str, author: Signature) -> Result<MergeOutcome> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        let Some(theirs) = self.remote_refs()?.head(zone, branch) else {
            return msg(format!("no remote-tracking branch {zone}/{branch} (run `vcs fetch`)"));
        };
        let label = format!("origin/{branch}");
        self.merge_commit(zone, theirs, &label, &format!("Merge remote-tracking branch '{label}'"), author)
    }

    fn merge_commit(&self, zone: &str, theirs: ObjectId, label: &str, msg_prefix: &str, author: Signature) -> Result<MergeOutcome> {
        let _l = self.lock()?;
        self.rebase_guard("merge")?;
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
        let prefix = zones.path_of(zone).unwrap_or("").to_owned();
        let status = self.status()?;
        self.require_clean(zone, &status)?;
        let their_commit = self.commit_obj(&theirs)?;
        if their_commit.zone != zone {
            return msg(format!("{label} belongs to zone {:?}, not {zone:?}", their_commit.zone));
        }
        let ours = refs.head(zone, &branch);
        let mut ix = self.index()?;
        let old = self.head_flat(zone, &zones, &ws, &refs)?;
        let Some(ours) = ours else {
            return self.fast_forward(zone, &branch, None, theirs, &old, &mut refs, &mut ix);
        };
        let g = StoreGraph(&self.store);
        let bases = merge_base(&g, &ours, &theirs).map_err(me)?;
        let base = match bases.as_slice() {
            [] => {
                return msg(format!(
                    "refusing to merge unrelated histories ({label} shares no commit with {zone}/{branch})"
                ))
            }
            [b] if *b == theirs => return Ok(MergeOutcome::UpToDate),
            [b] if *b == ours => return self.fast_forward(zone, &branch, Some(ours), theirs, &old, &mut refs, &mut ix),
            [b] => *b,
            many => {
                let ids: Vec<String> = many.iter().map(|b| b.short()).collect();
                return msg(format!(
                    "criss-cross history: {zone}/{branch} and {label} have {} merge bases ({}); recursive merge is not supported",
                    many.len(),
                    ids.join(", ")
                ));
            }
        };
        let root_of = |id: &ObjectId| -> Result<Flat> { treeops::flatten(&self.store, Some(self.commit_obj(id)?.root), &prefix) };
        let (fb, fr) = (root_of(&base)?, root_of(&theirs)?);
        let tm = merge_flat_with(&StoreBlobs(&self.store), Algorithm::Myers, self.structural, &fb, &old, &fr).map_err(me)?;
        if let Some(c) = tm.conflicts.iter().find(|c| c.kind == ConflictKind::FileDirectory) {
            return msg(format!(
                "{:?} is a file on one side and a directory on the other; file/directory conflicts are not supported yet (nothing was changed)",
                c.path
            ));
        }
        for (id, data) in &tm.new_blobs {
            let got = self.store.put_blob(data)?;
            if got != *id {
                return Err(Error::Corrupt("merged blob id mismatch".into()));
            }
        }
        let mut new = tm.entries.clone();
        let mut recs = Vec::new();
        let left_label = format!("ours ({zone}/{branch})");
        let labels = MarkerLabels {
            left: &left_label,
            base: "base",
            right: label,
        };
        for c in &tm.conflicts {
            let (mode, id, markers) = self.placeholder(c, &labels)?;
            new.insert(c.path.clone(), (mode, id));
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
        if recs.is_empty() {
            let entries: Vec<(&str, EntryMode, ObjectId)> = new.iter().map(|(p, (m, i))| (zones.relative(zone, p), *m, *i)).collect();
            let root = treeops::build(&self.store, entries)?;
            let message = format!("{msg_prefix} into {branch}");
            let commit = Commit {
                zone: zone.to_owned(),
                parents: vec![ours, theirs],
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
                message,
                lock_ops: vec![],
            }))?;
            self.transition(zone, &zones, &old, &new, &mut ix)?;
            self.save_index(&mut ix)?;
            refs.apply(&[update])?;
            self.store.sync()?; // merge commit + transaction durable before refs name them
            write_atomic(&self.vcs.join("refs"), &refs.encode())?;
            self.record_op(&format!("{msg_prefix} into {zone}/{branch}"), None)?;
            return Ok(MergeOutcome::Merged { commit: cid });
        }
        let touched: BTreeSet<String> = old
            .keys()
            .chain(new.keys())
            .filter(|p| old.get(*p) != new.get(*p))
            .cloned()
            .chain(recs.iter().map(|r| r.path.clone()))
            .collect();
        let state = MergeState {
            zone: zone.to_owned(),
            branch: branch.clone(),
            ours,
            theirs,
            theirs_label: label.to_owned(),
            touched: touched.into_iter().collect(),
            conflicts: recs,
        };
        // State first: a crash afterwards is recoverable with `vcs merge --abort`.
        self.clear_pick_marker()?; // a leftover marker must never turn this merge into a pick
        self.save_merge_state(&state)?;
        if let Err(e) = self.transition(zone, &zones, &old, &new, &mut ix) {
            self.clear_merge_state()?; // preflight failed before touching files
            return Err(e);
        }
        self.save_index(&mut ix)?;
        Ok(MergeOutcome::Conflicted {
            paths: state.conflicts.iter().map(|c| c.path.clone()).collect(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn fast_forward(
        &self,
        zone: &str,
        branch: &str,
        from: Option<ObjectId>,
        to: ObjectId,
        old: &Flat,
        refs: &mut Refs,
        ix: &mut Index,
    ) -> Result<MergeOutcome> {
        let zones = self.zones()?;
        let root = self.commit_obj(&to)?.root;
        let new = treeops::flatten(&self.store, Some(root), zones.path_of(zone).unwrap_or(""))?;
        self.transition(zone, &zones, old, &new, ix)?;
        self.save_index(ix)?;
        refs.apply(&[RefUpdate {
            zone: zone.into(),
            branch: branch.into(),
            old: from,
            new: to,
        }])?;
        write_atomic(&self.vcs.join("refs"), &refs.encode())?;
        self.record_op(&format!("merge: fast-forward {zone}/{branch}"), None)?;
        Ok(MergeOutcome::FastForward { from, to })
    }

    /// What to materialize for a conflict: (mode, blob, contains markers).
    pub(crate) fn placeholder(&self, c: &Conflict, labels: &MarkerLabels) -> Result<(EntryMode, ObjectId, bool)> {
        if let Some(h) = &c.hunks {
            let bytes = render_markers(h, labels);
            let id = self.store.put_blob(&bytes)?;
            let mode = c.left_mode.or(c.right_mode).unwrap_or(EntryMode::File);
            return Ok((mode, id, true));
        }
        match (c.left, c.right) {
            (Some(l), _) => Ok((c.left_mode.unwrap_or(EntryMode::File), l, false)),
            (None, Some(r)) => Ok((c.right_mode.unwrap_or(EntryMode::File), r, false)),
            (None, None) => Err(Error::Corrupt("conflict without content".into())),
        }
    }

    /// Discard an in-progress merge and restore the pre-merge state exactly. Refuses if
    /// anything outside the merge's own paths changed.
    pub fn merge_abort(&self) -> Result<()> {
        let _l = self.lock()?;
        self.rebase_guard("abort a merge")?;
        let Some(st) = self.merge_state()? else {
            return msg("no merge in progress");
        };
        self.restore_pre_merge(&st)?;
        self.clear_merge_state()?;
        self.record_op(&format!("merge --abort {}/{}", st.zone, st.branch), None)?;
        Ok(())
    }

    /// Put the zone's files, index and tree back to `st.ours` (lock held). Keeps the state file.
    pub(crate) fn restore_pre_merge(&self, st: &MergeState) -> Result<()> {
        let zones = self.zones()?;
        let prefix = zones.path_of(&st.zone).unwrap_or("").to_owned();
        let status = self.status()?;
        let a = status.zones.iter().find(|a| a.zone == st.zone).expect("subscribed");
        let touched: BTreeSet<&String> = st.touched.iter().collect();
        let foreign: Vec<&str> = a
            .staged
            .iter()
            .chain(a.unstaged.iter())
            .filter(|c| !touched.contains(&c.path))
            .map(|c| c.path.as_str())
            .collect();
        if !foreign.is_empty() {
            return msg(format!(
                "cannot abort: files outside the merge changed ({}); commit or revert them first",
                foreign.join(", ")
            ));
        }
        let ours_root = self.commit_obj(&st.ours)?.root;
        let ours = treeops::flatten(&self.store, Some(ours_root), &prefix)?;
        let mut ix = self.index()?;
        let mut dirs = BTreeSet::new();
        for p in &st.touched {
            let abs = self.worktree.join(p);
            match fs::symlink_metadata(&abs) {
                Ok(m) if m.is_file() => io(&abs, fs::remove_file(&abs))?,
                Ok(_) => return msg(format!("cannot abort: {p:?} is no longer a regular file")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(Error::Io { path: abs, source }),
            }
            if let Some(par) = abs.parent() {
                dirs.insert(par.to_owned());
            }
            ix.entries.remove(p);
        }
        let cur: Flat = ix
            .entries
            .iter()
            .filter(|(p, _)| zones.owner(p) == st.zone)
            .map(|(p, e)| (p.clone(), (e.mode, e.id)))
            .collect();
        self.transition(&st.zone, &zones, &cur, &ours, &mut ix)?;
        for mut d in dirs {
            while d != self.worktree && d.starts_with(&self.worktree) && fs::remove_dir(&d).is_ok() {
                if !d.pop() {
                    break;
                }
            }
        }
        self.save_index(&mut ix)?;
        Ok(())
    }

    /// Resolve one conflicted path of the in-progress merge or rebase (they share this state).
    /// "Ours" is the recorded left side (merge: the current branch; rebase: the branch being
    /// rebased onto), "theirs" the right side. Choosing a side that does not have the file
    /// removes it. The result is staged and the conflict marked resolved.
    pub fn resolve(&self, path: &str, how: &Resolution) -> Result<()> {
        let st = self
            .merge_state()?
            .ok_or_else(|| Error::Msg("no merge or rebase in progress".into()))?;
        let c = st
            .conflicts
            .iter()
            .find(|c| c.path == path)
            .ok_or_else(|| Error::Msg(format!("{path:?} is not conflicted")))?
            .clone();
        validate_path(path)?;
        let abs = self.worktree.join(path);
        let pick = |id: Option<ObjectId>, mode: Option<EntryMode>| -> Result<Option<(Vec<u8>, EntryMode)>> {
            id.map(|id| Ok((self.store.get_blob(&id)?, mode.unwrap_or(EntryMode::File))))
                .transpose()
        };
        let content = match how {
            Resolution::Mark => {
                self.add(&[path.to_owned()])?;
                let st = self
                    .merge_state()?
                    .ok_or_else(|| Error::Msg("no merge or rebase in progress".into()))?;
                return if st.conflicts.iter().any(|x| x.path == path && x.resolved) {
                    Ok(())
                } else {
                    msg(format!(
                        "{path:?} still holds the conflict markers it was written with; edit it, or choose --ours/--theirs/--base/--file"
                    ))
                };
            }
            Resolution::Ours => pick(c.left, c.left_mode)?,
            Resolution::Theirs => pick(c.right, c.right_mode)?,
            Resolution::Base => pick(c.base, c.base_mode)?,
            Resolution::File(f) => {
                let data = fs::read(f).map_err(|source| Error::Io { path: f.clone(), source })?;
                Some((data, c.left_mode.or(c.right_mode).unwrap_or(EntryMode::File)))
            }
        };
        match content {
            None => match fs::symlink_metadata(&abs) {
                Ok(m) if m.is_file() => io(&abs, fs::remove_file(&abs))?,
                Ok(_) => return msg(format!("{path:?} is not a regular file")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(Error::Io { path: abs, source }),
            },
            Some((data, mode)) => {
                if let Ok(m) = fs::symlink_metadata(&abs) {
                    if !m.is_file() {
                        return msg(format!("{path:?} is not a regular file"));
                    }
                }
                let parent = abs.parent().expect("file has a parent");
                io(parent, fs::create_dir_all(parent))?;
                let tmp = parent.join(format!(".vcs-tmp-{}", std::process::id()));
                io(&tmp, fs::write(&tmp, &data))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let bits = if mode == EntryMode::Executable { 0o755 } else { 0o644 };
                    io(&tmp, fs::set_permissions(&tmp, fs::Permissions::from_mode(bits)))?;
                }
                #[cfg(not(unix))]
                let _ = mode;
                io(&abs, fs::rename(&tmp, &abs))?;
            }
        }
        if self.index()?.entries.contains_key(path) || abs.exists() {
            self.add(&[path.to_owned()])?;
        }
        // An explicit choice always counts, even when the staged content equals the placeholder.
        let _l = self.lock()?;
        let Some(mut st) = self.merge_state()? else { return Ok(()) };
        if let Some(x) = st.conflicts.iter_mut().find(|x| x.path == path) {
            x.resolved = true;
        }
        self.save_merge_state(&st)
    }

    /// Verify the in-progress merge or rebase is ready to finish: every conflict resolved and no
    /// conflict markers left in any staged resolved file.
    pub fn resolve_check(&self) -> Result<ResolveCheck> {
        let st = self
            .merge_state()?
            .ok_or_else(|| Error::Msg("no merge or rebase in progress".into()))?;
        let ix = self.index()?;
        let mut rep = ResolveCheck::default();
        for c in &st.conflicts {
            if !c.resolved {
                rep.unresolved.push(c.path.clone());
                continue;
            }
            if let Some(e) = ix.entries.get(&c.path) {
                if has_conflict_markers(&self.store.get_blob(&e.id)?) {
                    rep.with_markers.push(c.path.clone());
                }
            }
        }
        Ok(rep)
    }

    /// Conflicts of the in-progress merge.
    pub fn conflicts(&self) -> Result<Vec<ConflictRec>> {
        match self.merge_state()? {
            Some(s) => Ok(s.conflicts),
            None => msg("no merge in progress"),
        }
    }

    /// Human-readable description of one conflict (ids plus a diff3 rendering for text).
    pub fn conflict_show(&self, path: &str) -> Result<String> {
        let st = self.merge_state()?.ok_or_else(|| Error::Msg("no merge in progress".into()))?;
        let c = st
            .conflicts
            .iter()
            .find(|c| c.path == path)
            .ok_or_else(|| Error::Msg(format!("{path:?} is not conflicted")))?;
        let f = |i: &Option<ObjectId>| i.map_or("(absent)".to_owned(), |i| i.short());
        let mut out = format!(
            "{} ({}, {})\n  base:   {}\n  ours:   {}\n  theirs: {} [{}]\n",
            c.path,
            kind_name(c.kind),
            if c.resolved { "resolved" } else { "unresolved" },
            f(&c.base),
            f(&c.left),
            f(&c.right),
            st.theirs_label
        );
        let get = |i: &Option<ObjectId>| -> Result<Option<Vec<u8>>> { i.map(|i| Ok(self.store.get_blob(&i)?)).transpose() };
        let (b, l, r) = (get(&c.base)?, get(&c.left)?, get(&c.right)?);
        let bin = [&b, &l, &r]
            .iter()
            .any(|x| x.as_ref().is_some_and(|v| v[..v.len().min(8000)].contains(&0)));
        if bin {
            out.push_str("  (binary content; choose a side and `vcs add` it)\n");
        } else if let (Some(l), Some(r)) = (&l, &r) {
            let b = b.unwrap_or_default();
            let left_label = format!("ours ({}/{})", st.zone, st.branch);
            let labels = MarkerLabels {
                left: &left_label,
                base: "base",
                right: &st.theirs_label,
            };
            let hunks = match merge3(Algorithm::Myers, &b, l, r) {
                TextMerge::Clean(bytes) => vec![vcs_merge::Hunk::Clean(vec![bytes])],
                TextMerge::Conflicted(h) => h,
            };
            out.push('\n');
            out.push_str(&String::from_utf8_lossy(&render_markers(&hunks, &labels)));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod marker_tests {
    use super::has_conflict_markers as h;

    #[test]
    fn detects_marker_blocks_only() {
        assert!(h(b"a\n<<<<<<< ours\nx\n=======\ny\n>>>>>>> theirs\nb\n"));
        assert!(h(b"<<<<<<<\n>>>>>>>"));
        assert!(!h(b"=======\ntitle\n=======\n"));
        assert!(!h(b"<<<<<<< only open\n"));
        assert!(!h(b">>>>>>> x\n<<<<<<< y\n"));
        assert!(!h(b"<<<<<<<<< not seven\n>>>>>>>>>\n"));
        assert!(!h(b"<<<<<<< x\n>>>>>>> y\n\0"));
    }
}
