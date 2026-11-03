//! Merge-base over a commit DAG.

use crate::error::MergeError;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};
use vcs_objects::{Object, ObjectId};
use vcs_store::ObjectStore;

/// Read-only view of the commit DAG.
pub trait CommitGraph {
    fn parents(&self, id: &ObjectId) -> Result<Vec<ObjectId>, MergeError>;

    /// Generation number of `id` (1 + the maximum over its parents, 1 for a root), when this
    /// graph knows it. An implementation that returns `Some` for a commit must return `Some`
    /// for all of its ancestors, with exactly that definition. The default knows nothing.
    fn generation(&self, _id: &ObjectId) -> Option<u32> {
        None
    }
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
pub(crate) fn ancestors(g: &dyn CommitGraph, start: &ObjectId) -> Result<BTreeSet<ObjectId>, MergeError> {
    let mut seen = BTreeSet::new();
    let mut q = VecDeque::from([*start]);
    while let Some(c) = q.pop_front() {
        if seen.insert(c) {
            q.extend(g.parents(&c)?);
        }
    }
    Ok(seen)
}

/// Is `anc` reachable from `desc` (or equal)? Uses generation numbers to prune when the graph
/// has them; the answer is the same either way.
pub fn is_ancestor(g: &dyn CommitGraph, anc: &ObjectId, desc: &ObjectId) -> Result<bool, MergeError> {
    match (g.generation(anc), g.generation(desc)) {
        (Some(ga), Some(gd)) => {
            if ga > gd {
                return Ok(false);
            }
            let mut seen = BTreeSet::new();
            let mut stack = vec![*desc];
            while let Some(c) = stack.pop() {
                if &c == anc {
                    return Ok(true);
                }
                if !seen.insert(c) || g.generation(&c).is_none_or(|x| x <= ga) {
                    continue; // at or below anc's generation and not anc itself: cannot lead to it
                }
                stack.extend(g.parents(&c)?);
            }
            Ok(false)
        }
        _ => Ok(ancestors(g, desc)?.contains(anc)),
    }
}

/// Best common ancestors of `a` and `b`: common ancestors that are not a strict ancestor of
/// another common ancestor. Sorted by id. Empty = unrelated histories; more than one =
/// criss-cross (caller may merge the bases recursively). If one side contains the other,
/// the result is that side's commit.
///
/// Cost: O(commits reachable from a and b), or, when the graph has generation numbers, only
/// the commits newer than the merge bases. The result is identical either way.
pub fn merge_base(g: &dyn CommitGraph, a: &ObjectId, b: &ObjectId) -> Result<Vec<ObjectId>, MergeError> {
    if g.generation(a).is_some() && g.generation(b).is_some() {
        return merge_base_gen(g, a, b);
    }
    merge_base_plain(g, a, b)
}

const P1: u8 = 1;
const P2: u8 = 2;
const STALE: u8 = 4;

/// Paint down from both tips in decreasing generation order (as git does), then drop
/// candidates that are ancestors of other candidates.
fn merge_base_gen(g: &dyn CommitGraph, a: &ObjectId, b: &ObjectId) -> Result<Vec<ObjectId>, MergeError> {
    let gen = |id: &ObjectId| g.generation(id).ok_or(MergeError::Missing(*id));
    let mut flags: BTreeMap<ObjectId, u8> = BTreeMap::new();
    let mut heap: BinaryHeap<(u32, ObjectId)> = BinaryHeap::new();
    *flags.entry(*a).or_default() |= P1;
    *flags.entry(*b).or_default() |= P2;
    heap.push((gen(a)?, *a));
    if a != b {
        heap.push((gen(b)?, *b));
    }
    let mut done = BTreeSet::new();
    let mut results = Vec::new();
    while heap.iter().any(|(_, n)| flags[n] & STALE == 0) {
        let (_, n) = heap.pop().expect("non-empty");
        if !done.insert(n) {
            continue;
        }
        let mut f = flags[&n];
        if f & (P1 | P2 | STALE) == (P1 | P2) {
            results.push(n);
            f |= STALE;
            flags.insert(n, f);
        }
        for p in g.parents(&n)? {
            let e = flags.entry(p).or_default();
            let new = *e | (f & (P1 | P2 | STALE));
            if new != *e {
                *e = new;
                if !done.contains(&p) {
                    heap.push((gen(&p)?, p));
                }
            }
        }
    }
    let mut out = Vec::new();
    for r in &results {
        let mut redundant = false;
        for s in &results {
            if s != r && is_ancestor(g, r, s)? {
                redundant = true;
                break;
            }
        }
        if !redundant {
            out.push(*r);
        }
    }
    out.sort();
    Ok(out)
}

pub(crate) fn merge_base_plain(g: &dyn CommitGraph, a: &ObjectId, b: &ObjectId) -> Result<Vec<ObjectId>, MergeError> {
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
