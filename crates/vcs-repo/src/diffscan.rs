//! Fast structured diffs. The reference implementations (`Repo::diff_pairs_full`, flatten both
//! sides of a revision pair) read every tracked file or tree; these find the paths that can
//! differ first, so the cost follows what changed, not what is tracked:
//!
//! * working tree vs index: the stat-cache check of `status` (parallel `lstat`, no content reads
//!   for files whose size/mtime/ctime match), then content only for the files that changed;
//! * index vs tree(s): the cache-tree walk of `status`, which skips every directory whose id is
//!   equal on both sides, then content only for the changed paths;
//! * tree vs tree: `diff_trees_files`, which prunes equal subtrees by id.
//!
//! Every function returns exactly what the reference returns (tests compare them on random repos).

use crate::diffpairs::FilePair;
use crate::error::{Error, Result};
use crate::index::{Index, IndexEntry};
use crate::indexview::IndexView;
use crate::meta::{Workspace, Zones};
use crate::repo::{zones_fp, Repo};
use crate::treeops;
use crate::worktree_fs::StatCursor;
use std::collections::{BTreeMap, BTreeSet};
use vcs_objects::{EntryMode, ObjectId};
use vcs_store::ObjectStore;

/// `p` is one of the `only` paths or below one (no `only` or an empty one means everything).
pub fn kept(only: &[String], p: &str) -> bool {
    only.is_empty()
        || only
            .iter()
            .any(|f| f.is_empty() || p == f || (p.len() > f.len() && p.starts_with(f.as_str()) && p.as_bytes()[f.len()] == b'/'))
}

type Entry = (EntryMode, ObjectId);
type TreeCache = std::collections::HashMap<ObjectId, vcs_objects::Tree>;

impl Repo {
    /// True when `rev` resolves in at least one subscribed zone (what `rev_files(rev).is_ok()` tests,
    /// without flattening anything).
    pub fn rev_resolves(&self, rev: &str) -> bool {
        match self.workspace() {
            Ok(ws) => ws.subs.keys().any(|z| self.resolve_rev(z, rev).is_ok()),
            Err(_) => false,
        }
    }

    /// [`Repo::diff_pairs`] limited to the paths `only` (repository-relative; a directory stands for
    /// everything below it).
    pub fn diff_pairs_limited(&self, staged: bool, only: &[String]) -> Result<Vec<FilePair>> {
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let bytes = self.rd("index")?;
        let view = IndexView::parse(&bytes)?;
        let mut out = Vec::new();
        if staged {
            if ws.subs.len() != zones.names().count() {
                // an unsubscribed zone's tracked files are not covered by the cache-tree walk
                let mut v = self.diff_pairs_full(staged)?;
                v.retain(|p| kept(only, &p.path));
                return Ok(v);
            }
            let roots = self.head_roots(&ws)?;
            if view.as_ref().is_some_and(|v| trees_match(&zones, v, &roots)) {
                return Ok(out); // every zone's cached tree id is HEAD's: nothing is staged
            }
            let ix = Index::decode(&bytes)?;
            let mut cache = TreeCache::new();
            for p in self.index_vs_roots(&zones, &ix, &roots)? {
                if !kept(only, &p) {
                    continue;
                }
                let head = self.root_entry(&zones, &roots, &mut cache, &p)?;
                let e = ix.entries.get(&p);
                let (old, new) = (head.map(|h| h.1), e.map(|e| e.id));
                let (om, nm) = (head.map(|h| h.0), e.map(|e| e.mode));
                if old == new && om == nm {
                    continue;
                }
                if let (Some(o), Some(n), Some(e)) = (old, new, e) {
                    if om == nm && self.same_bytes_other_layout(e.size, &o, &n)? {
                        continue;
                    }
                }
                out.push(FilePair {
                    path: p,
                    old: old.map(|i| self.store.get_content(&i)).transpose()?,
                    new: new.map(|i| self.store.get_content(&i)).transpose()?,
                    old_mode: om,
                    new_mode: nm,
                });
            }
        } else {
            // With a running watcher and a saved state for this index, only the paths it names can differ.
            let narrowed = if only.is_empty() { self.monitored_candidates(&bytes) } else { None };
            if narrowed.as_ref().is_some_and(|c| c.is_empty()) {
                return Ok(out);
            }
            let only = narrowed.as_deref().unwrap_or(only);
            for (p, e) in self.worktree_changed(&bytes, view.as_ref(), &zones, &ws, only, true, false)? {
                if let Some(pair) = self.unstaged_pair(&p, &e)? {
                    out.push(pair);
                }
            }
        }
        Ok(out)
    }

