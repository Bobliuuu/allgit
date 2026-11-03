//! Read-side helpers for the plumbing commands (`grep`, `ls-files`, `archive`, `cat-file`,
//! `describe`) and the checkout used by `bisect`.

use crate::error::{msg, Error, Result};
use crate::meta::ROOT_ZONE;
use crate::repo::{Repo, StatusOpts};
use crate::treeops::{self, Flat};
use vcs_objects::names::validate_branch_name;
use vcs_objects::{Commit, Object, ObjectId, RefUpdate};
use vcs_store::ObjectStore;

impl Repo {
    /// Tracked files of the working tree (the index), as `(path, mode, content id)` sorted by path.
    /// Paths are repository-relative; only subscribed zones are tracked.
    pub fn tracked_files(&self) -> Result<Vec<(String, vcs_objects::EntryMode, ObjectId)>> {
        let raw = self.rd("index")?;
        Ok(crate::index::Index::decode_list(&raw)?
            .into_iter()
            .map(|(p, e)| (p, e.mode, e.id))
            .collect())
    }

    /// Stream the staged entries (same validation as [`Repo::tracked_files`]) through `visit` on
    /// parallel workers, one `T` per index block, in path order. See [`crate::index::Index::scan`].
    pub fn scan_tracked<T: Send>(
        &self,
        init: impl Fn() -> T + Sync,
        visit: impl Fn(&mut T, &str, &crate::index::IndexEntry) + Sync,
    ) -> Result<Vec<T>> {
        let raw = self.rd("index")?;
        crate::index::Index::scan(&raw, init, visit)
    }

    /// Files of revision `rev` across the subscribed zones in which `rev` resolves (so `HEAD`
    /// covers every zone, a commit-id prefix just its own). Keys are repository-relative.
    pub fn rev_files(&self, rev: &str) -> Result<Flat> {
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut out = Flat::new();
        let mut any = false;
        for z in ws.subs.keys() {
            let Ok(id) = self.resolve_rev(z, rev) else { continue };
            any = true;
            let root = self.commit_obj(&id)?.root;
            out.extend(treeops::flatten(&self.store, Some(root), zones.path_of(z).unwrap_or(""))?);
        }
        if !any {
            return msg(format!("unknown revision {rev:?} in any subscribed zone"));
        }
        Ok(out)
    }

    /// `rev_files(rev)` as a path-ordered list. With one zone resolving `rev` (the usual case) the
    /// trees are walked straight into a vector, with no map in between.
    pub fn rev_file_list(&self, rev: &str) -> Result<Vec<(String, vcs_objects::EntryMode, ObjectId)>> {
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut hits = Vec::new();
        for z in ws.subs.keys() {
            if let Ok(id) = self.resolve_rev(z, rev) {
                hits.push((z, id));
            }
        }
        if hits.len() != 1 {
            return Ok(self.rev_files(rev)?.into_iter().map(|(p, (m, i))| (p, m, i)).collect());
        }
        let (z, id) = hits[0];
        let root = self.commit_obj(&id)?.root;
        let mut out = Vec::new();
        treeops::flatten_into(&self.store, root, zones.path_of(z).unwrap_or(""), &mut out)?;
        if !out.windows(2).all(|w| w[0].0 < w[1].0) {
            out.sort_by(|a, b| a.0.cmp(&b.0));
        }
        Ok(out)
    }

    /// The file entry for repository-relative `rel` in revision `rev`, with the same answer as
    /// `rev_files(rev).get(rel)` but reading only one tree per path component per zone. A
    /// directory, or no file at all, is `None`.
    pub fn rev_file_entry(&self, rev: &str, rel: &str) -> Result<Option<(vcs_objects::EntryMode, vcs_objects::ObjectId)>> {
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut found = None;
        let mut any = false;
        for z in ws.subs.keys() {
            let Ok(id) = self.resolve_rev(z, rev) else { continue };
            any = true;
            let prefix = zones.path_of(z).unwrap_or("");
            let zrel = if prefix.is_empty() {
                Some(rel)
            } else {
                rel.strip_prefix(prefix).and_then(|r| r.strip_prefix('/'))
            };
            let Some(zrel) = zrel.filter(|r| !r.is_empty()) else { continue };
            let root = self.commit_obj(&id)?.root;
            if let Some((m, i)) = treeops::entry_at(&self.store, root, zrel)? {
                if m != vcs_objects::EntryMode::Directory {
                    found = Some((m, i));
                }
            }
        }
        if !any {
            return msg(format!("unknown revision {rev:?} in any subscribed zone"));
        }
        Ok(found)
    }

