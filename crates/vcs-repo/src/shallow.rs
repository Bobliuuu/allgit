//! Shallow repositories: history truncated at a recorded boundary.
//!
//! `<common>/shallow` lists, one hex id per line, the commits whose parents were left on the
//! remote on purpose. The first time one is written the `shallow` feature is recorded in
//! `<common>/features`, so a binary that does not know this file refuses to open the repository
//! instead of reading a truncated history as a complete one. Every object that is present is
//! still hash-verified; a parent that is absent and whose child is not in the list stays an error.
//!
//! A boundary entry is a permission, not a claim: a listed commit whose parents are (all or
//! some of them) stored is read normally, so a boundary that became stale (a fetch brought the
//! old side in through a merge) cannot hide history.

use crate::error::{msg, Error, Result};
use crate::meta::write_atomic;
use crate::repo::Repo;
use std::collections::BTreeSet;
use std::fs;
use std::sync::Arc;
use vcs_objects::ObjectId;
use vcs_remote::packfetch::PackCatalog;
use vcs_remote::shallow::{fetch_commits_cut, Cut};
use vcs_remote::{Backend, Manifest};
use vcs_store::ObjectStore;

pub(crate) const SHALLOW_FILE: &str = "shallow";
pub const FEATURE_SHALLOW: &str = "shallow";

/// Text of the error every command gives when it needs history past the boundary.
pub const SHALLOW_HINT: &str =
    "this repository is shallow: that history was not downloaded (`vcs fetch --deepen N` fetches N more commits, `vcs fetch --unshallow` all of it)";

/// What [`Repo::fetch_deepen`] did.
#[derive(Debug, Default)]
pub struct DeepenReport {
    /// Heads fetched by the ordinary fetch that runs first.
    pub fetched: Vec<(String, String, ObjectId)>,
    /// Boundary commits that were extended (or removed).
    pub extended: usize,
    /// Commits still at the boundary afterwards.
    pub boundary: usize,
}

impl Repo {
    /// The recorded boundary commits (empty for a complete repository).
    pub fn shallow_boundary(&self) -> Result<BTreeSet<ObjectId>> {
        let Ok(text) = fs::read_to_string(self.common.join(SHALLOW_FILE)) else {
            return Ok(BTreeSet::new());
        };
        let mut out = BTreeSet::new();
        for l in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            out.insert(ObjectId::from_hex(l).map_err(|e| Error::Corrupt(format!("shallow file: {l:?}: {e}")))?);
        }
        Ok(out)
    }

    pub fn is_shallow(&self) -> bool {
        self.shallow_boundary().map(|b| !b.is_empty()).unwrap_or(true)
    }

    pub(crate) fn write_shallow(&self, set: &BTreeSet<ObjectId>) -> Result<()> {
        if !set.is_empty() {
            self.note_feature(FEATURE_SHALLOW)?; // before the file: an old binary must never see the file without the gate
        }
        let mut s = String::new();
        for id in set {
            s.push_str(&id.to_hex());
            s.push('\n');
        }
        write_atomic(&self.common.join(SHALLOW_FILE), s.as_bytes())
    }

    /// If `missing` is a parent of a boundary commit, the explanation to give instead of a bare
    /// "object not found" (and instead of fetching that one commit behind the user's back).
    pub fn shallow_hint_for(&self, missing: &ObjectId) -> Option<String> {
        let b = self.shallow_boundary().ok()?;
        for id in b {
            if let Ok(c) = self.commit_obj(&id) {
                if c.parents.contains(missing) {
                    return Some(format!("history is shallow: commit {} is before the boundary at {}; {SHALLOW_HINT}", missing.short(), id.short()));
                }
            }
        }
        None
    }

    /// Heads of every branch of `want` zones in `m`, with history cut at `cut`. Returns the heads
    /// (as [`vcs_remote::fetch_heads_lazy_shared`] would) and the commits left without parents.
    pub(crate) fn shallow_heads(
        &self,
        backend: &dyn Backend,
        want: &[String],
        m: &Manifest,
        cat: &Arc<PackCatalog>,
        cut: Cut,
    ) -> Result<(Vec<(String, String, ObjectId)>, BTreeSet<ObjectId>)> {
        let mut out = Vec::new();
        let mut starts = Vec::new();
        for ((a, b), id) in &m.branches {
            if want.iter().any(|w| w == a) {
                out.push((a.clone(), b.clone(), *id));
                starts.push(*id);
            }
        }
        let boundary = fetch_commits_cut(backend, &self.store, cat, &starts, Some(cut)).map_err(|e| Error::Msg(e.to_string()))?;
        Ok((out, boundary))
    }

    /// `vcs fetch --deepen N` (`Some(n)`) or `--unshallow` (`None`): fetch as usual, then extend
    /// every boundary commit (of `zones`, default all) by `n` more commits, or to the roots.
    pub fn fetch_deepen(&self, backend: &dyn Backend, zones: Option<&[String]>, deepen: Option<u32>) -> Result<DeepenReport> {
        if deepen == Some(0) {
            return msg("--deepen needs at least 1");
        }
        let _l = self.lock()?;
        let boundary = self.shallow_boundary()?;
        if boundary.is_empty() {
            return msg("this repository is not shallow");
        }
        let fetched = self.fetch_locked(backend, zones)?;
        let filter = self.resolve_filter(zones)?;
        let idx_cache = vcs_remote::IdxCacheBackend::new(backend, self.common.join("remote-idx"));
        let backend: &dyn Backend = &idx_cache;
        let mut keep = BTreeSet::new();
        let mut todo = Vec::new();
        let mut starts = Vec::new();
        for id in boundary {
            let c = self.commit_obj(&id)?;
            if filter.as_ref().is_some_and(|w| !w.contains(&c.zone)) {
                keep.insert(id);
                continue;
            }
            todo.push(id);
            starts.extend(c.parents.iter().copied());
        }
        let cat = PackCatalog::new();
        let cut = deepen.map(Cut::Depth);
        let added = fetch_commits_cut(backend, &self.store, &cat, &starts, cut).map_err(|e| Error::Msg(e.to_string()))?;
        let mut extended = 0;
        for id in todo {
            if self.commit_obj(&id)?.parents.iter().all(|p| self.store.contains(p).unwrap_or(false)) {
                extended += 1;
            } else {
                keep.insert(id); // a parent could not be fetched: stays a boundary
            }
        }
        keep.extend(added);
        self.write_shallow(&keep)?;
        Ok(DeepenReport {
            fetched,
            extended,
            boundary: keep.len(),
        })
    }
}
