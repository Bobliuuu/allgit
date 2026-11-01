//! Structured diff input: per-file old/new content, for renderers that need more than the
//! fixed unified text of [`Repo::diff`] (rename detection, word diff, name-status, ...).
//! Chunked files are read through `get_content`, so they arrive reassembled.

use crate::error::Result;
use crate::repo::Repo;
use crate::treeops;
use std::collections::BTreeSet;
use vcs_objects::{EntryMode, ObjectId};
use vcs_store::ObjectStore;

/// One changed path. `old`/`new` are `None` when the file does not exist on that side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePair {
    pub path: String,
    pub old: Option<Vec<u8>>,
    pub new: Option<Vec<u8>>,
    pub old_mode: Option<EntryMode>,
    pub new_mode: Option<EntryMode>,
}

impl Repo {
    /// Changed files of the same comparison as [`Repo::diff`] (`staged`: index vs branch heads,
    /// otherwise working tree vs index), as structured pairs, sorted by path.
    pub fn diff_pairs(&self, staged: bool) -> Result<Vec<FilePair>> {
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let ix = self.index()?;
        let mut out = Vec::new();
        if staged {
            let mut heads = treeops::Flat::new();
            for zone in ws.subs.keys() {
                heads.extend(self.head_flat(zone, &zones, &ws, &refs)?);
            }
            let mut paths: BTreeSet<&String> = heads.keys().collect();
            paths.extend(ix.entries.keys());
            for p in paths {
                let old = heads.get(p).map(|(_, id)| *id);
                let new = ix.entries.get(p).map(|e| e.id);
                let (om, nm) = (heads.get(p).map(|h| h.0), ix.entries.get(p).map(|e| e.mode));
                if old == new && om == nm {
                    continue;
                }
                out.push(FilePair {
                    path: p.clone(),
                    old: old.map(|i| self.store.get_content(&i)).transpose()?,
                    new: new.map(|i| self.store.get_content(&i)).transpose()?,
                    old_mode: om,
                    new_mode: nm,
                });
            }
        } else {
            for (p, e) in &ix.entries {
                if ws.skips(zones.owner(p), p) {
                    continue;
                }
                let abs = self.worktree.join(p);
                let cur = match std::fs::symlink_metadata(&abs) {
                    Ok(m) if m.file_type().is_symlink() && e.mode == EntryMode::Symlink => {
                        Some(crate::repo::read_link_bytes(&abs).map_err(|source| crate::Error::Io { path: abs.clone(), source })?)
                    }
                    Ok(m) if m.is_file() && e.mode != EntryMode::Symlink => {
                        Some(std::fs::read(&abs).map_err(|source| crate::Error::Io { path: abs.clone(), source })?)
                    }
                    Ok(_) => continue,
                    Err(x) if x.kind() == std::io::ErrorKind::NotFound => None,
                    Err(source) => return Err(crate::Error::Io { path: abs, source }),
                };
                let old = self.store.get_content(&e.id)?;
                if cur.as_deref() == Some(&old[..]) {
                    continue;
                }
                out.push(FilePair {
                    path: p.clone(),
                    old: Some(old),
                    new: cur.clone(),
                    old_mode: Some(e.mode),
                    new_mode: cur.map(|_| e.mode),
                });
            }
        }
        Ok(out)
    }

    /// Changed files of commit `id` against its first parent (the empty tree for a root commit),
    /// sorted by path. Paths are repository-relative.
    pub fn commit_pairs(&self, zone: &str, id: &ObjectId) -> Result<Vec<FilePair>> {
        self.commit_pairs_inner(zone, id, true)
    }

    /// Like [`Repo::commit_pairs`] but file contents are not read: `old`/`new` are `Some(empty)` where the
    /// file exists on that side. Enough for listings of names and add/delete/modify status.
    pub fn commit_pairs_names(&self, zone: &str, id: &ObjectId) -> Result<Vec<FilePair>> {
        self.commit_pairs_inner(zone, id, false)
    }

    fn commit_pairs_inner(&self, zone: &str, id: &ObjectId, content: bool) -> Result<Vec<FilePair>> {
        let zone = self.resolve_zone(zone)?;
        let prefix = self.zones()?.path_of(&zone).unwrap_or("").to_owned();
        let commit = self.commit_obj(id)?;
        let parent = match commit.parents.first() {
            Some(p) => Some(self.commit_obj(p)?),
            None => None,
        };
        // only the parts of the two trees that differ (equal subtrees are not read)
        let d = treeops::diff_trees_files(&self.store, parent.map(|c| c.root), Some(commit.root), &prefix)?;
        let (old, new) = (d.old, d.new);
        let mut paths: BTreeSet<&String> = old.keys().collect();
        paths.extend(new.keys());
        let mut out = Vec::new();
        for p in paths {
            let (o, n) = (old.get(p), new.get(p));
            if o == n {
                continue;
            }
            out.push(FilePair {
                path: p.clone(),
                old: o
                    .map(|(_, i)| if content { self.store.get_content(i) } else { Ok(Vec::new()) })
                    .transpose()?,
                new: n
                    .map(|(_, i)| if content { self.store.get_content(i) } else { Ok(Vec::new()) })
                    .transpose()?,
                old_mode: o.map(|x| x.0),
                new_mode: n.map(|x| x.0),
            });
        }
        Ok(out)
    }
}
