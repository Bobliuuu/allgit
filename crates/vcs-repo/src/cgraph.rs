//! The commit-graph cache, one file per zone (`.vcs/commit-graphs/<hash of zone id>`), see
//! `vcs_merge::CommitGraphFile`. A parent always lives in its child's zone, so a zone's graph
//! is closed and a commit in one zone never rewrites another zone's file.
//!
//! It is a pure cache: read paths ask for it, and anything wrong with it (missing, truncated,
//! checksum or consistency failure, an unreadable commit) means "do not use it" or "rebuild
//! it", never an error. Results with and without it are identical (property-tested).

use crate::error::{Error, Result};
use crate::meta::write_cache_atomic;
use crate::repo::Repo;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use vcs_merge::{CommitGraph, CommitGraphFile, MergeError};
use vcs_objects::ObjectId;
use vcs_store::ObjectStore;

pub(crate) const CG_DIR: &str = "commit-graphs";

/// File name of a zone's graph: 32 hex digits of the blake3 hash of the zone id.
pub(crate) fn cg_name(zone: &str) -> String {
    blake3::hash(zone.as_bytes()).to_hex().as_str()[..32].to_owned()
}

impl Repo {
    /// Enable or disable the commit-graph cache (default on). With it off, every query walks
    /// the commit objects; results are the same either way.
    pub fn set_commit_graph(&mut self, on: bool) {
        self.use_commit_graph = on;
    }

    fn load_cg_file(&self, zone: &str) -> Option<CommitGraphFile> {
        let path = self.common.join(CG_DIR).join(cg_name(zone));
        // `VCS_CACHE_EAGER_VERIFY_MAX` (bytes) overrides the size up to which a graph is verified completely at open
        let eager = std::env::var("VCS_CACHE_EAGER_VERIFY_MAX")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(vcs_merge::EAGER_VERIFY_MAX);
        let g = CommitGraphFile::from_map_with(vcs_store::mapfile::MapFile::open(&path, true).ok()?, eager).ok()?;
        if g.upgraded() {
            // an old-format file was converted in memory: write it back as v2 so the next open is lazy
            let _ = write_cache_atomic(&path, &g.encode());
        }
        Some(g)
    }

    /// `(parents, commit time)` of the real commit, the loader for building and verifying.
    fn cg_load(&self, id: &ObjectId) -> std::result::Result<(Vec<ObjectId>, u64), MergeError> {
        match self.store.get(id)? {
            vcs_objects::Object::Commit(c) => Ok((c.parents, c.author.timestamp)),
            _ => Err(MergeError::Corrupt(format!("{id} is not a commit"))),
        }
    }

    /// The graph of the zone that `extra` (non-empty, all in one zone) belongs to, covering
    /// every branch head of that zone and each of `extra`, brought up to date (and rewritten
    /// on disk) if the file was missing, stale or corrupt. `None` when it cannot be used
    /// (disabled, no or mixed zones, or some history is missing or malformed); callers then
    /// walk objects, with the same result.
    pub(crate) fn commit_graph(&self, extra: &[ObjectId]) -> Option<CommitGraphFile> {
        if !self.use_commit_graph {
            return None;
        }
        let first = extra.first()?;
        let zone = self.commit_obj(first).ok()?.zone;
        for e in &extra[1..] {
            if self.commit_obj(e).ok()?.zone != zone {
                return None;
            }
        }
        let old = self.load_cg_file(&zone).unwrap_or_default();
        let mut tips: Vec<ObjectId> = self
            .refs()
            .ok()?
            .branches
            .iter()
            .filter(|((z, _), _)| *z == zone)
            .map(|(_, id)| *id)
            .collect();
        tips.extend_from_slice(extra);
        if !old.is_empty() && tips.iter().all(|t| old.contains(t)) {
            return Some(old); // nothing new to add: no copy, no rewrite
        }
        // A parent must live in its child's zone. Commits entering the graph are checked here
        // (against each other, and against parents already in the file) so that the graph never
        // hides the corruption error the object walk would raise.
        let expect: RefCell<BTreeMap<ObjectId, String>> = RefCell::new(BTreeMap::new());
        let g = old
            .extend(&tips, |id| {
                let c = match self.store.get(id)? {
                    vcs_objects::Object::Commit(c) => c,
                    _ => return Err(MergeError::Corrupt(format!("{id} is not a commit"))),
                };
                if let Some(z) = expect.borrow_mut().remove(id) {
                    if z != c.zone {
                        return Err(MergeError::Corrupt(format!("commit {id} has parent from different zone")));
                    }
                }
                for p in &c.parents {
                    if old.contains(p) {
                        if self.commit_obj(p).map_err(|e| MergeError::Corrupt(e.to_string()))?.zone != c.zone {
                            return Err(MergeError::Corrupt(format!("commit {id} has parent from different zone")));
                        }
                    } else {
                        expect.borrow_mut().insert(*p, c.zone.clone());
                    }
                }
                Ok((c.parents, c.author.timestamp))
            })
            .ok()?;
        if g.len() != old.len() {
            let dir = self.common.join(CG_DIR);
            let _ = fs::create_dir_all(&dir);
            let _ = write_cache_atomic(&dir.join(cg_name(&zone)), &g.encode()); // best effort: it is only a cache
        }
        Some(g)
    }

