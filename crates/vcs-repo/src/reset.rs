//! `reset` (move a branch head, optionally resetting the index and working tree) and `stash apply`.
//!
//! Scope is ONE zone, like `switch`: git's `reset` moves "the" HEAD; here every zone has its own
//! branch heads, so `reset` moves the checked-out branch of the named zone and touches only the
//! index entries and files that zone owns (docs/git-to-vcs.md).
//!
//! * `Soft`: only the branch head moves. Index and files stay, so the difference shows as staged.
//! * `Mixed`: the head moves and the zone's index entries become the target's. Files stay (they
//!   show as unstaged changes; files the target lacks become untracked).
//! * `Hard`: head, index and files all become the target's. Uncommitted work in the zone is
//!   DISCARDED (the operation log does not hold working-tree contents).
//!
//! Files are written before the refs move, so an interrupted hard reset leaves the old head and
//! running it again completes it. Refused during a merge or rebase.

use crate::error::{io, msg, Result};
use crate::index::IndexEntry;
use crate::repo::{Repo, Wt};
use crate::stash::StashPopOutcome;
use crate::treeops;
use std::collections::BTreeSet;
use vcs_objects::{EntryMode, ObjectId, RefUpdate};
use vcs_store::ObjectStore;

/// Most differing paths for which a reset looks each one up instead of walking whole trees.
const MAX_DIFF_PATHS: usize = 4096;

thread_local! {
    static DIFF_LIMIT: std::cell::Cell<usize> = const { std::cell::Cell::new(MAX_DIFF_PATHS) };
}