    /// Changed files of revision `rev` against the working tree (`staged == false`; tracked files
    /// and files `rev` has) or against the index (`staged == true`), limited to `only`.
    pub fn rev_diff_pairs(&self, rev: &str, staged: bool, only: &[String]) -> Result<Vec<FilePair>> {
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let (roots, any) = self.rev_roots(&ws, rev)?;
        if !any {
            return Err(Error::Msg(format!("unknown revision {rev:?} in any subscribed zone")));
        }
        let bytes = self.rd("index")?;
        let view = IndexView::parse(&bytes)?;
        let all_subscribed = ws.subs.len() == zones.names().count();
        let tree_eq = all_subscribed && view.as_ref().is_some_and(|v| trees_match(&zones, v, &roots));
        if staged && tree_eq {
            return Ok(Vec::new()); // the index holds exactly `rev`'s trees
        }
        let mut paths: BTreeSet<String> = BTreeSet::new();
        let mut ix: Option<Index> = None;
        if !tree_eq {
            let i = Index::decode(&bytes)?;
            if all_subscribed {
                paths.extend(self.index_vs_roots(&zones, &i, &roots)?);
            } else {
                // slow and general: every path of either side
                for z in ws.subs.keys() {
                    if let Some(Some(root)) = roots.get(z.as_str()) {
                        paths.extend(treeops::flatten(&self.store, Some(*root), zones.path_of(z).unwrap_or(""))?.into_keys());
                    }
                }
                paths.extend(i.entries.keys().cloned());
            }
            ix = Some(i);
        }
        if !staged {
            // a symlink is read through to its target by this comparison: always look at it
            paths.extend(
                self.worktree_changed(&bytes, view.as_ref(), &zones, &ws, only, false, true)?
                    .into_iter()
                    .map(|(p, _)| p),
            );
        }
        let mut out = Vec::new();
        let mut cache = TreeCache::new();
        for k in paths {
            if !kept(only, &k) {
                continue;
            }
            let o = self.root_entry(&zones, &roots, &mut cache, &k)?;
            if staged {
                let e = ix.as_ref().and_then(|i| i.entries.get(&k));
                let n = e.map(|e| (e.mode, e.id));
                if o == n {
                    continue;
                }
                out.push(FilePair {
                    path: k,
                    old: o.map(|(_, id)| self.content(&id)).transpose()?,
                    new: n.map(|(_, id)| self.content(&id)).transpose()?,
                    old_mode: o.map(|(m, _)| m),
                    new_mode: n.map(|(m, _)| m),
                });
            } else {
                let new = std::fs::read(self.worktree().join(&k)).ok();
                let same = match (&o, &new) {
                    (Some((_, id)), Some(n)) => &self.content(id)? == n,
                    (None, None) => true,
                    _ => false,
                };
                if same {
                    continue;
                }
                out.push(FilePair {
                    path: k,
                    old: o.map(|(_, id)| self.content(&id)).transpose()?,
                    old_mode: o.map(|(m, _)| m),
                    new_mode: new.as_ref().map(|_| o.map_or(EntryMode::File, |(m, _)| m)),
                    new,
                });
            }
        }
        Ok(out)
    }

