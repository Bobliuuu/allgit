//! Revision ranges: `A..B`, `A...B`, `^A B`, `--not`, and "every ref of a zone" (`log --all`).
//!
//! A range is a set of commits, `include` minus everything reachable from `exclude`, listed in
//! `log` order (author time, newest first, ties by larger id). With the commit-graph cache the
//! set is found by a walk in decreasing generation number that stops when nothing that could
//! still belong to the answer is waiting (`CommitGraphFile::range_ids`); without the cache the
//! same set is computed from the commit objects. Both give identical results (tested).
//!
//! Semantics per zone: a commit belongs to exactly one zone and all of its parents to the same
//! zone, so a range is always inside one zone. Every revision in it is resolved in that zone
//! (`HEAD`, branch, tag, commit-id prefix, with `~N`/`^`); a revision of another zone is an error.
//! "All refs" of a zone are its branches, its tags (peeled to commits) and, when the zone is on a
//! detached HEAD, that commit. Remote-tracking branches are not included.

use crate::error::{msg, Error, Result};
use crate::repo::Repo;
use std::collections::{BinaryHeap, HashSet};
use vcs_objects::names::DETACHED_BRANCH;
use vcs_objects::{Commit, ObjectId};

/// A parsed revision list, git style. See [`RevSpec::parse`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RevSpec {
    /// Revisions whose history is wanted.
    pub include: Vec<String>,
    /// Revisions whose history is removed (`^A`, `--not A`, the left side of `A..B`).
    pub exclude: Vec<String>,
    /// `A...B`: commits reachable from either but not from both (their merge bases are excluded).
    pub sym: Vec<(String, String)>,
    /// Also include every branch, tag and a detached HEAD of the zone.
    pub all: bool,
}

impl RevSpec {
    /// Parse git revision arguments: `A`, `^A`, `A..B`, `A...B`, `..B`, `A..` (an empty side is
    /// `HEAD`), and the word `--not`, which flips the meaning of the revisions after it. An empty
    /// list means `HEAD`.
    pub fn parse<S: AsRef<str>>(tokens: &[S]) -> Result<RevSpec> {
        let mut s = RevSpec::default();
        let mut not = false;
        for t in tokens {
            let t = t.as_ref();
            if t == "--not" {
                not = !not;
                continue;
            }
            let head = |x: &str| if x.is_empty() { "HEAD".to_owned() } else { x.to_owned() };
            if let Some((a, b)) = t.split_once("...") {
                if not {
                    return msg("--not cannot be combined with a symmetric range (A...B)");
                }
                s.sym.push((head(a), head(b)));
            } else if let Some((a, b)) = t.split_once("..") {
                if not {
                    return msg("--not cannot be combined with a range (A..B)");
                }
                s.exclude.push(head(a));
                s.include.push(head(b));
            } else if let Some(r) = t.strip_prefix('^') {
                if r.is_empty() {
                    return msg("empty revision after '^'");
                }
                if not {
                    s.include.push(r.to_owned());
                } else {
                    s.exclude.push(r.to_owned());
                }
            } else if not {
                s.exclude.push(t.to_owned());
            } else {
                s.include.push(t.to_owned());
            }
        }
        Ok(s)
    }

    /// True for a spec that is at most one plain revision (no range, no exclusion, no `--all`):
    /// the case `log` answers with its ordinary branch walk.
    pub fn is_plain(&self) -> bool {
        self.exclude.is_empty() && self.sym.is_empty() && !self.all && self.include.len() <= 1
    }

    /// Is `token` a range-ish argument (`A..B`, `A...B`, `^A`)?
    pub fn is_range_token(token: &str) -> bool {
        token.contains("..") || (token.starts_with('^') && token.len() > 1)
    }
}

impl Repo {
    /// Every branch head, every tag (peeled to its commit) and the detached HEAD (when `zone` is
    /// on one) of `zone`, without duplicates.
    pub fn zone_tips(&self, zone: &str) -> Result<Vec<ObjectId>> {
        let zone = self.resolve_zone(zone)?;
        let refs = self.refs()?;
        let mut v: Vec<ObjectId> = refs.branches_of(&zone).into_iter().map(|(_, h)| h).collect();
        for ((a, _), id) in &refs.tags {
            if *a == zone {
                v.push(self.peel(&zone, *id)?.0);
            }
        }
        v.extend(self.detached_head(&zone)?);
        let mut seen = HashSet::new();
        v.retain(|i| seen.insert(*i));
        Ok(v)
    }

    /// The commits of `spec` in `zone`, newest first, at most `limit`.
    pub fn rev_list(&self, zone: &str, spec: &RevSpec, limit: usize) -> Result<Vec<(ObjectId, Commit)>> {
        let ids = self.rev_list_ids(zone, spec, limit)?;
        self.load_commits(ids)
    }

