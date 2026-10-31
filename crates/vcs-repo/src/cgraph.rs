//! The commit-graph cache (`.vcs/commit-graph`), see `vcs_merge::CommitGraphFile`.
//!
//! It is a pure cache: read paths ask for it, and anything wrong with it (missing, truncated,
//! checksum or consistency failure, an unreadable commit) means "do not use it" or "rebuild
//! it", never an error. Results with and without it are identical (property-tested).

use crate::error::{Error, Result};
use crate::meta::write_atomic;
use crate::repo::Repo;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use vcs_merge::{CommitGraph, CommitGraphFile, MergeError};
use vcs_objects::ObjectId;
use vcs_store::ObjectStore;

pub(crate) const CG_FILE: &str = "commit-graph";

impl Repo {
    /// Enable or disable the commit-graph cache (default on). With it off, every query walks
    /// the commit objects; results are the same either way.
    pub fn set_commit_graph(&mut self, on: bool) {
        self.use_commit_graph = on;
    }

    fn load_cg_file(&self) -> Option<CommitGraphFile> {
        CommitGraphFile::decode(&fs::read(self.vcs.join(CG_FILE)).ok()?).ok()
    }

    /// `(parents, commit time)` of the real commit, the loader for building and verifying.
    fn cg_load(&self, id: &ObjectId) -> std::result::Result<(Vec<ObjectId>, u64), MergeError> {
        match self.store.get(id)? {
            vcs_objects::Object::Commit(c) => Ok((c.parents, c.author.timestamp)),
            _ => Err(MergeError::Corrupt(format!("{id} is not a commit"))),
        }
    }

    /// A valid graph that covers every branch head and each of `extra`, brought up to date
    /// (and rewritten on disk) if the file was missing, stale or corrupt. `None` when it cannot
    /// be used (disabled, or some history is missing or malformed); callers then walk objects.
    pub(crate) fn commit_graph(&self, extra: &[ObjectId]) -> Option<CommitGraphFile> {
        if !self.use_commit_graph {
            return None;
        }
        let old = self.load_cg_file().unwrap_or_default();
        let mut tips: Vec<ObjectId> = self.refs().ok()?.branches.values().copied().collect();
        tips.extend_from_slice(extra);
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
            let _ = write_atomic(&self.vcs.join(CG_FILE), &g.encode()); // best effort: it is only a cache
        }
        Some(g)
    }

    /// Best common ancestors of two commits (see `vcs_merge::merge_base`), via the
    /// commit-graph when available.
    pub fn merge_bases(&self, a: &ObjectId, b: &ObjectId) -> Result<Vec<ObjectId>> {
        let r = match self.commit_graph(&[*a, *b]) {
            Some(g) => vcs_merge::merge_base(&g as &dyn CommitGraph, a, b),
            None => vcs_merge::merge_base(&vcs_merge::StoreGraph(&self.store), a, b),
        };
        r.map_err(|e| Error::Msg(format!("merge base: {e}")))
    }

    /// Check `.vcs/commit-graph` (if present) against the real commits; one message per problem.
    pub(crate) fn fsck_commit_graph(&self, problems: &mut Vec<String>) {
        let bytes = match fs::read(self.vcs.join(CG_FILE)) {
            Ok(b) => b,
            Err(_) => return, // no cache is fine
        };
        match CommitGraphFile::decode(&bytes) {
            Err(e) => problems.push(format!("{e} (it is only a cache and is rebuilt on next use)")),
            Ok(g) => problems.extend(g.verify_against(|id| self.cg_load(id))),
        }
    }
}