    /// Changed files between revisions `a` and `b` (`diff A B`), limited to `only`, sorted by path.
    /// Subtrees with equal ids are never read.
    pub fn rev_rev_pairs(&self, a: &str, b: &str, only: &[String]) -> Result<Vec<FilePair>> {
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let (ra, any_a) = self.rev_roots(&ws, a)?;
        let (rb, any_b) = self.rev_roots(&ws, b)?;
        for (rev, any) in [(a, any_a), (b, any_b)] {
            if !any {
                return Err(Error::Msg(format!("unknown revision {rev:?} in any subscribed zone")));
            }
        }
        let (mut old, mut new) = (treeops::Flat::new(), treeops::Flat::new());
        for z in ws.subs.keys() {
            let (x, y) = (ra.get(z.as_str()).copied().flatten(), rb.get(z.as_str()).copied().flatten());
            let d = treeops::diff_trees_files(&self.store, x, y, zones.path_of(z).unwrap_or(""))?;
            old.extend(d.old);
            new.extend(d.new);
        }
        let mut keys: BTreeSet<&String> = old.keys().collect();
        keys.extend(new.keys());
        let mut out = Vec::new();
        for k in keys {
            if !kept(only, k) {
                continue;
            }
            let (o, n) = (old.get(k), new.get(k));
            if o == n {
                continue;
            }
            out.push(FilePair {
                path: k.clone(),
                old: o.map(|(_, id)| self.content(id)).transpose()?,
                new: n.map(|(_, id)| self.content(id)).transpose()?,
                old_mode: o.map(|(m, _)| *m),
                new_mode: n.map(|(m, _)| *m),
            });
        }
        Ok(out)
    }

