//! Recursive merge bases: a virtual base tree for criss-cross histories.
//!
//! Two commits whose history crosses over (each merged the other's earlier tip) have more than
//! one best common ancestor. Git's `recursive`/`ort` strategies merge those ancestors with each
//! other first and use the result as the base of the real merge; so do we. The merge of the
//! ancestors is a *virtual commit*: only its tree exists (it is written to the object store so the
//! merge engine can read it, and is garbage once nothing refers to it). Conflicts between the
//! ancestors are not an error there: the conflict-marker text is taken as the content, exactly as
//! git does, so the real merge sees it as the common base.
//!
//! A virtual commit has the commits it merged as parents, so its ancestors are the union of theirs.
//! That is why the helpers here work on a *member set* of real commits instead of a made-up commit
//! id: the merge bases of the virtual commit and `x` are the best of the merge bases of each
//! member and `x`, which the commit-graph answers quickly.

use crate::error::{msg, Result};
use crate::repo::Repo;
use crate::treemerge::Merge3;
use std::collections::BTreeSet;
use vcs_merge::{ConflictKind, MarkerLabels};
use vcs_objects::ObjectId;

/// How many levels of criss-cross inside criss-cross are followed before giving up (git: 1000
/// "recursion" levels in practice, never reached; this bound only keeps a corrupt graph finite).
const MAX_DEPTH: usize = 64;

impl Repo {
    /// Best common ancestors of the virtual commit whose parents are `members` and of `other`:
    /// the best of every member's merge bases with `other`, without any that is an ancestor of
    /// another.
    pub(crate) fn bases_of_set(&self, members: &[ObjectId], other: &ObjectId) -> Result<Vec<ObjectId>> {
        if let [m] = members {
            return self.merge_bases(m, other);
        }
        let mut all = BTreeSet::new();
        for m in members {
            all.extend(self.merge_bases(m, other)?);
        }
        let all: Vec<ObjectId> = all.into_iter().collect();
        let mut out = Vec::new();
        for x in &all {
            let mut dominated = false;
            for y in &all {
                if x != y && self.merge_bases(x, y)?.as_slice() == [*x] {
                    dominated = true;
                    break;
                }
            }
            if !dominated {
                out.push(*x);
            }
        }
        Ok(out)
    }

    /// The root tree to use as the merge base for `bases` (best common ancestors): `None` (the
    /// empty tree) when there is none, the commit's own tree when there is one, the virtual
    /// merge of all of them when there are several.
    pub(crate) fn base_root_for(&self, prefix: &str, bases: &[ObjectId]) -> Result<Option<ObjectId>> {
        self.base_root_depth(prefix, bases, 0)
    }

    fn base_root_depth(&self, prefix: &str, bases: &[ObjectId], depth: usize) -> Result<Option<ObjectId>> {
        match bases {
            [] => Ok(None),
            [b] => Ok(Some(self.commit_obj(b)?.root)),
            many => Ok(Some(self.virtual_base_root(prefix, many, depth)?)),
        }
    }

    /// Merge `bases` (two or more, none an ancestor of another) into one virtual tree.
    fn virtual_base_root(&self, prefix: &str, bases: &[ObjectId], depth: usize) -> Result<ObjectId> {
        if depth >= MAX_DEPTH {
            return msg("criss-cross history is nested too deeply to build a merge base");
        }
        let mut acc = self.commit_obj(&bases[0])?.root;
        let mut members = vec![bases[0]];
        for b in &bases[1..] {
            let inner = self.bases_of_set(&members, b)?;
            let base_root = self.base_root_depth(prefix, &inner, depth + 1)?;
            let theirs = self.commit_obj(b)?.root;
            let mut tm = self.merge3_trees(prefix, base_root, Some(acc), Some(theirs))?;
            acc = self.settle_virtual(&mut tm, acc)?;
            members.push(*b);
        }
        Ok(acc)
    }

    /// Turn the result of merging two ancestors into a tree: conflicts keep their marker text.
    fn settle_virtual(&self, tm: &mut Merge3, ours: ObjectId) -> Result<ObjectId> {
        if let Some(c) = tm.conflicts.iter().find(|c| c.kind == ConflictKind::FileDirectory) {
            return msg(format!(
                "criss-cross history: the common ancestors disagree about {:?} being a file or a directory; file/directory conflicts are not supported yet (nothing was changed)",
                c.path
            ));
        }
        for (id, data) in &tm.new_blobs {
            if self.store.put_content(data)? != *id {
                return Err(crate::error::Error::Corrupt("merged blob id mismatch".into()));
            }
        }
        let labels = MarkerLabels {
            left: "merged common ancestor",
            base: "base",
            right: "merged common ancestor",
        };
        let conflicts = std::mem::take(&mut tm.conflicts);
        for c in &conflicts {
            let (mode, id, _) = self.placeholder(c, &labels)?;
            tm.set_placeholder(&c.path, (mode, id));
        }
        Ok(tm.apply(&self.store, Some(ours), true)?.0)
    }

    /// Merge `theirs` into the virtual commit made of `members` whose tree is `ours_root`,
    /// against their (possibly virtual) common base. Nothing is written except the virtual base.
    pub(crate) fn merge_into_set(&self, prefix: &str, members: &[ObjectId], ours_root: ObjectId, theirs: &ObjectId) -> Result<Merge3> {
        let bases = self.bases_of_set(members, theirs)?;
        let base_root = self.base_root_for(prefix, &bases)?;
        let their_root = self.commit_obj(theirs)?.root;
        self.merge3_trees(prefix, base_root, Some(ours_root), Some(their_root))
    }
}
