//! Path-level sparse sets inside a subscribed zone.
//!
//! A workspace may materialize only some path prefixes of a zone. Design (see
//! docs/sparse-paths.md): the index keeps an entry for *every* tracked path of the zone, taken
//! from the branch head's tree; entries outside the sparse set carry an empty stat and are never
//! compared with the disk. So status never reports them as deleted, `commit` builds the tree from
//! the index and therefore carries them over from the parent unchanged, and checkout/merge/
//! rebase/stash only move them in the index.

use crate::error::{msg, Result};
use crate::meta::{in_sparse_set, Workspace};
use crate::repo::Repo;
use crate::treeops::Flat;
use crate::worktree_fs::FileKind;
use std::collections::BTreeSet;
use vcs_objects::names::validate_path;
use vcs_remote::Backend;

/// What `sparse set|add|remove|disable` changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SparseReport {
    /// Files written to the working tree.
    pub materialized: usize,
    /// Files removed from the working tree (they stay tracked).
    pub dematerialized: usize,
}

impl Repo {
    /// Sparse sets by zone (zones without an entry are fully materialized).
    pub fn sparse_list(&self) -> Result<std::collections::BTreeMap<String, Vec<String>>> {
        Ok(self
            .workspace()?
            .sparse
            .into_iter()
            .map(|(z, s)| (z, s.into_iter().collect()))
            .collect())
    }

    /// Replace the zone's sparse set with `paths` (repo-relative directories or files).
    pub fn sparse_set(&self, backend: Option<&dyn Backend>, zone: &str, paths: &[String]) -> Result<SparseReport> {
        if paths.is_empty() {
            return msg("`sparse set` needs at least one path (use `sparse disable` to materialize the whole zone)");
        }
        self.sparse_apply(backend, zone, |_| Ok(paths.iter().cloned().collect()))
    }

    /// Add `paths` to the zone's sparse set (a zone that was not sparse becomes sparse with them).
    pub fn sparse_add(&self, backend: Option<&dyn Backend>, zone: &str, paths: &[String]) -> Result<SparseReport> {
        if paths.is_empty() {
            return msg("nothing to add");
        }
        let was_full = |ws: &Workspace, z: &str| !ws.sparse.contains_key(z);
        self.sparse_apply(backend, zone, |ws| {
            if was_full(ws, zone) {
                return msg("the zone is fully materialized; use `sparse set` to restrict it");
            }
            let mut s = ws.sparse[zone].clone();
            s.extend(paths.iter().cloned());
            Ok(s)
        })
    }

    /// Remove `paths` from the zone's sparse set (they must be listed exactly as stored).
    pub fn sparse_remove(&self, zone: &str, paths: &[String]) -> Result<SparseReport> {
        self.sparse_apply(None, zone, |ws| {
            let Some(cur) = ws.sparse.get(zone) else {
                return msg("the zone is fully materialized; nothing to remove");
            };
            let mut s = cur.clone();
            for p in paths {
                if !s.remove(p) {
                    return msg(format!("{p:?} is not in the sparse set (see `vcs sparse list`)"));
                }
            }
            if s.is_empty() {
                return msg("removing the last path would leave nothing materialized; use `sparse disable` instead");
            }
            Ok(s)
        })
    }

    /// Materialize the whole zone again.
    pub fn sparse_disable(&self, backend: Option<&dyn Backend>, zone: &str) -> Result<SparseReport> {
        self.sparse_apply_opt(backend, zone, |_| Ok(None))
    }

    fn sparse_apply(
        &self,
        backend: Option<&dyn Backend>,
        zone: &str,
        f: impl FnOnce(&Workspace) -> Result<BTreeSet<String>>,
    ) -> Result<SparseReport> {
        self.sparse_apply_opt(backend, zone, |ws| f(ws).map(Some))
    }