    /// Build or refresh the commit-graph file of every zone that has a branch head, so the first history walk
    /// after an import or a gc does not pay for it (100,000 commits: 13 s cold, 0.1 s warm). Best effort: a
    /// cache that cannot be written is simply rebuilt later.
    pub fn warm_commit_graph(&self) {
        if !self.use_commit_graph {
            return;
        }
        let Ok(refs) = self.refs() else { return };
        let mut done = std::collections::BTreeSet::new();
        for ((zone, _), head) in &refs.branches {
            if done.insert(zone.clone()) {
                let _ = self.commit_graph(&[*head]);
            }
        }
    }

    /// A lazily opened graph file met data that contradicts the format: delete it so the next use rebuilds it
    /// (`fsck` reports such a file; it cannot repair a cache it did not write).
    pub(crate) fn discard_bad_commit_graph(&self, any_commit: &ObjectId) {
        if let Ok(c) = self.commit_obj(any_commit) {
            let _ = fs::remove_file(self.common.join(CG_DIR).join(cg_name(&c.zone)));
        }
    }

    /// Best common ancestors of two commits (see `vcs_merge::merge_base`), via the
    /// commit-graph when available.
    pub fn merge_bases(&self, a: &ObjectId, b: &ObjectId) -> Result<Vec<ObjectId>> {
        let from_graph = self.commit_graph(&[*a, *b]).and_then(|g| {
            let r = g
                .merge_bases_indexed(a, b)
                .map_or_else(|| vcs_merge::merge_base(&g as &dyn CommitGraph, a, b), Ok);
            // a cache file that contradicts itself is not trusted: walk the objects instead
            if g.is_bad() {
                self.discard_bad_commit_graph(a);
                return None;
            }
            Some(r)
        });
        let r = from_graph.unwrap_or_else(|| vcs_merge::merge_base(&vcs_merge::StoreGraph(&self.store), a, b));
        r.map_err(|e| Error::Msg(format!("merge base: {e}")))
    }

    /// Check every file under `.vcs/commit-graphs` against the real commits; one message per problem.
    pub(crate) fn fsck_commit_graph(&self, problems: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(self.common.join(CG_DIR)) else {
            return; // no cache is fine
        };
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        files.sort();
        for path in files {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if name.contains('.') {
                continue; // temp file of a crashed cache write
            }
            if fs::metadata(&path).is_err() {
                continue;
            }
            // open lazily, then do the whole-file validation explicitly (this is the only place that pays for it)
            let opened = CommitGraphFile::open(&path).and_then(|g| g.verify_full().map(|()| g));
            match opened {
                Err(e) => problems.push(format!("{name}: {e} (it is only a cache and is rebuilt on next use)")),
                Ok(g) => {
                    problems.extend(g.verify_against(|id| self.cg_load(id)).into_iter().map(|m| format!("{name}: {m}")));
                    for id in g.iter_ids() {
                        if let Ok(c) = self.commit_obj(&id) {
                            if cg_name(&c.zone) != name {
                                problems.push(format!("commit-graph {name}: {id} belongs to another zone"));
                            }
                        }
                    }
                }
            }
        }
    }
}