    /// Root tree of the branch checked out in each subscribed zone (`None`: no commits yet).
    fn head_roots<'w>(&self, ws: &'w Workspace) -> Result<BTreeMap<&'w str, Option<ObjectId>>> {
        let refs = self.refs()?;
        let mut m = BTreeMap::new();
        for z in ws.subs.keys() {
            let root = match self.current_head(z, ws, &refs) {
                Some(h) => Some(self.commit_obj(&h)?.root),
                None => None,
            };
            m.insert(z.as_str(), root);
        }
        Ok(m)
    }

    /// Root tree of `rev` in each subscribed zone where it resolves (`None` elsewhere), and whether
    /// it resolved anywhere.
    fn rev_roots<'w>(&self, ws: &'w Workspace, rev: &str) -> Result<(BTreeMap<&'w str, Option<ObjectId>>, bool)> {
        let mut m = BTreeMap::new();
        let mut any = false;
        for z in ws.subs.keys() {
            let root = match self.resolve_rev(z, rev) {
                Ok(id) => {
                    any = true;
                    Some(self.commit_obj(&id)?.root)
                }
                Err(_) => None,
            };
            m.insert(z.as_str(), root);
        }
        Ok((m, any))
    }

    /// Paths whose index entry differs from the tree in `roots` (or that one side lacks), found
    /// without reading directories whose ids agree.
    fn index_vs_roots(&self, zones: &Zones, ix: &Index, roots: &BTreeMap<&str, Option<ObjectId>>) -> Result<BTreeSet<String>> {
        let fp = zones_fp(zones);
        let mut out = BTreeSet::new();
        for (zone, root) in roots {
            let mut it = treeops::IndexTrees::new(&self.store, &ix.entries, zones, zone, ix.entries.cached_trees(&fp));
            out.extend(it.diff(zones.path_of(zone).unwrap_or(""), *root)?.into_iter().map(|(p, _)| p));
        }
        Ok(out)
    }

    /// The file entry at repository path `p` in the zone tree that owns it (directories count as absent).
    fn root_entry(&self, zones: &Zones, roots: &BTreeMap<&str, Option<ObjectId>>, cache: &mut TreeCache, p: &str) -> Result<Option<Entry>> {
        let z = zones.owner(p);
        let Some(Some(root)) = roots.get(z) else { return Ok(None) };
        let rel = if z == crate::meta::ROOT_ZONE || zones.path_of(z).unwrap_or("").is_empty() {
            p
        } else {
            let zp = zones.path_of(z).unwrap_or("");
            match p.strip_prefix(zp).and_then(|r| r.strip_prefix('/')) {
                Some(r) => r,
                None => return Ok(None),
            }
        };
        Ok(match treeops::entry_at_cached(&self.store, cache, *root, rel)? {
            Some((EntryMode::Directory, _)) | None => None,
            e => e,
        })
    }

    /// Tracked entries (limited to `only`) whose file may differ from the index: the status stat-cache
    /// check, in parallel, on the borrowed `AGI3` view when there is one. A superset of the files whose
    /// bytes differ. `skip_sparse` leaves out paths outside a zone's sparse set; `symlinks` also returns
    /// every symlink entry.
    #[allow(clippy::too_many_arguments)]
    fn worktree_changed(
        &self,
        bytes: &[u8],
        view: Option<&IndexView<'_>>,
        zones: &Zones,
        ws: &Workspace,
        only: &[String],
        skip_sparse: bool,
        symlinks: bool,
    ) -> Result<Vec<(String, IndexEntry)>> {
        let sparse = skip_sparse && !ws.sparse.is_empty();
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(16);
        let Some(view) = view else {
            let ix = Index::decode(bytes)?;
            let mut list: Vec<(&String, &IndexEntry)> = Vec::new();
            let keep = |p: &String| kept(only, p) && (!sparse || !ws.skips(zones.owner(p), p));
            list.extend(ix.entries.iter().filter(|(p, _)| keep(p)));
            let (changes, _clean) = self.unstaged_changes(&list, ix.written_ns)?;
            let mut out: Vec<(String, IndexEntry)> = changes.into_iter().map(|c| (c.path.clone(), ix.entries[&c.path].clone())).collect();
            if symlinks {
                let have: BTreeSet<String> = out.iter().map(|x| x.0.clone()).collect();
                out.extend(
                    ix.entries
                        .iter()
                        .filter(|(p, e)| e.mode == EntryMode::Symlink && !have.contains(*p))
                        .map(|(p, e)| (p.clone(), e.clone())),
                );
                out.sort_by(|a, b| a.0.cmp(&b.0));
            }
            return Ok(out);
        };
        let blocks = view.blocks_for(only)?;
        let work = |cur: &mut StatCursor, p: &str, e: &IndexEntry| -> Result<Option<(String, IndexEntry)>> {
            if !kept(only, p) || (sparse && ws.skips(zones.owner(p), p)) {
                return Ok(None);
            }
            let (st, _) = self.wt_state_seq(cur, p, e, view.written_ns)?;
            Ok(match st {
                crate::repo::Wt::Clean if !(symlinks && e.mode == EntryMode::Symlink) => None,
                _ => {
                    // the scan does not validate names (clean files are only stat'ed); a path that leaves the
                    // scan is checked like the full decode would
                    vcs_objects::names::validate_path(p)?;
                    Some((p.to_owned(), e.clone()))
                }
            })
        };
        view.par_filter_map(&blocks, threads, &work)
    }

    /// The reference per-file comparison of the working tree against index entry `e`.
    fn unstaged_pair(&self, p: &str, e: &IndexEntry) -> Result<Option<FilePair>> {
        let abs = self.worktree.join(p);
        if let Ok(m) = std::fs::symlink_metadata(&abs) {
            if m.is_file() && e.mode != EntryMode::Symlink && self.large_unchanged(&abs, e.size, m.len(), &e.id)? {
                return Ok(None);
            }
        }
        let cur = match std::fs::symlink_metadata(&abs) {
            Ok(m) if m.file_type().is_symlink() && e.mode == EntryMode::Symlink => {
                Some(crate::repo::read_link_bytes(&abs).map_err(|source| Error::Io { path: abs.clone(), source })?)
            }
            Ok(m) if m.is_file() && e.mode != EntryMode::Symlink => {
                Some(std::fs::read(&abs).map_err(|source| Error::Io { path: abs.clone(), source })?)
            }
            Ok(_) => return Ok(None),
            Err(x) if x.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => return Err(Error::Io { path: abs, source }),
        };
        let old = self.store.get_content(&e.id)?;
        if cur.as_deref() == Some(&old[..]) {
            return Ok(None);
        }
        Ok(Some(FilePair {
            path: p.to_owned(),
            old: Some(old),
            new: cur.clone(),
            old_mode: Some(e.mode),
            new_mode: cur.map(|_| e.mode),
        }))
    }
}

/// Does the cache-tree of the index already say that every zone's tree equals the one in `roots`?
/// (`false` also means "cannot tell": a directory with too few files is not cached.)
fn trees_match(zones: &Zones, view: &IndexView<'_>, roots: &BTreeMap<&str, Option<ObjectId>>) -> bool {
    if view.trees_fp != Some(zones_fp(zones)) {
        return false;
    }
    roots
        .iter()
        .all(|(zone, root)| match (root, view.trees.get(zones.path_of(zone).unwrap_or(""))) {
            (Some(r), Some(c)) => r == c,
            _ => false,
        })
}