    /// Move the zone to the sparse set `f` computes (`None` = fully materialized). The zone must
    /// have no uncommitted changes and no merge or rebase in progress. Order: files that leave
    /// the set are removed first, then the workspace file is saved, then files that join are
    /// written; a crash in between leaves missing files visible as deletions (`vcs restore`
    /// writes them), never silent loss.
    fn sparse_apply_opt(
        &self,
        backend: Option<&dyn Backend>,
        zone: &str,
        f: impl FnOnce(&Workspace) -> Result<Option<BTreeSet<String>>>,
    ) -> Result<SparseReport> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        let _l = self.lock()?;
        self.rebase_guard("change the sparse set")?;
        let zones = self.zones()?;
        let ws = self.workspace()?;
        if !ws.subs.contains_key(zone) {
            return msg(format!("zone {zone:?} is not subscribed in this workspace"));
        }
        let new_set = f(&ws)?;
        if let Some(set) = &new_set {
            for p in set {
                validate_path(p)?;
                if zones.owner(&format!("{p}/_")) != zone {
                    return msg(format!("{p:?} is not inside zone {zone:?}"));
                }
            }
        }
        let mut next = ws.clone();
        match &new_set {
            Some(s) => {
                next.sparse.insert(zone.to_owned(), s.clone());
            }
            None => {
                next.sparse.remove(zone);
            }
        }
        let mut ix = self.index()?;
        let st = self.status_inner(crate::repo::StatusOpts::default(), &ix, false)?;
        self.require_clean(zone, &st)?;
        // Paths whose state changes. Out-of-set paths have no disk state to compare, so only
        // joiners need a preflight and only leavers need removal.
        let (mut join, mut leave): (Flat, Vec<String>) = (Flat::new(), Vec::new());
        for (p, e) in ix.entries.iter().filter(|(p, _)| zones.owner(p) == zone) {
            match (ws.skips(zone, p), next.skips(zone, p)) {
                (true, false) => {
                    join.insert(p.clone(), (e.mode, e.id));
                }
                (false, true) => leave.push(p.clone()),
                _ => {}
            }
        }
        self.transition_check_paths_ws(&next, zone, &zones, &Flat::new(), &join)?;
        if let Some(b) = backend {
            let ids: BTreeSet<_> = join.values().map(|(_, id)| *id).collect();
            self.prefetch_ids(b, ids)?;
        }
        let mut rep = SparseReport::default();
        // 1. leavers: remove the (clean) files, keep the index entries with an empty stat
        let mut dirs: BTreeSet<String> = BTreeSet::new();
        for p in &leave {
            let abs = self.worktree.join(p);
            match self.wfs.stat(p) {
                Ok(m) if matches!(m.kind, FileKind::File | FileKind::Symlink) => {
                    crate::error::io(&abs, self.wfs.remove(p))?;
                    rep.dematerialized += 1;
                }
                Ok(_) => return msg(format!("expected a file at {p:?}")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(crate::error::Error::Io { path: abs, source }),
            }
            dirs.insert(p.rsplit_once('/').map_or("", |(d, _)| d).to_owned());
            let e = ix.entries.get_mut(p).expect("tracked");
            e.size = 0;
            e.mtime_ns = 0;
            e.ctime_ns = 0;
        }
        self.prune_dirs_fs(dirs);
        if !leave.is_empty() {
            self.save_index(&mut ix)?;
        }
        // 2. the workspace file
        self.save_ws(&next)?;
        // 3. joiners
        if !join.is_empty() {
            rep.materialized = join.len();
            self.transition_apply_ws(&next, zone, &Flat::new(), &join, &mut ix)?;
            self.save_index(&mut ix)?;
        }
        let what = match &new_set {
            Some(s) => format!("sparse {zone}: {}", s.iter().cloned().collect::<Vec<_>>().join(" ")),
            None => format!("sparse disable {zone}"),
        };
        self.record_op(&what, None)?;
        Ok(rep)
    }

    /// Refuse (before anything is written) a merge-like operation whose conflicts include paths
    /// this workspace does not materialize: markers cannot be written or resolved there. The
    /// remedy is to add the paths to the sparse set and retry.
    pub(crate) fn require_conflicts_materialized<'a>(
        &self,
        zone: &str,
        paths: impl IntoIterator<Item = &'a str>,
        what: &str,
    ) -> Result<()> {
        let ws = self.workspace()?;
        let bad: Vec<&str> = paths.into_iter().filter(|p| ws.skips(zone, p)).collect();
        if bad.is_empty() {
            return Ok(());
        }
        msg(format!(
            "{what} conflicts in paths outside this workspace's sparse set ({}); `vcs sparse add {zone} <path>` them and retry (nothing was changed)",
            bad.join(", ")
        ))
    }

    /// Like [`Self::require_conflicts_materialized`] for any path the operation would change.
    pub(crate) fn require_changes_materialized(&self, zone: &str, old: &Flat, new: &Flat, what: &str) -> Result<()> {
        let ws = self.workspace()?;
        let set = match ws.sparse.get(zone) {
            Some(s) => s,
            None => return Ok(()),
        };
        let bad: Vec<&str> = new
            .iter()
            .filter(|(p, v)| old.get(*p) != Some(v))
            .map(|(p, _)| p.as_str())
            .chain(old.keys().filter(|p| !new.contains_key(*p)).map(String::as_str))
            .filter(|p| !in_sparse_set(set, p))
            .collect();
        if bad.is_empty() {
            return Ok(());
        }
        msg(format!(
            "{what} would change paths outside this workspace's sparse set ({}); `vcs sparse add {zone} <path>` them and retry (nothing was changed)",
            bad.join(", ")
        ))
    }
}