/// Override [`MAX_DIFF_PATHS`] for this thread (tests: 0 forces the flat walk).
#[doc(hidden)]
pub fn set_reset_diff_limit(n: Option<usize>) {
    DIFF_LIMIT.with(|l| l.set(n.unwrap_or(MAX_DIFF_PATHS)));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetMode {
    Soft,
    Mixed,
    Hard,
}

#[derive(Debug)]
pub struct ResetReport {
    pub zone: String,
    pub branch: String,
    pub old: Option<ObjectId>,
    pub new: ObjectId,
    /// Index entries changed (mixed, hard).
    pub index_changed: usize,
    /// Files written or removed (hard).
    pub files_changed: usize,
}

impl Repo {
    /// Move the checked-out branch of `zone` to `target` (a commit of that zone).
    pub fn reset(&self, zone: &str, target: &ObjectId, mode: ResetMode) -> Result<ResetReport> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        let _l = self.lock()?;
        self.rebase_guard("reset")?;
        if self.merge_state()?.is_some() {
            return msg("cannot reset: a merge is in progress\n\nOptions:\n  vcs merge --abort");
        }
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut refs = self.refs()?;
        let Some(branch) = ws.subs.get(zone).cloned() else {
            return msg(format!("zone {zone:?} is not subscribed in this workspace"));
        };
        let tc = self.commit_obj(target)?;
        if tc.zone != zone {
            return msg(format!("commit {} is not in zone {zone:?}", target.short()));
        }
        let old = refs.head(zone, &branch);
        let prefix = zones.path_of(zone).unwrap_or("").to_owned();
        let mut index_changed = 0;
        let mut files_changed = 0;
        if mode != ResetMode::Soft {
            (index_changed, files_changed) = match self.reset_index_by_diff(zone, &zones, &ws, tc.root, mode)? {
                Some(counts) => counts,
                None => self.reset_index_flat(zone, &zones, &ws, tc.root, &prefix, mode)?,
            };
        }
        if old != Some(*target) {
            refs.apply(&[RefUpdate {
                zone: zone.into(),
                branch: branch.clone(),
                old,
                new: *target,
            }])?;
            crate::meta::write_atomic(&self.common.join("refs"), &refs.encode())?;
        }
        let m = match mode {
            ResetMode::Soft => "--soft",
            ResetMode::Mixed => "--mixed",
            ResetMode::Hard => "--hard",
        };
        self.record_op(&format!("reset {m} {zone}/{branch} to {}", target.short()), None)?;
        Ok(ResetReport {
            zone: zone.into(),
            branch,
            old,
            new: *target,
            index_changed,
            files_changed,
        })
    }

    /// `reset --mixed|--hard` by walking the whole target tree and every index entry of the zone.
    /// The general path; [`Self::reset_index_by_diff`] handles the common near target.
    fn reset_index_flat(
        &self,
        zone: &str,
        zones: &crate::meta::Zones,
        ws: &crate::meta::Workspace,
        root: ObjectId,
        prefix: &str,
        mode: ResetMode,
    ) -> Result<(usize, usize)> {
        let new = treeops::flatten(&self.store, Some(root), prefix)?;
        let (mut index_changed, mut files_changed) = (0usize, 0usize);
        let mut ix = self.index()?;
        let owned: Vec<String> = ix.under("").into_iter().filter(|p| zones.owner(p) == zone).collect();
        let mut dirs = BTreeSet::new();
        // 1. entries (and for hard, files) the target does not have
        for p in &owned {
            if new.contains_key(p) {
                continue;
            }
            if mode == ResetMode::Hard && !ws.skips(zone, p) {
                match self.wfs.remove(p) {
                    Ok(()) => files_changed += 1,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(crate::error::Error::Io {
                            path: self.worktree.join(p),
                            source,
                        })
                    }
                }
                if let Some((d, _)) = p.rsplit_once('/') {
                    dirs.insert(d.to_owned());
                }
            }
            ix.entries.remove(p);
            index_changed += 1;
        }
        // 2. entries the target has
        for (p, &(m, id)) in &new {
            let cur = ix.entries.get(p).cloned();
            let same = cur.is_some_and(|e| e.id == id && e.mode == m);
            let sparse = ws.skips(zone, p);
            if mode == ResetMode::Hard && !sparse {
                if same {
                    let e = ix.entries[p].clone();
                    if matches!(self.wt_state(p, &e, ix.written_ns)?, Wt::Clean) {
                        continue;
                    }
                }
                let st = if m != EntryMode::Symlink {
                    match self.content_or_stream(p, &id, m)? {
                        crate::repo::FileContent::Streamed(e) => {
                            ix.entries.remove(p);
                            ix.insert(p, e)?;
                            files_changed += 1;
                            if !same {
                                index_changed += 1;
                            }
                            continue;
                        }
                        crate::repo::FileContent::Bytes(data) => io(&self.worktree.join(p), self.wfs.write(p, &data, m))?,
                    }
                } else {
                    let data = self.store.get_content(&id)?;
                    crate::portable::check_symlink_target(p, &data)
                        .or_else(|why| msg(format!("refusing to restore symlink {p:?}: {why}")))?;
                    io(&self.worktree.join(p), self.wfs.symlink(&data, p))?
                };
                ix.entries.remove(p);
                ix.insert(
                    p,
                    IndexEntry {
                        id,
                        mode: m,
                        size: st.len,
                        mtime_ns: st.mtime_ns,
                        ctime_ns: st.ctime_ns,
                    },
                )?;
                files_changed += 1;
                if !same {
                    index_changed += 1;
                }
            } else if !same {
                let size = self.content_len(&id)?;
                ix.entries.remove(p);
                // mtime 0 never matches the file's, so status re-reads it instead of trusting a stale stat
                ix.insert(
                    p,
                    IndexEntry {
                        id,
                        mode: m,
                        size,
                        mtime_ns: 0,
                        ctime_ns: 0,
                    },
                )?;
                index_changed += 1;
            }
        }
        self.prune_dirs_fs(dirs);
        self.save_index(&mut ix)?;
        Ok((index_changed, files_changed))
    }

    /// `reset --mixed|--hard` for a target that is close to the index: the index is compared with
    /// the target tree with subtree skipping (cached tree ids), so only the paths that differ are
    /// looked up in the target; for `--hard` the unchanged entries are checked against the files
    /// on the status worker pool. `None` (nothing touched) when more than [`MAX_DIFF_PATHS`] paths
    /// differ: the flat walk is cheaper then. Same results as [`Self::reset_index_flat`].
    fn reset_index_by_diff(
        &self,
        zone: &str,
        zones: &crate::meta::Zones,
        ws: &crate::meta::Workspace,
        root: ObjectId,
        mode: ResetMode,
    ) -> Result<Option<(usize, usize)>> {
        use crate::repo::ChangeKind;
        let prefix = zones.path_of(zone).unwrap_or("");
        let mut ix = self.index()?;
        let fp = crate::repo::zones_fp(zones);
        let (changes, fresh) = {
            let mut it = treeops::IndexTrees::new(&self.store, &ix.entries, zones, zone, ix.entries.cached_trees(&fp));
            let changes = it.diff(prefix, Some(root))?;
            (changes, it.cacheable())
        };
        if changes.len() > DIFF_LIMIT.with(|l| l.get()) {
            return Ok(None);
        }
        if !fresh.is_empty() {
            ix.entries.store_trees(fp, fresh);
        }
        let hard = mode == ResetMode::Hard;
        let (mut index_changed, mut files_changed) = (0usize, 0usize);
        let mut dirs = BTreeSet::new();
        // 1. entries (and for hard, files) the target does not have
        for (p, kind) in &changes {
            if *kind != ChangeKind::Added {
                continue;
            }
            if hard && !ws.skips(zone, p) {
                match self.wfs.remove(p) {
                    Ok(()) => files_changed += 1,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(crate::error::Error::Io {
                            path: self.worktree.join(p),
                            source,
                        })
                    }
                }
                if let Some((d, _)) = p.rsplit_once('/') {
                    dirs.insert(d.to_owned());
                }
            }
            ix.entries.remove(p);
            index_changed += 1;
        }
        // 2. entries the target has and the index lacks or holds differently
        for (p, kind) in &changes {
            if *kind == ChangeKind::Added {
                continue;
            }
            let rel = if prefix.is_empty() {
                p.as_str()
            } else {
                p.strip_prefix(prefix).and_then(|r| r.strip_prefix('/')).unwrap_or(p.as_str())
            };
            let Some((m, id)) = treeops::entry_at(&self.store, root, rel)?.filter(|e| e.0 != EntryMode::Directory) else {
                return msg(format!("internal error: {p:?} is in the tree diff but not in the target tree"));
            };
            ix.entries.remove(p);
            let entry = if hard && !ws.skips(zone, p) {
                files_changed += 1;
                self.write_target_file(p, m, id)?
            } else {
                // mtime 0 never matches the file's, so status re-reads it instead of trusting a stale stat
                IndexEntry {
                    id,
                    mode: m,
                    size: self.content_len(&id)?,
                    mtime_ns: 0,
                    ctime_ns: 0,
                }
            };
            ix.insert(p, entry)?;
            index_changed += 1;
        }
        // 3. hard: entries the target agrees with still lose local edits and deletions
        if hard {
            let touched: std::collections::HashSet<&str> = changes.iter().map(|c| c.0.as_str()).collect();
            let single = zones.names().count() <= 1;
            let list: Vec<(&String, &IndexEntry)> = ix
                .entries
                .iter()
                .filter(|(p, _)| (single || zones.owner(p) == zone) && !ws.skips(zone, p) && !touched.contains(p.as_str()))
                .collect();
            let (dirty, _) = self.unstaged_changes(&list, ix.written_ns)?;
            let redo: Vec<(String, EntryMode, ObjectId)> = dirty
                .into_iter()
                .map(|c| {
                    let e = &ix.entries[&c.path];
                    (c.path, e.mode, e.id)
                })
                .collect();
            for (p, m, id) in redo {
                let entry = self.write_target_file(&p, m, id)?;
                ix.entries.insert(p, entry);
                files_changed += 1;
            }
        }
        self.prune_dirs_fs(dirs);
        self.save_index(&mut ix)?;
        Ok(Some((index_changed, files_changed)))
    }

    /// Write the target's version of `p` into the working tree and return its index entry.
    fn write_target_file(&self, p: &str, m: EntryMode, id: ObjectId) -> Result<IndexEntry> {
        let st = if m != EntryMode::Symlink {
            match self.content_or_stream(p, &id, m)? {
                crate::repo::FileContent::Streamed(e) => return Ok(e),
                crate::repo::FileContent::Bytes(data) => io(&self.worktree.join(p), self.wfs.write(p, &data, m))?,
            }
        } else {
            let data = self.store.get_content(&id)?;
            crate::portable::check_symlink_target(p, &data).or_else(|why| msg(format!("refusing to restore symlink {p:?}: {why}")))?;
            io(&self.worktree.join(p), self.wfs.symlink(&data, p))?
        };
        Ok(IndexEntry {
            id,
            mode: m,
            size: st.len,
            mtime_ns: st.mtime_ns,
            ctime_ns: st.ctime_ns,
        })
    }

    /// Apply stash entry `n` (0 = newest) like `stash pop`, but keep the entry.
    pub fn stash_apply(&self, n: usize) -> Result<StashPopOutcome> {
        let _l = self.lock()?;
        self.rebase_guard("stash apply")?;
        let list = self.stash_list()?;
        if n >= list.len() {
            return msg(format!("no such stash entry {n} ({} saved)", list.len()));
        }
        let entry = list[n].clone();
        let out = self.apply_entry(&entry, "stash apply", "Stashed changes")?;
        self.record_op(&format!("stash apply: {}", entry.message), None)?;
        Ok(out)
    }
}
