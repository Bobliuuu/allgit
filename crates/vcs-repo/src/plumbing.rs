//! Read-side helpers for the plumbing commands (`grep`, `ls-files`, `archive`, `cat-file`,
//! `describe`) and the checkout used by `bisect`.

use crate::error::{msg, Error, Result};
use crate::meta::ROOT_ZONE;
use crate::repo::{Repo, StatusOpts};
use crate::treeops::{self, Flat};
use vcs_objects::names::validate_user_branch_name;
use vcs_objects::{Commit, Object, ObjectId, RefUpdate};
use vcs_store::ObjectStore;

impl Repo {
    /// Tracked files of the working tree (the index), as `(path, mode, content id)` sorted by path.
    /// Paths are repository-relative; only subscribed zones are tracked.
    pub fn tracked_files(&self) -> Result<Vec<(String, vcs_objects::EntryMode, ObjectId)>> {
        Ok(self.index_image()?.list()?.into_iter().map(|(p, e)| (p, e.mode, e.id)).collect())
    }

    /// Stream the staged entries (same validation as [`Repo::tracked_files`]) through `visit` on
    /// parallel workers, one `T` per index block, in path order. See [`crate::index::Index::scan`].
    pub fn scan_tracked<T: Send>(
        &self,
        init: impl Fn() -> T + Sync,
        visit: impl Fn(&mut T, &str, &crate::index::IndexEntry) + Sync,
    ) -> Result<Vec<T>> {
        let img = self.index_image()?;
        // AGI5 (and AGI3): walk the mapped blocks directly, no 100k-entry decode into memory
        if let Some(view) = self.index_view(&img)? {
            let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
            return view.par_scan(threads, &init, &visit);
        }
        crate::index::Index::scan(&img.flat()?, init, visit)
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

    /// The entries directly inside the root of revision `rev` (files and directories, as
    /// `ls-tree` lists them), read from the one top tree. Directories carry `EntryMode::Directory`
    /// and their tree id. `None` when the answer is not one tree: `rev` resolves in several
    /// zones, or in a zone that lives below the repository root.
    pub fn rev_top_level(&self, rev: &str) -> Result<Option<Vec<(String, vcs_objects::EntryMode, ObjectId)>>> {
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut hits = Vec::new();
        for z in ws.subs.keys() {
            if let Ok(id) = self.resolve_rev(z, rev) {
                hits.push((z, id));
            }
        }
        match hits.as_slice() {
            [] => msg(format!("unknown revision {rev:?} in any subscribed zone")),
            [(z, id)] if zones.path_of(z).unwrap_or("").is_empty() => {
                let root = self.commit_obj(id)?.root;
                let Object::Tree(t) = self.store.get(&root)? else {
                    return Err(Error::Corrupt(format!("object {root} is not a tree")));
                };
                Ok(Some(t.entries().iter().map(|e| (e.name.clone(), e.mode, e.id)).collect()))
            }
            _ => Ok(None),
        }
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
        validate_user_branch_name(branch)?;
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

/// What `count-objects` reports: loose objects and pack files of the object store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ObjectStats {
    pub loose: usize,
    pub loose_bytes: u64,
    pub packs: usize,
    pub pack_bytes: u64,
}

impl Repo {
    /// Store file content (a plain blob, or chunks plus a manifest for a large file) and return the
    /// id a tree would name it by; the same as `vcs add` would store for a file with these bytes.
    pub fn store_content(&self, data: &[u8]) -> Result<ObjectId> {
        let _l = self.lock()?;
        let id = self.store.put_content(data)?;
        self.store.sync()?;
        Ok(id)
    }

    /// Loose objects (the `xx/yyyy...` files) and pack files in the object store, counted from disk.
    pub fn object_stats(&self) -> Result<ObjectStats> {
        let dir = self.common.join("objects");
        let mut st = ObjectStats::default();
        let rd = crate::error::io(&dir, std::fs::read_dir(&dir))?;
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let name = e.file_name().to_string_lossy().into_owned();
            if ft.is_dir() && name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
                for f in std::fs::read_dir(e.path()).into_iter().flatten().flatten() {
                    if let Ok(m) = f.metadata() {
                        if m.is_file() {
                            st.loose += 1;
                            st.loose_bytes += m.len();
                        }
                    }
                }
            } else if ft.is_dir() && name == "pack" {
                for f in std::fs::read_dir(e.path()).into_iter().flatten().flatten() {
                    let fname = f.file_name().to_string_lossy().into_owned();
                    if let Ok(m) = f.metadata() {
                        if fname.ends_with(".pack") {
                            st.packs += 1;
                        }
                        if fname.ends_with(".pack") || fname.ends_with(".idx") {
                            st.pack_bytes += m.len();
                        }
                    }
                }
            }
        }
        Ok(st)
    }

