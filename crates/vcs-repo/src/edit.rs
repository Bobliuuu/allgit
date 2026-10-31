//! `restore`, `rm` and `mv`: the everyday edit commands.
//!
//! All three work on the index and the working tree together and validate everything before
//! touching a file, so a refused command changes nothing.
//!
//! * `restore` rewrites working-tree files from the index (the default), or index entries from
//!   HEAD (`staged`, working tree untouched). It only ever writes tracked paths, so untracked
//!   files are never overwritten; a directory or symlink in the way is refused.
//! * `rm` deletes tracked files (and their index entries). Without `force` it refuses any file
//!   with staged or unstaged changes; with `cached` it only un-tracks and keeps the file.
//! * `mv` renames a tracked file or directory inside one zone. Moving across a zone boundary
//!   is refused (it would change which zone owns the file).
//!
//! Each successful command records one operation (the branch view does not change, so
//! `op undo` has no refs to move; the log entry documents what happened).

use crate::error::{io, msg, Error, Result};
use crate::index::{Index, IndexEntry};
use crate::meta::{Workspace, Zones};
use crate::repo::{mtime_ns, Repo, Wt};
use crate::treeops::Flat;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use vcs_objects::names::validate_path;
use vcs_objects::EntryMode;
use vcs_store::ObjectStore;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RestoreReport {
    /// Files rewritten (worktree restore) or index entries changed (staged restore).
    pub restored: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RmReport {
    pub removed: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MvReport {
    /// `(from, to)` for every tracked file moved.
    pub moved: Vec<(String, String)>,
}

fn under(path: &str, prefix: &str) -> bool {
    prefix.is_empty() || path == prefix || (path.starts_with(prefix) && path.as_bytes().get(prefix.len()) == Some(&b'/'))
}

impl Repo {
    fn require_subscribed(&self, zones: &Zones, ws: &Workspace, p: &str) -> Result<()> {
        if p.is_empty() {
            return Ok(());
        }
        let owner = zones.owner(p);
        if !ws.subs.contains_key(owner) {
            return msg(format!(
                "{p:?} belongs to zone {owner:?}, which this workspace has not subscribed to (vcs zone add {owner})"
            ));
        }
        Ok(())
    }

    /// Refuse to write `p` when an ancestor in the working tree is a symlink or a file.
    fn check_ancestors(&self, p: &str) -> Result<()> {
        let mut acc = self.worktree.clone();
        let comps: Vec<&str> = p.split('/').collect();
        for c in &comps[..comps.len() - 1] {
            acc.push(c);
            match fs::symlink_metadata(&acc) {
                Ok(m) if m.file_type().is_symlink() => return msg(format!("refusing to write through symlink at {}", acc.display())),
                Ok(m) if m.is_file() => return msg(format!("cannot create {p:?}: {:?} is a file", c)),
                _ => {}
            }
        }
        Ok(())
    }

    fn prune_dirs(&self, dirs: BTreeSet<PathBuf>) {
        for mut d in dirs {
            while d != self.worktree && d.starts_with(&self.worktree) && fs::remove_dir(&d).is_ok() {
                if !d.pop() {
                    break;
                }
            }
        }
    }

    /// Tracked, subscribed index paths matched by each pathspec; every pathspec must match.
    fn match_index(&self, specs: &[String], zones: &Zones, ws: &Workspace, ix: &Index, what: &str) -> Result<BTreeSet<String>> {
        let mut out = BTreeSet::new();
        for p in specs {
            self.require_subscribed(zones, ws, p)?;
            let hits: Vec<String> = ix.under(p).into_iter().filter(|q| ws.subs.contains_key(zones.owner(q))).collect();
            if hits.is_empty() && !p.is_empty() {
                return msg(format!("pathspec {p:?} did not match any {what} files"));
            }
            out.extend(hits);
        }
        Ok(out)
    }

    fn unresolved_conflicts(&self) -> Result<BTreeSet<String>> {
        Ok(self
            .merge_state()?
            .map(|m| m.unresolved().iter().map(|c| c.path.clone()).collect())
            .unwrap_or_default())
    }

    /// Restore working-tree files from the index, or (`staged`) index entries from HEAD.
    pub fn restore(&self, specs: &[String], staged: bool) -> Result<RestoreReport> {
        let _l = self.lock()?;
        if specs.is_empty() {
            return msg("nothing specified; use `vcs restore .` to restore everything");
        }
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut ix = self.index()?;
        let n = if staged {
            self.restore_staged(specs, &zones, &ws, &mut ix)?
        } else {
            self.restore_worktree(specs, &zones, &ws, &mut ix)?
        };
        if n > 0 {
            self.save_index(&mut ix)?;
        }
        let what = if staged { "restore --staged" } else { "restore" };
        self.record_op(&format!("{what} {}", specs.join(" ")), None)?;
        Ok(RestoreReport { restored: n })
    }

    fn restore_worktree(&self, specs: &[String], zones: &Zones, ws: &Workspace, ix: &mut Index) -> Result<usize> {
        let paths = self.match_index(specs, zones, ws, ix, "tracked")?;
        // 1. decide and validate everything first
        let mut todo = Vec::new();
        for p in &paths {
            let e = &ix.entries[p];
            if matches!(self.wt_state(p, e, ix.written_ns)?, Wt::Clean) {
                continue;
            }
            self.check_ancestors(p)?;
            match fs::symlink_metadata(self.worktree.join(p)) {
                Ok(m) if m.is_file() => {}
                Ok(_) => return msg(format!("cannot restore {p:?}: a directory or special file is in the way")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(Error::Io {
                        path: self.worktree.join(p),
                        source,
                    })
                }
            }
            todo.push(p.clone());
        }
        // 2. write
        for p in &todo {
            let (mode, id) = (ix.entries[p].mode, ix.entries[p].id);
            let data = self.store.get_blob(&id)?;
            let abs = self.worktree.join(p);
            let parent = abs.parent().unwrap();
            io(parent, fs::create_dir_all(parent))?;
            let tmp = parent.join(format!(".vcs-tmp-{}", std::process::id()));
            io(&tmp, fs::write(&tmp, &data))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let bits = if mode == EntryMode::Executable { 0o755 } else { 0o644 };
                io(&tmp, fs::set_permissions(&tmp, fs::Permissions::from_mode(bits)))?;
            }
            io(&abs, fs::rename(&tmp, &abs))?;
            let m = io(&abs, fs::metadata(&abs))?;
            ix.entries.insert(
                p.clone(),
                IndexEntry {
                    id,
                    mode,
                    size: m.len(),
                    mtime_ns: mtime_ns(&m),
                },
            );
        }
        Ok(todo.len())
    }

    fn restore_staged(&self, specs: &[String], zones: &Zones, ws: &Workspace, ix: &mut Index) -> Result<usize> {
        let refs = self.refs()?;
        let mut heads: BTreeMap<String, Flat> = BTreeMap::new();
        for zone in ws.subs.keys() {
            heads.insert(zone.clone(), self.head_flat(zone, zones, ws, &refs)?);
        }
        let conflicted = self.unresolved_conflicts()?;
        let mut cands = BTreeSet::new();
        for p in specs {
            self.require_subscribed(zones, ws, p)?;
            let mut hit = false;
            for q in ix.under(p) {
                if ws.subs.contains_key(zones.owner(&q)) {
                    cands.insert(q);
                    hit = true;
                }
            }
            for flat in heads.values() {
                for q in flat.keys().filter(|q| under(q, p)) {
                    cands.insert(q.clone());
                    hit = true;
                }
            }
            if !hit && !p.is_empty() {
                return msg(format!("pathspec {p:?} did not match any tracked files"));
            }
        }
        if let Some(c) = cands.iter().find(|c| conflicted.contains(*c)) {
            return msg(format!("{c:?} has unresolved merge conflicts; resolve it or abort the merge first"));
        }
        // drop entries that HEAD does not have, then set the ones it does (so file/dir swaps work)
        let mut changed = 0;
        let mut set = Vec::new();
        for q in &cands {
            let head = heads.get(zones.owner(q)).and_then(|f| f.get(q)).copied();
            match (head, ix.entries.get(q)) {
                (None, None) => {}
                (None, Some(_)) => {
                    ix.entries.remove(q);
                    changed += 1;
                }
                (Some((m, id)), Some(e)) if e.id == id && e.mode == m => {}
                (Some((m, id)), _) => {
                    ix.entries.remove(q);
                    set.push((q.clone(), m, id));
                    changed += 1;
                }
            }
        }
        for (q, mode, id) in set {
            let size = self.store.get_blob(&id)?.len() as u64;
            // mtime 0 never matches the file's, so status re-hashes it instead of trusting a stale stat
            ix.insert(
                &q,
                IndexEntry {
                    id,
                    mode,
                    size,
                    mtime_ns: 0,
                },
            )?;
        }
        Ok(changed)
    }

    /// Remove tracked files from the index and the working tree (`cached`: index only).
    pub fn rm(&self, specs: &[String], cached: bool, force: bool) -> Result<RmReport> {
        let _l = self.lock()?;
        if specs.is_empty() {
            return msg("nothing specified");
        }
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let mut ix = self.index()?;
        let paths = self.match_index(specs, &zones, &ws, &ix, "tracked")?;
        if paths.is_empty() {
            return msg("nothing to remove");
        }
        if !force {
            let conflicted = self.unresolved_conflicts()?;
            let mut heads: BTreeMap<String, Flat> = BTreeMap::new();
            let mut bad = Vec::new();
            for p in &paths {
                let e = &ix.entries[p];
                let zone = zones.owner(p).to_owned();
                if !heads.contains_key(&zone) {
                    heads.insert(zone.clone(), self.head_flat(&zone, &zones, &ws, &refs)?);
                }
                let in_head = heads[&zone].get(p) == Some(&(e.mode, e.id));
                let wt = self.wt_state(p, e, ix.written_ns)?;
                if conflicted.contains(p) {
                    bad.push(format!("  {p} (unresolved merge conflict)"));
                } else if cached {
                    // un-tracking keeps the file; only refuse if it would drop a staged version nobody has
                    if !in_head && matches!(wt, Wt::Modified | Wt::Deleted) {
                        bad.push(format!("  {p} (staged content differs from both HEAD and the file)"));
                    }
                } else if !in_head {
                    bad.push(format!("  {p} (has staged changes)"));
                } else if !matches!(wt, Wt::Clean) {
                    bad.push(format!("  {p} (has local modifications)"));
                }
            }
            if !bad.is_empty() {
                return msg(format!(
                    "refusing to remove files with uncommitted work:\n{}\n\nOptions:\n  vcs commit / vcs restore first\n  vcs rm --cached <path>   (keep the file)\n  vcs rm --force <path>    (discard)",
                    bad.join("\n")
                ));
            }
        }
        if !cached {
            for p in &paths {
                match fs::symlink_metadata(self.worktree.join(p)) {
                    Ok(m) if m.is_file() => {}
                    Ok(_) => return msg(format!("cannot remove {p:?}: not a regular file in the working tree")),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(Error::Io {
                            path: self.worktree.join(p),
                            source,
                        })
                    }
                }
            }
        }
        let mut dirs = BTreeSet::new();
        for p in &paths {
            if !cached {
                let abs = self.worktree.join(p);
                match fs::remove_file(&abs) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => return Err(Error::Io { path: abs, source }),
                }
                if let Some(par) = abs.parent() {
                    dirs.insert(par.to_owned());
                }
            }
            ix.entries.remove(p);
        }
        self.prune_dirs(dirs);
        self.save_index(&mut ix)?;
        self.record_op(&format!("rm {}", specs.join(" ")), None)?;
        Ok(RmReport {
            removed: paths.into_iter().collect(),
        })
    }

    /// Rename a tracked file or directory (into `dst` when `dst` is an existing directory).
    pub fn mv(&self, src: &str, dst: &str, force: bool) -> Result<MvReport> {
        let _l = self.lock()?;
        if src.is_empty() || dst.is_empty() {
            return msg("cannot move the repository root");
        }
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut ix = self.index()?;
        self.require_subscribed(&zones, &ws, src)?;
        self.require_subscribed(&zones, &ws, dst)?;
        let old: Vec<String> = ix.under(src);
        if old.is_empty() {
            return msg(format!("{src:?} is not tracked"));
        }
        let into_dir = match fs::symlink_metadata(self.worktree.join(dst)) {
            Ok(m) => m.is_dir() && !ix.entries.contains_key(dst),
            Err(_) => false,
        };
        let base = if into_dir {
            let name = src.rsplit('/').next().unwrap();
            format!("{dst}/{name}")
        } else {
            dst.to_owned()
        };
        self.require_subscribed(&zones, &ws, &base)?;
        if base == src {
            return msg(format!("{src:?} and {base:?} are the same"));
        }
        if under(&base, src) {
            return msg(format!("cannot move {src:?} into itself"));
        }
        let conflicted = self.unresolved_conflicts()?;
        let mut plan = Vec::new();
        for o in &old {
            let n = if o == src {
                base.clone()
            } else {
                format!("{base}{}", &o[src.len()..])
            };
            validate_path(&n)?;
            if zones.owner(o) != zones.owner(&n) {
                return msg(format!(
                    "cannot move {o:?} to {n:?}: they belong to different zones ({:?} and {:?})",
                    zones.owner(o),
                    zones.owner(&n)
                ));
            }
            if conflicted.contains(o) {
                return msg(format!("{o:?} has unresolved merge conflicts; resolve it first"));
            }
            match fs::symlink_metadata(self.worktree.join(o)) {
                Ok(m) if m.is_file() => {}
                Ok(_) => return msg(format!("{o:?} is not a regular file in the working tree")),
                Err(_) => return msg(format!("{o:?} is missing from the working tree; `vcs restore {o}` it first")),
            }
            self.check_ancestors(&n)?;
            let occupied = ix.entries.contains_key(&n) && !old.contains(&n) || fs::symlink_metadata(self.worktree.join(&n)).is_ok();
            if occupied && !force {
                return msg(format!("destination {n:?} already exists (use --force to overwrite)"));
            }
            if occupied {
                match fs::symlink_metadata(self.worktree.join(&n)) {
                    Ok(m) if !m.is_file() => return msg(format!("cannot overwrite {n:?}: not a regular file")),
                    _ => {}
                }
            }
            plan.push((o.clone(), n));
        }
        // execute, rolling back on failure
        let mut done: Vec<(String, String)> = Vec::new();
        for (o, n) in &plan {
            let (from, to) = (self.worktree.join(o), self.worktree.join(n));
            let step = to.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::rename(&from, &to));
            if let Err(source) = step {
                for (o2, n2) in done.iter().rev() {
                    let _ = fs::rename(self.worktree.join(n2), self.worktree.join(o2));
                }
                return Err(Error::Io { path: to, source });
            }
            done.push((o.clone(), n.clone()));
        }
        let mut dirs = BTreeSet::new();
        let mut moved_entries = Vec::new();
        for (o, n) in &plan {
            if let Some(par) = self.worktree.join(o).parent() {
                dirs.insert(par.to_owned());
            }
            moved_entries.push((n.clone(), ix.entries.remove(o).expect("tracked")));
            ix.entries.remove(n);
        }
        for (n, e) in moved_entries {
            ix.insert(&n, e)?;
        }
        self.prune_dirs(dirs);
        self.save_index(&mut ix)?;
        self.record_op(&format!("mv {src} {dst}"), None)?;
        Ok(MvReport { moved: plan })
    }
}