    /// Commit `id` (any zone).
    pub fn commit_of(&self, id: &ObjectId) -> Result<Commit> {
        self.commit_obj(id)
    }

    /// File content by id (blob, or a chunked file reassembled and verified).
    pub fn content(&self, id: &ObjectId) -> Result<Vec<u8>> {
        Ok(self.store.get_content(id)?)
    }

    /// Head commit of the branch checked out in `zone`, if any.
    pub fn head_commit(&self, zone: &str) -> Result<Option<ObjectId>> {
        let ws = self.workspace()?;
        let refs = self.refs()?;
        Ok(self.current_head(zone, &ws, &refs))
    }

    /// Name of the branch checked out in `zone`.
    pub fn current_branch(&self, zone: &str) -> Result<Option<String>> {
        Ok(self
            .workspace()?
            .subs
            .get(zone)
            .filter(|b| *b != vcs_objects::names::DETACHED_BRANCH)
            .cloned())
    }

    /// Point `branch` of `zone` at `target` (creating it) and check it out in the working tree.
    /// Same safety as `checkout`: refuses with uncommitted changes or a merge in progress. The
    /// worktree and index move before the refs and workspace, so an interruption leaves the
    /// old branch intact; running it again completes the move.
    pub fn checkout_commit(&self, zone: &str, branch: &str, target: &ObjectId) -> Result<()> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        validate_branch_name(branch)?;
        let _l = self.lock()?;
        let zones = self.zones()?;
        let mut ws = self.workspace()?;
        let mut refs = self.refs()?;
        if !ws.subs.contains_key(zone) {
            return msg(format!("zone {zone:?} is not subscribed in this workspace"));
        }
        if self.commit_obj(target)?.zone != zone {
            return Err(Error::Msg(format!("commit {} is not in zone {zone:?}", target.short())));
        }
        let mut ix = self.index()?;
        let st = self.status_inner(StatusOpts::default(), &ix, false, None)?;
        self.require_clean(zone, &st)?;
        let prefix = zones.path_of(zone).unwrap_or("");
        let old_root = match self.current_head(zone, &ws, &refs) {
            Some(h) => Some(self.commit_obj(&h)?.root),
            None => None,
        };
        let new_root = self.commit_obj(target)?.root;
        let diff = treeops::diff_trees(&self.store, old_root, Some(new_root), prefix)?;
        self.transition_diff(zone, &zones, &diff, &mut ix)?;
        let cur = refs.head(zone, branch);
        if cur != Some(*target) {
            refs.apply(&[RefUpdate {
                zone: zone.into(),
                branch: branch.into(),
                old: cur,
                new: *target,
            }])?;
        }
        ws.subs.insert(zone.into(), branch.into());
        self.save_index(&mut ix)?;
        self.write_refs(&refs)?;
        self.save_ws(&ws)?;
        self.record_op(&format!("checkout {zone}/{branch} at {}", target.short()), None)?;
        Ok(())
    }

    /// Whether the object exists and what kind it is (for `cat-file`).
    pub fn object_kind(&self, id: &ObjectId) -> Result<&'static str> {
        Ok(match self.store.get(id)? {
            Object::Blob(_) => "blob",
            Object::Chunked(_) => "blob",
            Object::Commit(_) => "commit",
            Object::Tag(_) => "tag",
            _ => "other",
        })
    }

    /// The root zone's id.
    pub fn root_zone() -> &'static str {
        ROOT_ZONE
    }
}

impl Repo {
    /// Newest commit time among the commits `rev` names across the subscribed zones (what
    /// `archive` stamps on every entry, so output depends only on the revision).
    pub fn rev_time(&self, rev: &str) -> Result<u64> {
        let ws = self.workspace()?;
        let mut t = None::<u64>;
        for z in ws.subs.keys() {
            if let Ok(id) = self.resolve_rev(z, rev) {
                let ts = self.commit_obj(&id)?.author.timestamp;
                t = Some(t.map_or(ts, |x| x.max(ts)));
            }
        }
        t.ok_or_else(|| Error::Msg(format!("unknown revision {rev:?} in any subscribed zone")))
    }
}