    /// Like [`Repo::rev_list`] but only the ids (no commit is read when the commit-graph has them).
    pub fn rev_list_ids(&self, zone: &str, spec: &RevSpec, limit: usize) -> Result<Vec<ObjectId>> {
        let zone = self.resolve_zone(zone)?;
        let mut include = Vec::new();
        let mut exclude = Vec::new();
        for r in &spec.include {
            include.push(self.resolve_rev(&zone, r)?);
        }
        for r in &spec.exclude {
            exclude.push(self.resolve_rev(&zone, r)?);
        }
        for (a, b) in &spec.sym {
            let (ia, ib) = (self.resolve_rev(&zone, a)?, self.resolve_rev(&zone, b)?);
            include.push(ia);
            include.push(ib);
            exclude.extend(self.merge_bases(&ia, &ib)?);
        }
        if spec.all {
            include.extend(self.zone_tips(&zone)?);
        }
        if spec.include.is_empty() && spec.exclude.is_empty() && spec.sym.is_empty() && !spec.all {
            include.push(self.resolve_rev(&zone, "HEAD")?);
        }
        self.range_ids_ordered(&include, &exclude, limit)
    }

    /// `include` minus `exclude` (all commits of one zone) in `log` order, at most `limit`.
    pub fn range_ids_ordered(&self, include: &[ObjectId], exclude: &[ObjectId], limit: usize) -> Result<Vec<ObjectId>> {
        let mut inc: Vec<ObjectId> = include.to_vec();
        inc.sort();
        inc.dedup();
        if inc.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let all: Vec<ObjectId> = inc.iter().chain(exclude).copied().collect();
        if let Some(g) = self.commit_graph(&all) {
            if let Some(members) = g.range_ids(&inc, exclude) {
                let set: HashSet<ObjectId> = members.iter().copied().collect();
                let mut heap: BinaryHeap<(u64, ObjectId)> = BinaryHeap::new();
                for i in &inc {
                    if set.contains(i) {
                        heap.push((g.time(i).unwrap_or(0), *i));
                    }
                }
                let mut seen: HashSet<ObjectId> = HashSet::with_capacity(members.len());
                let mut out = Vec::with_capacity(members.len().min(limit));
                while let Some((_, id)) = heap.pop() {
                    if out.len() >= limit {
                        break;
                    }
                    if !seen.insert(id) {
                        continue;
                    }
                    for p in vcs_merge::CommitGraph::parents(&g, &id).map_err(|e| Error::Msg(e.to_string()))? {
                        if set.contains(&p) && !seen.contains(&p) {
                            heap.push((g.time(&p).unwrap_or(0), p));
                        }
                    }
                    out.push(id);
                }
                return Ok(out);
            }
        }
        self.range_ids_walk(&inc, exclude, limit)
    }

    /// The same answer from commit objects only (no commit-graph); also the reference the
    /// graph version is tested against.
    pub fn range_ids_walk(&self, include: &[ObjectId], exclude: &[ObjectId], limit: usize) -> Result<Vec<ObjectId>> {
        let pi = self.parent_index(&[]);
        let mut hidden: HashSet<ObjectId> = HashSet::new();
        for e in exclude {
            let mut todo = vec![*e];
            while let Some(id) = todo.pop() {
                if hidden.insert(id) {
                    todo.extend(pi.parents(&id)?);
                }
            }
        }
        let mut heap: BinaryHeap<(u64, ObjectId)> = BinaryHeap::new();
        let mut queued: HashSet<ObjectId> = HashSet::new();
        for i in include {
            if !hidden.contains(i) && queued.insert(*i) {
                heap.push((self.commit_obj(i)?.author.timestamp, *i));
            }
        }
        let mut out = Vec::new();
        while let Some((_, id)) = heap.pop() {
            if out.len() >= limit {
                break;
            }
            for p in pi.parents(&id)? {
                if !hidden.contains(&p) && queued.insert(p) {
                    heap.push((self.commit_obj(&p)?.author.timestamp, p));
                }
            }
            out.push(id);
        }
        Ok(out)
    }

    /// Commits that were only reachable from the detached HEAD `d` of `zone`, i.e. would be lost
    /// from every branch and tag when leaving it (newest first).
    pub fn commits_only_in(&self, zone: &str, d: ObjectId) -> Result<Vec<ObjectId>> {
        let zone = self.resolve_zone(zone)?;
        let refs = self.refs()?;
        let mut tips: Vec<ObjectId> = refs.branches_of(&zone).into_iter().map(|(_, h)| h).collect();
        for ((a, _), id) in &refs.tags {
            if *a == zone {
                tips.push(self.peel(&zone, *id)?.0);
            }
        }
        self.range_ids_ordered(&[d], &tips, usize::MAX)
    }

    /// Is `name` the internal detached-HEAD ref name?
    pub fn is_detached_name(name: &str) -> bool {
        name == DETACHED_BRANCH
    }
}
