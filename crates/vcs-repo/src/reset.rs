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
        let new = treeops::flatten(&self.store, Some(tc.root), &prefix)?;
        let mut index_changed = 0;
        let mut files_changed = 0;
        if mode != ResetMode::Soft {
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
                    let data = self.store.get_content(&id)?;
                    let st = if m == EntryMode::Symlink {
                        crate::portable::check_symlink_target(p, &data)
                            .or_else(|why| msg(format!("refusing to restore symlink {p:?}: {why}")))?;
                        io(&self.worktree.join(p), self.wfs.symlink(&data, p))?
                    } else {
                        io(&self.worktree.join(p), self.wfs.write(p, &data, m))?
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
                    let size = self.store.get_content(&id)?.len() as u64;
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
