//! Merge-base over a commit DAG.

use crate::error::MergeError;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use vcs_objects::{Object, ObjectId};
use vcs_store::ObjectStore;

/// Read-only view of the commit DAG.
pub trait CommitGraph {
    fn parents(&self, id: &ObjectId) -> Result<Vec<ObjectId>, MergeError>;
}

/// In-memory graph (tests, tools).
#[derive(Default, Clone, Debug)]
pub struct MemGraph(pub BTreeMap<ObjectId, Vec<ObjectId>>);

impl MemGraph {
    pub fn add(&mut self, id: ObjectId, parents: Vec<ObjectId>) {
        self.0.insert(id, parents);
    }
}

impl CommitGraph for MemGraph {
    fn parents(&self, id: &ObjectId) -> Result<Vec<ObjectId>, MergeError> {
        self.0.get(id).cloned().ok_or(MergeError::Missing(*id))
    }
}

/// Commit graph backed by an object store.
pub struct StoreGraph<'a>(pub &'a dyn ObjectStore);

impl CommitGraph for StoreGraph<'_> {
    fn parents(&self, id: &ObjectId) -> Result<Vec<ObjectId>, MergeError> {
        match self.0.get(id)? {
            Object::Commit(c) => Ok(c.parents),
            _ => Err(MergeError::Corrupt(format!("{id} is not a commit"))),
        }
    }
}

/// All ancestors of `start`, including `start`.
fn ancestors(g: &dyn CommitGraph, start: &ObjectId) -> Result<BTreeSet<ObjectId>, MergeError> {
    let mut seen = BTreeSet::new();
    let mut q = VecDeque::from([*start]);
    while let Some(c) = q.pop_front() {
        if seen.insert(c) {
            q.extend(g.parents(&c)?);
        }
    }
    Ok(seen)
}

pub fn is_ancestor(g: &dyn CommitGraph, anc: &ObjectId, desc: &ObjectId) -> Result<bool, MergeError> {
    Ok(ancestors(g, desc)?.contains(anc))
}

/// Best common ancestors of `a` and `b`: common ancestors that are not a strict ancestor of
/// another common ancestor. Sorted by id. Empty = unrelated histories; more than one =
/// criss-cross (caller may merge the bases recursively). If one side contains the other,
/// the result is that side's commit.
///
/// Cost: O(commits reachable from a and b); no generation numbers yet (M5).
pub fn merge_base(g: &dyn CommitGraph, a: &ObjectId, b: &ObjectId) -> Result<Vec<ObjectId>, MergeError> {
    let aa = ancestors(g, a)?;
    let ab = ancestors(g, b)?;
    let common: BTreeSet<ObjectId> = aa.intersection(&ab).copied().collect();
    let mut dominated = BTreeSet::new();
    let mut q = VecDeque::new();
    for c in &common {
        q.extend(g.parents(c)?);
    }
    while let Some(c) = q.pop_front() {
        if dominated.insert(c) {
            q.extend(g.parents(&c)?);
        }
    }
    Ok(common.into_iter().filter(|c| !dominated.contains(c)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(n: u8) -> ObjectId {
        ObjectId::from_bytes([n; 32])
    }
    fn graph(edges: &[(u8, &[u8])]) -> MemGraph {
        let mut g = MemGraph::default();
        for (c, ps) in edges {
            g.add(id(*c), ps.iter().map(|p| id(*p)).collect());
        }
        g
    }

    #[test]
    fn linear_and_fork() {
        let g = graph(&[(1, &[]), (2, &[1]), (3, &[2]), (4, &[2])]);
        assert_eq!(merge_base(&g, &id(3), &id(4)).unwrap(), vec![id(2)]);
        assert_eq!(merge_base(&g, &id(3), &id(1)).unwrap(), vec![id(1)]);
        assert_eq!(merge_base(&g, &id(3), &id(3)).unwrap(), vec![id(3)]);
        assert!(is_ancestor(&g, &id(1), &id(3)).unwrap());
        assert!(!is_ancestor(&g, &id(4), &id(3)).unwrap());
    }

    #[test]
    fn unrelated_histories() {
        let g = graph(&[(1, &[]), (2, &[]), (3, &[1]), (4, &[2])]);
        assert!(merge_base(&g, &id(3), &id(4)).unwrap().is_empty());
    }

    #[test]
    fn criss_cross_returns_all_best() {
        let g = graph(&[(1, &[]), (2, &[1]), (3, &[1]), (4, &[2, 3]), (5, &[3, 2])]);
        assert_eq!(merge_base(&g, &id(4), &id(5)).unwrap(), vec![id(2), id(3)]);
    }

    #[test]
    fn merge_commit_dominates_ancestors() {
        let g = graph(&[(1, &[]), (2, &[1]), (3, &[1]), (4, &[2, 3]), (5, &[4]), (6, &[3])]);
        assert_eq!(merge_base(&g, &id(5), &id(6)).unwrap(), vec![id(3)]);
    }

    #[test]
    fn missing_commit_is_error() {
        let g = graph(&[(1, &[2])]);
        assert!(matches!(merge_base(&g, &id(1), &id(1)), Err(MergeError::Missing(_))));
    }
}
