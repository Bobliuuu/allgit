//! Linked worktrees: several working trees over one object store and one set of refs.
//! Layout, locking and the GC / fsck rules: docs/worktrees.md.
//!
//! The main worktree is the repository as it always was (`<root>/.vcs` holds everything). A linked
//! worktree has a real `.vcs` directory too, but it holds only per-worktree state (index,
//! workspace, merge / rebase / stash files) plus a `commondir` file naming the main `.vcs`, which
//! keeps the objects, refs, zones, remote state, operation log and the lock. The main repository
//! keeps a registry, `.vcs/worktrees/<name>/path`, so that gc and fsck can find every index.

use crate::error::{io, msg, Error, Result};
use crate::index::Index;
use crate::meta::{write_atomic, Workspace, ROOT_ZONE};
use crate::repo::{Counters, Repo, FORMAT_FILE, FORMAT_TEXT};
use std::fs;
use std::path::{Path, PathBuf};
use vcs_objects::names::{validate_branch_name, META_DIR};
use vcs_objects::RefUpdate;
use vcs_store::LocalStore;

const COMMONDIR: &str = "commondir";
const NAME_FILE: &str = "worktree-name";
const REGISTRY: &str = "worktrees";

/// One working tree of a repository.
#[derive(Clone, Debug)]
pub struct WorktreeInfo {
    /// Registry name; `None` for the main worktree.
    pub name: Option<String>,
    pub path: PathBuf,
    pub main: bool,
    /// The worktree this handle was opened in.
    pub current: bool,
    /// (zone, checked-out branch)
    pub subs: Vec<(String, String)>,
    /// The directory or its `.vcs` is gone (removed by hand, or on an unmounted disk).
    pub missing: bool,
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 100
        && !n.starts_with('.')
        && n.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

/// Where the shared state of the repository whose per-worktree directory is `vcs` lives.
pub(crate) fn resolve_common(vcs: &Path) -> Result<PathBuf> {
    let p = vcs.join(COMMONDIR);
    let s = match fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vcs.to_owned()),
        Err(source) => return Err(Error::Io { path: p, source }),
    };
    let common = PathBuf::from(s.trim_end_matches('\n'));
    let name = fs::read_to_string(vcs.join(NAME_FILE)).unwrap_or_default();
    let name = name.trim_end_matches('\n');
    if !common.is_absolute() || !common.join(FORMAT_FILE).is_file() || !valid_name(name) {
        return msg(format!(
            "{} is a linked worktree whose main repository ({}) cannot be found; if the repository moved, remove this directory and `vcs worktree add` again",
            vcs.display(),
            common.display()
        ));
    }
    if !common.join(REGISTRY).join(name).join("path").is_file() {
        return msg(format!(
            "this worktree is no longer registered in {}: it was removed with `vcs worktree remove`; delete this directory",
            common.display()
        ));
    }
    Ok(common)
}

impl Repo {
    fn main_worktree(&self) -> PathBuf {
        self.common.parent().map(Path::to_owned).unwrap_or_else(|| self.common.clone())
    }

