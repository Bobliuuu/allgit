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
use crate::repo::{Repo, Wt};
use crate::treeops;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use vcs_objects::names::validate_path;
use vcs_objects::{EntryMode, ObjectId};

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

    /// [`Self::check_ancestors`] that remembers directories already found fine, so a big batch
    /// examines each directory once instead of once per file beneath it.
    fn check_ancestors_memo(&self, p: &str, ok: &mut std::collections::HashSet<String>) -> Result<()> {
        let Some((dir, _)) = p.rsplit_once('/') else { return Ok(()) };
        if ok.contains(dir) {
            return Ok(());
        }
        self.check_ancestors(p)?;
        let mut d = dir;
        loop {
            if !ok.insert(d.to_owned()) {
                break;
            }
            match d.rsplit_once('/') {
                Some((up, _)) => d = up,
                None => break,
            }
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
                Ok(m) if crate::portable::is_link_like(&m) => {
                    return msg(format!("refusing to write through symlink at {}", acc.display()))
                }
                Ok(m) if m.is_file() => return msg(format!("cannot create {p:?}: {:?} is a file", c)),
                _ => {}
            }
        }
        Ok(())
    }

    /// Remove now-empty directories, never the worktree root and never the process's current directory
    /// (removing it would leave the caller's next command failing with "No such file or directory").
    pub(crate) fn prune_dirs(&self, dirs: impl IntoIterator<Item = PathBuf>) {
        let cwd = std::env::current_dir().ok().and_then(|c| c.canonicalize().ok());
        let keep = |d: &PathBuf| cwd.as_ref().is_some_and(|c| d.canonicalize().is_ok_and(|d| *c == d));
        for mut d in dirs {
            while d != self.worktree && d.starts_with(&self.worktree) && !keep(&d) && fs::remove_dir(&d).is_ok() {
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
            let all: Vec<String> = ix.under(p).into_iter().filter(|q| ws.subs.contains_key(zones.owner(q))).collect();
            let hits: Vec<String> = all.iter().filter(|q| !ws.skips(zones.owner(q), q)).cloned().collect();
            if hits.is_empty() && !all.is_empty() {
                return msg(format!(
                    "pathspec {p:?} is outside this workspace's sparse set (not materialized); `vcs sparse add` it first"
                ));
            }
            if hits.is_empty() && !p.is_empty() {
                return msg(format!("pathspec {p:?} did not match any {what} files"));
            }
            out.extend(hits);
        }
        Ok(out)
    }

    /// [`Self::match_index`] without copying every path: the matching entries in path order, borrowed
    /// from the index (`restore .` on 100k files would otherwise clone 100k strings twice).
    fn match_index_list<'a>(
        &self,
        specs: &[String],
        zones: &Zones,
        ws: &Workspace,
        ix: &'a Index,
        what: &str,
    ) -> Result<Vec<(&'a String, &'a IndexEntry)>> {
        let mut out: Vec<(&String, &IndexEntry)> = Vec::new();
        for p in specs {
            self.require_subscribed(zones, ws, p)?;
            let (mut owned, mut hits) = (0usize, 0usize);
            let mut take = |k: &'a String, e: &'a IndexEntry| {
                let zone = zones.owner(k);
                if ws.subs.contains_key(zone) {
                    owned += 1;
                    if !ws.skips(zone, k) {
                        hits += 1;
                        out.push((k, e));
                    }
                }
            };
            if p.is_empty() {
                for (k, e) in ix.entries.iter() {
                    take(k, e);
                }
            } else {
                if let Some((k, e)) = ix.entries.get_key_value(p.as_str()) {
                    take(k, e);
                }
                for (k, e) in ix.entries.range(format!("{p}/")..format!("{p}0")) {
                    take(k, e);
                }
            }
            if hits == 0 && owned > 0 {
                return msg(format!(
                    "pathspec {p:?} is outside this workspace's sparse set (not materialized); `vcs sparse add` it first"
                ));
            }
            if hits == 0 && !p.is_empty() {
                return msg(format!("pathspec {p:?} did not match any {what} files"));
            }
        }
        if specs.len() > 1 {
            out.sort_unstable_by(|a, b| a.0.cmp(b.0));
            out.dedup_by(|a, b| std::ptr::eq(a.0, b.0));
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
        let what = if staged { "restore --staged" } else { "restore" };
        if staged {
            // The operation is recorded before the (unsynced) index write, see `prepare_index_patch`.
            let (n, patched) = self.restore_staged(specs, &zones, &ws)?;
            self.record_op(&format!("{what} {}", specs.join(" ")), None)?;
            if let Some(bytes) = patched {
                self.write_index_bytes(&bytes)?;
            }
            return Ok(RestoreReport { restored: n });
        }
        let mut ix = self.index()?;
        let n = self.restore_worktree(specs, &zones, &ws, &mut ix)?;
        if n > 0 {
            self.save_index(&mut ix)?;
        }
        self.record_op(&format!("{what} {}", specs.join(" ")), None)?;
        Ok(RestoreReport { restored: n })
    }

    fn restore_worktree(&self, specs: &[String], zones: &Zones, ws: &Workspace, ix: &mut Index) -> Result<usize> {
        use crate::repo::ChangeKind;
        // 1. decide and validate everything first. The stat of every matched path (and the
        // content check of a racy one) runs on the status worker pool.
        let list = self.match_index_list(specs, zones, ws, ix, "tracked")?;
        let (changed, _clean) = self.unstaged_changes(&list, ix.written_ns)?;
        drop(list);
        let mut ok_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut todo: Vec<(String, bool)> = Vec::with_capacity(changed.len()); // (path, known absent)
        for c in &changed {
            let p = &c.path;
            self.check_ancestors_memo(p, &mut ok_dirs)?;
            let absent = c.kind == ChangeKind::Deleted;
            if !absent {
                match fs::symlink_metadata(self.worktree.join(p)) {
                    Ok(m) if m.is_file() || m.file_type().is_symlink() => {}
                    Ok(_) => return msg(format!("cannot restore {p:?}: a directory or special file is in the way")),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(Error::Io {
                            path: self.worktree.join(p),
                            source,
                        })
                    }
                }
            }
            todo.push((p.clone(), absent));
        }
        // 2. write: independent per path, so a big batch runs on a bounded pool (8 threads)
        let write_one = |(p, absent): &(String, bool)| -> Result<IndexEntry> {
            let (mode, id) = (ix.entries[p].mode, ix.entries[p].id);
            let data = match self.content_or_stream(p, &id, mode)? {
                crate::repo::FileContent::Streamed(e) => return Ok(e),
                crate::repo::FileContent::Bytes(b) => b,
            };
            let abs = || self.worktree.join(p);
            let m = if mode == EntryMode::Symlink {
                crate::portable::check_symlink_target(p, &data).or_else(|why| msg(format!("refusing to restore symlink {p:?}: {why}")))?;
                io(&abs(), self.wfs.symlink(&data, p))?
            } else if *absent {
                // known absent a moment ago: create in place; if something appeared, replace atomically
                match self.wfs.create_new(p, &data, mode) {
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => io(&abs(), self.wfs.write(p, &data, mode))?,
                    r => io(&abs(), r)?,
                }
            } else {
                // Replacing local edits: unlink and create in place, like git. A rename over the
                // old file makes ext4 flush the new data before returning, which dominates when
                // many files are restored. (A crash in between leaves the file missing; it shows as
                // deleted and `restore` brings it back.)
                match self.wfs.remove(p) {
                    Ok(()) => match self.wfs.create_new(p, &data, mode) {
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => io(&abs(), self.wfs.write(p, &data, mode))?,
                        r => io(&abs(), r)?,
                    },
                    Err(_) => io(&abs(), self.wfs.write(p, &data, mode))?,
                }
            };
            Ok(IndexEntry {
                id,
                mode,
                size: m.len,
                mtime_ns: m.mtime_ns,
                ctime_ns: m.ctime_ns,
            })
        };
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
        let written: Vec<Result<IndexEntry>> = if threads <= 1 || todo.len() < 64 {
            todo.iter().map(write_one).collect()
        } else {
            let per = todo.len().div_ceil(threads);
            std::thread::scope(|sc| {
                let write_one = &write_one;
                let hs: Vec<_> = todo
                    .chunks(per)
                    .map(|part| sc.spawn(move || part.iter().map(write_one).collect::<Vec<_>>()))
                    .collect();
                hs.into_iter().flat_map(|h| h.join().expect("restore writer panicked")).collect()
            })
        };
        let n = todo.len();
        for ((p, _), e) in todo.into_iter().zip(written) {
            ix.entries.insert(p, e?);
        }
        Ok(n)
    }

    /// `restore --staged`: put the index entries under `specs` back to what HEAD has. Works on the
    /// lazily read index file and looks HEAD up per path, so one file costs a block, not the tree.
    fn restore_staged(&self, specs: &[String], zones: &Zones, ws: &Workspace) -> Result<(usize, Option<Vec<u8>>)> {
        let refs = self.refs()?;
        let f = self.index_file()?;
        let mut roots: BTreeMap<String, Option<ObjectId>> = BTreeMap::new();
        for zone in ws.subs.keys() {
            roots.insert(zone.clone(), self.head_root(zone, ws, &refs)?);
        }
        let conflicted = self.unresolved_conflicts()?;
        let mut cands = BTreeSet::new();
        for p in specs {
            self.require_subscribed(zones, ws, p)?;
            let mut hit = false;
            for (q, _) in f.under(p)? {
                if ws.subs.contains_key(zones.owner(&q)) {
                    cands.insert(q);
                    hit = true;
                }
            }
            for (zone, root) in &roots {
                let Some(root) = *root else { continue };
                let zp = zones.path_of(zone).unwrap_or("");
                // the part of this zone's tree that lies under `p`
                let rel: Option<&str> = if p.is_empty() || zp == p.as_str() {
                    Some("")
                } else if zp.is_empty() {
                    Some(p.as_str())
                } else if let Some(r) = p.strip_prefix(zp).and_then(|r| r.strip_prefix('/')) {
                    Some(r)
                } else if zp.strip_prefix(p.as_str()).is_some_and(|r| r.starts_with('/')) {
                    Some("") // the zone lies inside `p`
                } else {
                    None
                };
                let Some(rel) = rel else { continue };
                let Some(ent) = treeops::entry_at(&self.store, root, rel)? else {
                    continue;
                };
                let base = if rel.is_empty() {
                    zp.to_owned()
                } else if zp.is_empty() {
                    rel.to_owned()
                } else {
                    format!("{zp}/{rel}")
                };
                if ent.0 == EntryMode::Directory {
                    for q in treeops::flatten(&self.store, Some(ent.1), &base)?.into_keys() {
                        if under(&q, p) {
                            cands.insert(q);
                            hit = true;
                        }
                    }
                } else if under(&base, p) {
                    cands.insert(base);
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
        // entries HEAD does not have go, the ones it does are set (a file/dir swap works because
        // the patch is judged after all of its removals)
        let mut edits = crate::index_file::Edits::new();
        for q in &cands {
            let zone = zones.owner(q);
            let head = match roots.get(zone) {
                Some(root) => self.head_entry(*root, zones, zone, q)?,
                None => None,
            };
            match (head, f.get(q)?) {
                (None, None) => {}
                (None, Some(_)) => {
                    edits.insert(q.clone(), None);
                }
                (Some((m, id)), Some(e)) if e.id == id && e.mode == m => {}
                (Some((m, id)), _) => {
                    let size = self.content_len(&id)?;
                    // mtime 0 never matches the file's, so status re-hashes it instead of trusting a stale stat
                    edits.insert(
                        q.clone(),
                        Some(IndexEntry {
                            id,
                            mode: m,
                            size,
                            mtime_ns: 0,
                            ctime_ns: 0,
                        }),
                    );
                }
            }
        }
        let n = edits.len();
        let (patched, _) = self.prepare_index_patch(&f, &edits)?;
        Ok((n, patched))
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
        // The index is read lazily: a few lookups and one patched block, not a decode of every entry.
        let f = self.index_file()?;
        let paths = self.match_index_f(specs, &zones, &ws, &f, "tracked")?;
        if paths.is_empty() {
            return msg("nothing to remove");
        }
        if !force {
            let conflicted = self.unresolved_conflicts()?;
            let mut roots: BTreeMap<String, Option<ObjectId>> = BTreeMap::new();
            let mut bad = Vec::new();
            for (p, e) in &paths {
                let zone = zones.owner(p).to_owned();
                if !roots.contains_key(&zone) {
                    roots.insert(zone.clone(), self.head_root(&zone, &ws, &refs)?);
                }
                let in_head = self.head_entry(roots[&zone], &zones, &zone, p)? == Some((e.mode, e.id));
                let wt = self.wt_state(p, e, f.written_ns)?;
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
            for p in paths.keys() {
                match fs::symlink_metadata(self.worktree.join(p)) {
                    Ok(m) if m.is_file() || m.file_type().is_symlink() => {}
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
        let mut edits = crate::index_file::Edits::new();
        for p in paths.keys() {
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
            edits.insert(p.clone(), None);
        }
        self.prune_dirs(dirs);
        let (patched, _) = self.prepare_index_patch(&f, &edits)?;
        self.record_op(&format!("rm {}", specs.join(" ")), None)?;
        if let Some(bytes) = patched {
            self.write_index_bytes(&bytes)?;
        }
        Ok(RmReport {
            removed: paths.into_keys().collect(),
        })
    }

    /// `match_index` over a lazily read index file; keeps the entries it found.
    fn match_index_f(
        &self,
        specs: &[String],
        zones: &Zones,
        ws: &Workspace,
        f: &crate::index_file::IndexFile,
        what: &str,
    ) -> Result<BTreeMap<String, IndexEntry>> {
        let mut out = BTreeMap::new();
        for p in specs {
            self.require_subscribed(zones, ws, p)?;
            let all: Vec<(String, IndexEntry)> = f
                .under(p)?
                .into_iter()
                .filter(|(q, _)| ws.subs.contains_key(zones.owner(q)))
                .collect();
            let hits: Vec<(String, IndexEntry)> = all.iter().filter(|(q, _)| !ws.skips(zones.owner(q), q)).cloned().collect();
            if hits.is_empty() && !all.is_empty() {
                return msg(format!(
                    "pathspec {p:?} is outside this workspace's sparse set (not materialized); `vcs sparse add` it first"
                ));
            }
            if hits.is_empty() && !p.is_empty() {
                return msg(format!("pathspec {p:?} did not match any {what} files"));
            }
            out.extend(hits);
        }
        Ok(out)
    }

    /// Rename a tracked file or directory (into `dst` when `dst` is an existing directory).
    pub fn mv(&self, src: &str, dst: &str, force: bool) -> Result<MvReport> {
        let _l = self.lock()?;
        if src.is_empty() || dst.is_empty() {
            return msg("cannot move the repository root");
        }
        let zones = self.zones()?;
        let ws = self.workspace()?;
        // Lazy index: a few lookups and one patched block, not a decode and rewrite of every entry.
        let f = self.index_file()?;
        self.require_subscribed(&zones, &ws, src)?;
        self.require_subscribed(&zones, &ws, dst)?;
        let old_entries = f.under(src)?;
        let old: Vec<String> = old_entries.iter().map(|(k, _)| k.clone()).collect();
        if old.is_empty() {
            return msg(format!("{src:?} is not tracked"));
        }
        if let Some(o) = old.iter().find(|o| ws.skips(zones.owner(o), o)) {
            return msg(format!(
                "{o:?} is outside this workspace's sparse set (not materialized); `vcs sparse add` it first"
            ));
        }
        let into_dir = match fs::symlink_metadata(self.worktree.join(dst)) {
            Ok(m) => m.is_dir() && f.get(dst)?.is_none(),
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
            if ws.skips(zones.owner(&n), &n) {
                return msg(format!(
                    "destination {n:?} is outside this workspace's sparse set; `vcs sparse add` it first"
                ));
            }
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
                Ok(m) if m.is_file() || m.file_type().is_symlink() => {}
                Ok(_) => return msg(format!("{o:?} is not a regular file in the working tree")),
                Err(_) => return msg(format!("{o:?} is missing from the working tree; `vcs restore {o}` it first")),
            }
            self.check_ancestors(&n)?;
            let occupied = f.get(&n)?.is_some() && !old.contains(&n) || fs::symlink_metadata(self.worktree.join(&n)).is_ok();
            if occupied && !force {
                return msg(format!("destination {n:?} already exists (use --force to overwrite)"));
            }
            if occupied {
                match fs::symlink_metadata(self.worktree.join(&n)) {
                    Ok(m) if !(m.is_file() || m.file_type().is_symlink()) => {
                        return msg(format!("cannot overwrite {n:?}: not a regular file"))
                    }
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
        let mut edits = crate::index_file::Edits::new();
        let mut sets = Vec::with_capacity(plan.len());
        for ((o, n), (_, e)) in plan.iter().zip(old_entries) {
            if let Some(par) = self.worktree.join(o).parent() {
                dirs.insert(par.to_owned());
            }
            edits.insert(o.clone(), None);
            sets.push((n.clone(), e));
        }
        // sets last, so a destination that is also a source ends up set, not removed
        for (n, e) in sets {
            edits.insert(n, Some(e));
        }
        self.prune_dirs(dirs);
        let (patched, _) = self.prepare_index_patch(&f, &edits)?;
        self.record_op(&format!("mv {src} {dst}"), None)?;
        if let Some(bytes) = patched {
            self.write_index_bytes(&bytes)?;
        }
        Ok(MvReport { moved: plan })
    }
}