    /// The tree the index holds for `zone` (git `write-tree`): stored, its id returned. `None` when
    /// the zone has nothing staged and no commits (there is no tree to write).
    pub fn write_tree(&self, zone: &str) -> Result<Option<ObjectId>> {
        let zone = self.resolve_zone(zone)?;
        let _l = self.lock()?;
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let img = self.index_image()?;
        let fp = crate::repo::zones_fp(&zones);
        let parent_root = match ws.subs.get(&zone).and_then(|b| refs.head(&zone, b)) {
            Some(p) => Some(self.commit_obj(&p)?.root),
            None => None,
        };
        let zpath = zones.path_of(&zone).unwrap_or("");
        let cached = img.trees()?.filter(|(f, _)| *f == fp).map(|(_, m)| m);
        let mut it = treeops::IndexTrees::new(&self.store, &img, &zones, &zone, cached);
        if parent_root.is_none() && it.is_empty(zpath)? {
            return Ok(None);
        }
        let root = it.build_root(zpath, parent_root)?;
        self.store.sync()?;
        Ok(Some(root))
    }

    /// A commit object with the given tree and parents, stored but not referenced by any branch
    /// (git `commit-tree`). Every parent must be a commit of `zone`; `tree` must be a tree object.
    pub fn commit_tree(
        &self,
        zone: &str,
        tree: &ObjectId,
        parents: &[ObjectId],
        message: &str,
        author: vcs_objects::Signature,
    ) -> Result<ObjectId> {
        let zone = self.resolve_zone(zone)?;
        if message.trim().is_empty() {
            return msg("empty commit message");
        }
        if !matches!(self.store.get(tree)?, Object::Tree(_)) {
            return msg(format!("{} is not a tree", tree.to_hex()));
        }
        for p in parents {
            if self.commit_obj(p)?.zone != zone {
                return msg(format!("parent {} is not a commit of zone {zone:?}", p.short()));
            }
        }
        let _l = self.lock()?;
        let change_id = crate::repo::fresh_change_id(&zone, tree, parents.first(), &author);
        let id = self.store.put(&Object::Commit(Commit {
            zone,
            parents: parents.to_vec(),
            root: *tree,
            change_id,
            author,
            message: message.to_owned(),
        }))?;
        self.store.sync()?;
        Ok(id)
    }

    /// Point `zone`'s branch at commit `to` without touching the index or any file (git
    /// `update-ref refs/heads/<branch> <to>`); creates the branch when it does not exist. With
    /// `expect` the move only happens when the branch currently is (or is not: `Some(None)`) there.
    /// Refuses a branch another worktree has checked out. Returns the previous head.
    pub fn branch_set(&self, zone: &str, branch: &str, to: &ObjectId, expect: Option<Option<ObjectId>>) -> Result<Option<ObjectId>> {
        let zone = self.resolve_zone(zone)?;
        validate_branch_name(branch)?;
        if self.commit_obj(to)?.zone != zone {
            return msg(format!("commit {} is not in zone {zone:?}", to.short()));
        }
        let _l = self.lock()?;
        let mut refs = self.refs()?;
        let old = refs.head(&zone, branch);
        if let Some(e) = expect {
            if e != old {
                return msg(format!(
                    "cannot lock ref {zone}/{branch}: is at {} but expected {}",
                    old.map_or("nothing".to_owned(), |i| i.to_hex()),
                    e.map_or("nothing".to_owned(), |i| i.to_hex())
                ));
            }
        }
        if let Some(wt) = self.branch_holder(&zone, branch)? {
            return msg(format!("branch {zone}/{branch} is checked out in the worktree {}", wt.display()));
        }
        if old == Some(*to) {
            return Ok(old);
        }
        refs.apply(&[RefUpdate {
            zone: zone.clone(),
            branch: branch.to_owned(),
            old,
            new: *to,
        }])?;
        self.commit_refs_op_lazy(&refs, &format!("update-ref {zone}/{branch} to {}", to.short()), None)?;
        Ok(old)
    }
}