    /// Registered linked worktrees as (registry name, path), sorted by name.
    fn linked_worktrees(&self) -> Result<Vec<(String, PathBuf)>> {
        let dir = self.common.join(REGISTRY);
        let rd = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(source) => return Err(Error::Io { path: dir, source }),
        };
        let mut out = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let p = e.path().join("path");
            // an entry without `path` is a worktree add that crashed before registering
            if let Ok(s) = fs::read_to_string(&p) {
                if valid_name(&name) {
                    out.push((name, PathBuf::from(s.trim_end_matches('\n'))));
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// A handle on the worktree at `wt` (main or linked) over this repository's shared state.
    fn open_view(&self, wt: &Path) -> Result<Repo> {
        let vcs = if wt == self.main_worktree() {
            self.common.clone()
        } else {
            wt.join(META_DIR)
        };
        if !vcs.join(FORMAT_FILE).is_file() {
            return msg(format!(
                "worktree {} is missing or damaged (no {}); if it is gone, run `vcs worktree remove --force {}`",
                wt.display(),
                vcs.display(),
                wt.display()
            ));
        }
        Ok(Repo {
            worktree: wt.to_owned(),
            wfs: std::sync::Arc::new(crate::worktree_fs::RealFs::new(wt.to_owned())),
            vcs,
            common: self.common.clone(),
            store: LocalStore::open(self.common.join("objects"))?,
            structural: true,
            renames: false,
            use_commit_graph: true,
            index_racy_ns: self.index_racy_ns,
            counters: Counters::default(),
        })
    }

    /// Every worktree (main first) as a handle, for gc and fsck. Fails if one cannot be read:
    /// deleting objects while a worktree's index cannot be seen is never safe.
    pub(crate) fn worktree_views(&self) -> Result<Vec<Repo>> {
        let mut v = vec![self.open_view(&self.main_worktree())?];
        for (_, p) in self.linked_worktrees()? {
            v.push(self.open_view(&p)?);
        }
        Ok(v)
    }

    pub(crate) fn require_no_linked_worktrees(&self, what: &str) -> Result<()> {
        if self.linked_worktrees()?.is_empty() {
            return Ok(());
        }
        msg(format!(
            "{what} is not available while linked worktrees exist (it rewrites every branch at once); remove them with `vcs worktree remove`"
        ))
    }

    /// The other worktree that has `zone/branch` checked out, if any.
    pub(crate) fn branch_holder(&self, zone: &str, branch: &str) -> Result<Option<PathBuf>> {
        let mut all = vec![self.main_worktree()];
        all.extend(self.linked_worktrees()?.into_iter().map(|(_, p)| p));
        for wt in all {
            if wt == self.worktree {
                continue;
            }
            let f = if wt == self.main_worktree() {
                self.common.join("workspace")
            } else {
                wt.join(META_DIR).join("workspace")
            };
            let bytes = match fs::read(&f) {
                Ok(b) => b,
                // a vanished worktree holds nothing; `worktree remove` clears its registry entry
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(Error::Io { path: f, source }),
            };
            if Workspace::decode(&bytes)?.subs.get(zone).map(String::as_str) == Some(branch) {
                return Ok(Some(wt));
            }
        }
        Ok(None)
    }

    /// Refuse to check `zone/branch` out here when another worktree already has it: two working
    /// trees committing to one branch would silently overwrite each other's work.
    pub(crate) fn require_branch_free(&self, zone: &str, branch: &str) -> Result<()> {
        match self.branch_holder(zone, branch)? {
            None => Ok(()),
            Some(p) => msg(format!(
                "branch {zone}/{branch} is already checked out in the worktree {}; use another branch (`vcs branch create`) or a detached copy",
                p.display()
            )),
        }
    }

    /// All worktrees, main first.
    pub fn worktree_list(&self) -> Result<Vec<WorktreeInfo>> {
        let info = |name: Option<String>, path: PathBuf, main: bool| {
            let ws = if main {
                self.common.join("workspace")
            } else {
                path.join(META_DIR).join("workspace")
            };
            let (subs, missing) = match fs::read(&ws).ok().and_then(|b| Workspace::decode(&b).ok()) {
                Some(w) => (w.subs.into_iter().collect(), false),
                None => (vec![], true),
            };
            WorktreeInfo {
                name,
                current: path == self.worktree,
                path,
                main,
                subs,
                missing,
            }
        };
        let mut v = vec![info(None, self.main_worktree(), true)];
        for (n, p) in self.linked_worktrees()? {
            v.push(info(Some(n), p, false));
        }
        Ok(v)
    }

    /// Create a linked worktree at `path` with the root zone on `branch` (default: the directory's
    /// name; created at this worktree's root-zone head when it does not exist yet). Other zones are
    /// added later with `vcs zone add`. `path` must be a new or empty directory outside every
    /// existing worktree.
    pub fn worktree_add(&self, path: &Path, branch: Option<&str>) -> Result<WorktreeInfo> {
        let _l = self.lock()?;
        let abs = if path.is_absolute() {
            path.to_owned()
        } else {
            io(path, std::env::current_dir())?.join(path)
        };
        let Some(base) = abs.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            return msg("worktree path needs a directory name");
        };
        let existed = abs.exists();
        if existed && (!abs.is_dir() || io(&abs, fs::read_dir(&abs))?.next().is_some()) {
            return msg(format!("{} exists and is not an empty directory", abs.display()));
        }
        let abs = if existed {
            io(&abs, abs.canonicalize())?
        } else {
            let parent = abs.parent().unwrap_or(Path::new("."));
            io(parent, parent.canonicalize())?.join(&base)
        };
        let mut all = vec![self.main_worktree()];
        all.extend(self.linked_worktrees()?.into_iter().map(|(_, p)| p));
        if let Some(w) = all.iter().find(|w| abs.starts_with(w) || w.starts_with(&abs)) {
            return msg(format!(
                "{} overlaps the worktree {}; worktrees may not be nested",
                abs.display(),
                w.display()
            ));
        }
        let branch = branch.unwrap_or(&base).to_owned();
        validate_branch_name(&branch)?;
        self.require_branch_free(ROOT_ZONE, &branch)?;
        let mut refs = self.refs()?;
        if refs.head(ROOT_ZONE, &branch).is_none() {
            let ws = self.workspace()?;
            let Some(head) = self.current_head(ROOT_ZONE, &ws, &refs) else {
                return msg("the root zone has no commits yet; commit first");
            };
            refs.apply(&[RefUpdate {
                zone: ROOT_ZONE.into(),
                branch: branch.clone(),
                old: None,
                new: head,
            }])?;
            write_atomic(&self.common.join("refs"), &refs.encode())?;
        }
        // Reserve a registry name (create_dir is atomic).
        let sane: String = base
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let sane = sane.trim_start_matches('.').chars().take(80).collect::<String>();
        let sane = if sane.is_empty() { "wt".to_owned() } else { sane };
        let reg_root = self.common.join(REGISTRY);
        io(&reg_root, fs::create_dir_all(&reg_root))?;
        let (name, reg) = (0..1000)
            .find_map(|i| {
                let n = if i == 0 { sane.clone() } else { format!("{sane}-{}", i + 1) };
                let d = reg_root.join(&n);
                fs::create_dir(&d).ok().map(|()| (n, d))
            })
            .ok_or_else(|| Error::Msg("too many worktrees with this name".into()))?;
        let built = (|| -> Result<()> {
            let vcs = abs.join(META_DIR);
            io(&vcs, fs::create_dir_all(&vcs))?;
            write_atomic(&vcs.join(COMMONDIR), self.common.to_string_lossy().as_bytes())?;
            write_atomic(&vcs.join(NAME_FILE), name.as_bytes())?;
            write_atomic(&vcs.join("index"), &Index::default().encode())?;
            write_atomic(&vcs.join("workspace"), &Workspace::default().encode())?;
            write_atomic(&vcs.join(FORMAT_FILE), FORMAT_TEXT.as_bytes())?;
            // Registered before any file is checked out, so gc sees this index from the first entry.
            write_atomic(&reg.join("path"), abs.to_string_lossy().as_bytes())?;
            let linked = self.open_view(&abs)?;
            linked.zone_add_locked(ROOT_ZONE, Some(&branch))
        })();
        if let Err(e) = built {
            let _ = fs::remove_dir_all(&reg);
            let _ = fs::remove_dir_all(abs.join(META_DIR));
            if !existed {
                let _ = fs::remove_dir_all(&abs);
            }
            return Err(e);
        }
        Ok(WorktreeInfo {
            name: Some(name),
            path: abs,
            main: false,
            current: false,
            subs: vec![(ROOT_ZONE.to_owned(), branch)],
            missing: false,
        })
    }

    /// Delete a linked worktree and its registration. Refuses (unless `force`) when it has staged,
    /// unstaged or untracked changes, an unfinished merge or rebase, or stash entries: the stash
    /// lives only in that worktree. Commits and branches are never touched; objects only it reached
    /// become garbage for `gc`.
    pub fn worktree_remove(&self, path: &Path, force: bool) -> Result<()> {
        let _l = self.lock()?;
        let abs = if path.is_absolute() {
            path.to_owned()
        } else {
            io(path, std::env::current_dir())?.join(path)
        };
        let abs = abs.canonicalize().unwrap_or(abs);
        if abs == self.main_worktree() {
            return msg("the main worktree cannot be removed");
        }
        if abs == self.worktree {
            return msg("cannot remove the worktree you are in; run this from another worktree");
        }
        let Some((name, _)) = self.linked_worktrees()?.into_iter().find(|(_, p)| *p == abs) else {
            return msg(format!("{} is not a registered worktree (`vcs worktree list`)", abs.display()));
        };
        let vcs = abs.join(META_DIR);
        if vcs.join(FORMAT_FILE).is_file() {
            // Never delete a directory that is not provably this worktree.
            let ours = fs::read_to_string(vcs.join(NAME_FILE))
                .map(|s| s.trim_end_matches('\n') == name)
                .unwrap_or(false)
                && fs::read_to_string(vcs.join(COMMONDIR))
                    .map(|s| Path::new(s.trim_end_matches('\n')) == self.common)
                    .unwrap_or(false);
            if !ours {
                return msg(format!("{} does not belong to this repository; not deleting it", abs.display()));
            }
            if !force {
                let v = self.open_view(&abs)?;
                let st = v.status()?;
                let mut why = Vec::new();
                for z in &st.zones {
                    if !z.staged.is_empty() || !z.unstaged.is_empty() {
                        why.push(format!(
                            "zone {} has {} staged and {} unstaged change(s)",
                            z.zone,
                            z.staged.len(),
                            z.unstaged.len()
                        ));
                    }
                }
                if !st.untracked.is_empty() {
                    why.push(format!("{} untracked file(s)", st.untracked.len()));
                }
                if st.merging.is_some() || v.rebase_state_raw()?.is_some() || v.pick_marker()?.is_some() {
                    why.push("an unfinished merge, rebase or cherry-pick".into());
                }
                if !v.stash_list()?.is_empty() {
                    why.push("stash entries (they live only in this worktree)".into());
                }
                if !why.is_empty() {
                    return msg(format!(
                        "worktree {} is not clean: {}; commit or discard them, or pass --force",
                        abs.display(),
                        why.join("; ")
                    ));
                }
            }
            io(&abs, fs::remove_dir_all(&abs))?;
        }
        // Registry last: a crash before this leaves a worktree gc can still see.
        let reg = self.common.join(REGISTRY).join(&name);
        io(&reg, fs::remove_dir_all(&reg))
    }

    /// fsck: registry entries must point at readable worktrees, and a branch must be checked out
    /// in at most one of them. Also returns every worktree's index entries for the blob check.
    pub(crate) fn fsck_worktrees(&self, problems: &mut Vec<String>) -> Vec<(String, Index)> {
        let mut seen: std::collections::BTreeMap<(String, String), PathBuf> = Default::default();
        let mut indexes = Vec::new();
        let views = match self.worktree_views() {
            Ok(v) => v,
            Err(e) => {
                problems.push(format!("worktrees: {e}"));
                return vec![(self.worktree.display().to_string(), self.index().unwrap_or_default())];
            }
        };
        for v in views {
            match v.workspace() {
                Ok(ws) => {
                    for (z, b) in ws.subs {
                        if let Some(other) = seen.insert((z.clone(), b.clone()), v.worktree.clone()) {
                            problems.push(format!(
                                "branch {z}/{b} is checked out in both {} and {}",
                                other.display(),
                                v.worktree.display()
                            ));
                        }
                    }
                }
                Err(e) => problems.push(format!("worktree {}: workspace: {e}", v.worktree.display())),
            }
            match v.index() {
                Ok(ix) => indexes.push((v.worktree.display().to_string(), ix)),
                Err(e) => problems.push(format!("worktree {}: index: {e}", v.worktree.display())),
            }
        }
        indexes
    }
}
