//! History walks that stop early and read as little of each tree as possible:
//! a streaming `log` walker and a path filter that answers "does this commit change one of these
//! paths?" by comparing the ids of the entries at those paths (a directory whose id is equal on
//! both sides cannot hold a change), instead of flattening the whole tree of every commit.
//!
//! Everything here returns exactly what the whole-tree versions returned (tested against them).

use crate::error::{msg, Error, Result};
use crate::pathbloom::{may_contain, Filter, PathBlooms};
use crate::repo::Repo;
use crate::treeops;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashSet};
use vcs_objects::{Commit, EntryMode, ObjectId};

/// Newest-first walk over the history of a branch (by author time, ties by id), one commit at a
/// time, so a caller that needs only the first few matches reads only the commits it passes.
pub struct LogWalk<'a> {
    repo: &'a Repo,
    seen: BTreeSet<ObjectId>,
    heap: BinaryHeap<(u64, ObjectId)>,
    cache: BTreeMap<ObjectId, Commit>,
    failed: bool,
}

impl Iterator for LogWalk<'_> {
    type Item = Result<(ObjectId, Commit)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let r = self.step();
        match r {
            Ok(x) => x.map(Ok),
            Err(e) => {
                self.failed = true;
                Some(Err(e))
            }
        }
    }
}

impl LogWalk<'_> {
    fn step(&mut self) -> Result<Option<(ObjectId, Commit)>> {
        while let Some((_, id)) = self.heap.pop() {
            if !self.seen.insert(id) {
                continue;
            }
            let c = self.cache.remove(&id).expect("cached");
            for p in &c.parents {
                if !self.seen.contains(p) && !self.cache.contains_key(p) {
                    let pc = self.repo.commit_obj(p)?;
                    if pc.zone != c.zone {
                        return Err(Error::Corrupt(format!("commit {} has parent from different zone", id)));
                    }
                    self.heap.push((pc.author.timestamp, *p));
                    self.cache.insert(*p, pc);
                }
            }
            return Ok(Some((id, c)));
        }
        Ok(None)
    }
}

impl Repo {
    /// The zone id and head commit that `log` and friends start from.
    fn log_head(&self, zone: &str, branch: Option<&str>) -> Result<(String, Option<ObjectId>)> {
        let zone = self.resolve_zone(zone)?;
        if !self.zones()?.exists(&zone) {
            return msg(format!("no such zone {zone:?}"));
        }
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let head = match branch {
            Some(b) => refs
                .head(&zone, b)
                .ok_or_else(|| Error::Msg(format!("no such branch {zone}/{b}")))?
                .into(),
            None => self.current_head(&zone, &ws, &refs),
        };
        Ok((zone, head))
    }

    /// Like [`Repo::log`] but lazy: the same commits in the same order, read as you ask for them.
    pub fn log_walk(&self, zone: &str, branch: Option<&str>) -> Result<LogWalk<'_>> {
        let (_, head) = self.log_head(zone, branch)?;
        let mut w = LogWalk {
            repo: self,
            seen: BTreeSet::new(),
            heap: BinaryHeap::new(),
            cache: BTreeMap::new(),
            failed: false,
        };
        if let Some(h) = head {
            let c = self.commit_obj(&h)?;
            w.heap.push((c.author.timestamp, h));
            w.cache.insert(h, c);
        }
        Ok(w)
    }

    /// The first `limit` commits of the history of `head` in `log` order (newest author time
    /// first, ties by larger id), from the commit-graph without reading any commit object.
    fn graph_order(g: &vcs_merge::CommitGraphFile, head: ObjectId, limit: usize) -> Option<Vec<ObjectId>> {
        let mut seen = HashSet::new();
        let mut heap: BinaryHeap<(u64, ObjectId)> = BinaryHeap::new();
        heap.push((g.time(&head)?, head));
        let mut out = Vec::new();
        while let Some((_, id)) = heap.pop() {
            if out.len() >= limit {
                break;
            }
            if !seen.insert(id) {
                continue;
            }
            for p in vcs_merge::CommitGraph::parents(g, &id).ok()? {
                if !seen.contains(&p) {
                    heap.push((g.time(&p)?, p));
                }
            }
            out.push(id);
        }
        Some(out)
    }

    /// Load commits, on several threads when there are many.
    fn load_commits(&self, ids: Vec<ObjectId>) -> Result<Vec<(ObjectId, Commit)>> {
        if ids.len() < 256 {
            return ids.into_iter().map(|i| Ok((i, self.commit_obj(&i)?))).collect();
        }
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
        let per = ids.len().div_ceil(threads);
        let parts: Vec<Result<Vec<(ObjectId, Commit)>>> = std::thread::scope(|s| {
            let hs: Vec<_> = ids
                .chunks(per)
                .map(|ch| s.spawn(move || ch.iter().map(|i| Ok((*i, self.commit_obj(i)?))).collect()))
                .collect();
            hs.into_iter().map(|h| h.join().expect("commit loader panicked")).collect()
        });
        let mut out = Vec::with_capacity(ids.len());
        for p in parts {
            out.extend(p?);
        }
        Ok(out)
    }

    /// History of the branch checked out for `zone` (or `branch`), newest first (by author time,
    /// ties by id), at most `limit` commits. A long history is ordered from the commit-graph
    /// cache and its commits are read on all cores; the result is the same as walking the objects.
    pub fn log(&self, zone: &str, branch: Option<&str>, limit: usize) -> Result<Vec<(ObjectId, Commit)>> {
        if limit >= GRAPH_LOG_MIN {
            let (_, head) = self.log_head(zone, branch)?;
            if let Some(h) = head {
                if let Some(g) = self.commit_graph(&[h]) {
                    if let Some(order) = Self::graph_order(&g, h, limit) {
                        return self.load_commits(order);
                    }
                }
            }
        }
        self.log_walk(zone, branch)?.take(limit).collect()
    }

    /// The commits (newest first, at most `limit`) that `author` accepts and that change one of
    /// `specs` (repo-relative paths; empty = any), exactly what filtering [`Repo::log_walk`] with
    /// [`PathFilter`] gives, but with the commit-graph and changed-path filters a commit that
    /// provably changes none of the paths is passed without being read. The flag is true when the
    /// branch has no commits at all.
    pub fn log_paths(
        &self,
        zone: &str,
        branch: Option<&str>,
        specs: &[String],
        author: Option<&dyn Fn(&Commit) -> bool>,
        limit: usize,
    ) -> Result<(Vec<(ObjectId, Commit)>, bool)> {
        let (zid, head) = self.log_head(zone, branch)?;
        let Some(h) = head else { return Ok((Vec::new(), true)) };
        let mut pf = PathFilter::new(self, &zid, specs)?;
        let mut shown = Vec::new();
        let ok = |pf: &mut PathFilter, id: &ObjectId, c: &Commit| -> Result<bool> {
            Ok(author.is_none_or(|f| f(c)) && (specs.is_empty() || pf.touches(id, c)?))
        };
        let graph = if specs.is_empty() { None } else { self.commit_graph(&[h]) };
        if let Some(g) = graph {
            pf.preload_filters();
            let mut seen = HashSet::new();
            let mut heap: BinaryHeap<(u64, ObjectId)> = BinaryHeap::new();
            heap.push((g.time(&h).unwrap_or(0), h));
            while let Some((_, id)) = heap.pop() {
                if shown.len() >= limit {
                    break;
                }
                if !seen.insert(id) {
                    continue;
                }
                let parents = vcs_merge::CommitGraph::parents(&g, &id).map_err(|e| Error::Msg(e.to_string()))?;
                for p in &parents {
                    if !seen.contains(p) {
                        heap.push((g.time(p).unwrap_or(0), *p));
                    }
                }
                if pf.quick_miss(&id, parents.len()) {
                    continue;
                }
                let c = self.commit_obj(&id)?;
                if ok(&mut pf, &id, &c)? {
                    shown.push((id, c));
                }
            }
            return Ok((shown, false));
        }
        for r in self.log_walk(zone, branch)? {
            let (id, c) = r?;
            if shown.len() >= limit {
                break;
            }
            if ok(&mut pf, &id, &c)? {
                shown.push((id, c));
            }
        }
        Ok((shown, false))
    }
}

/// `log` with at least this many commits wanted goes through the commit-graph.
const GRAPH_LOG_MIN: usize = 2048;

type Entry = Option<(EntryMode, ObjectId)>;

/// A walk that has passed this many commits starts using (and building) the changed-path filters.
const BLOOM_AFTER: usize = 256;
/// Persist newly computed filters once at least this many are pending (a file is rewritten whole,
/// so an existing one is touched only for a good number of new commits).
const BLOOM_FLUSH: usize = 64;
/// ... and when there is no file yet.
const BLOOM_FLUSH_NEW: usize = 16;

/// "Does this commit change any of these paths?" (git's pathspec-limited history).
///
/// A commit touches the paths when the entries at them differ from those of every parent (a
/// root commit touches them if it has any file there). Paths are repo-relative. After a few
/// hundred commits the per-zone changed-path filters (see `pathbloom`) are consulted: when they
/// say a parent's diff cannot involve any of the paths, the trees are not read at all.
pub struct PathFilter<'a> {
    repo: &'a Repo,
    zone: String,
    /// Zone-relative specs; `""` is the whole zone.
    specs: Vec<String>,
    /// Entries at the specs, per tree root already looked at.
    cache: BTreeMap<ObjectId, Vec<Entry>>,
    asked: usize,
    bloom_after: usize,
    first_parent_only: bool,
    blooms: Option<PathBlooms>,
}

/// `spec` (repo-relative) as a path inside the zone at `prefix`, or `None` if it cannot match.
fn zone_relative(prefix: &str, spec: &str) -> Option<String> {
    if spec.is_empty() || prefix.is_empty() {
        return Some(spec.to_owned());
    }
    if spec == prefix {
        return Some(String::new());
    }
    if let Some(r) = spec.strip_prefix(prefix).and_then(|r| r.strip_prefix('/')) {
        return Some(r.to_owned());
    }
    // the spec names a directory above the zone: everything in the zone is below it
    prefix.strip_prefix(spec).is_some_and(|r| r.starts_with('/')).then(String::new)
}

impl<'a> PathFilter<'a> {
    /// `zone` is the zone (id, name or path) the commits belong to.
    pub fn new(repo: &'a Repo, zone: &str, specs: &[String]) -> Result<PathFilter<'a>> {
        let zone = repo.resolve_zone(zone)?;
        let prefix = repo.zones()?.path_of(&zone).unwrap_or("").to_owned();
        let mut s: Vec<String> = specs.iter().filter_map(|p| zone_relative(&prefix, p)).collect();
        s.sort();
        s.dedup();
        // a spec below another one adds nothing
        let all = s.clone();
        s.retain(|p| {
            !all.iter()
                .any(|q| q != p && (q.is_empty() || p.strip_prefix(q.as_str()).is_some_and(|r| r.starts_with('/'))))
        });
        Ok(PathFilter {
            repo,
            zone,
            specs: s,
            cache: BTreeMap::new(),
            asked: 0,
            bloom_after: BLOOM_AFTER,
            first_parent_only: false,
            blooms: None,
        })
    }

    /// Compare with the first parent only (`--first-parent`): a merge that equals its first parent
    /// at the paths does not touch them, whatever the other parents hold.
    pub fn first_parent_only(mut self, on: bool) -> Self {
        self.first_parent_only = on;
        self
    }

    /// Consult the changed-path filters after `n` commits instead of the default (tests use 0).
    pub fn use_filters_after(mut self, n: usize) -> Self {
        self.bloom_after = n;
        self
    }

    fn entries(&mut self, root: ObjectId) -> Result<Vec<Entry>> {
        if let Some(v) = self.cache.get(&root) {
            return Ok(v.clone());
        }
        let v = self
            .specs
            .iter()
            .map(|s| treeops::entry_at(self.repo.store(), root, s))
            .collect::<Result<Vec<_>>>()?;
        if self.cache.len() > 4096 {
            self.cache.clear();
        }
        self.cache.insert(root, v.clone());
        Ok(v)
    }

    /// The changed-path filters of commit `id` (one per parent): stored ones, or computed (and remembered)
    /// once the walk is long. `None` when filters are off, not yet wanted, or the commit has none (root).
    fn filters(&mut self, id: &ObjectId, c: &Commit) -> Result<Option<Vec<Filter>>> {
        if !self.repo.use_commit_graph || self.specs.iter().any(String::is_empty) {
            return Ok(None);
        }
        if let Some(f) = self.blooms.as_ref().and_then(|b| b.get(id)) {
            return Ok((f.len() == c.parents.len()).then_some(f));
        }
        // a zone that already has a filter file keeps extending it; a new one waits for a long walk
        if self.asked < self.bloom_after && !self.blooms.as_ref().is_some_and(PathBlooms::has_file) {
            return Ok(None);
        }
        let repo = self.repo;
        let zone = &self.zone;
        let b = self.blooms.get_or_insert_with(|| repo.load_path_blooms(zone));
        if let Some(f) = b.get(id) {
            return Ok((f.len() == c.parents.len()).then_some(f));
        }
        let Some(f) = repo.compute_filters(c)? else { return Ok(None) };
        b.insert(*id, f.clone());
        Ok(Some(f))
    }

    /// Load the stored filters now (a walk that is going to be long wants them from the start).
    pub(crate) fn preload_filters(&mut self) {
        if self.repo.use_commit_graph && self.blooms.is_none() && !self.specs.iter().any(String::is_empty) {
            self.blooms = Some(self.repo.load_path_blooms(&self.zone));
        }
    }

    /// True when stored filters already prove that commit `id` (with `nparents` parents) equals one
    /// of its parents at every path, so it does not touch them and need not be read.
    pub(crate) fn quick_miss(&self, id: &ObjectId, nparents: usize) -> bool {
        let Some(b) = &self.blooms else { return false };
        match b.get(id) {
            Some(fs) if nparents > 0 && fs.len() == nparents => fs.iter().any(|f| !self.specs.iter().any(|s| may_contain(f, s))),
            _ => false,
        }
    }

    /// Does `c` (with id `id`) change the paths relative to its parents?
    pub fn touches(&mut self, id: &ObjectId, c: &Commit) -> Result<bool> {
        self.asked += 1;
        if c.parents.is_empty() {
            let mine = self.entries(c.root)?;
            for e in mine.into_iter().flatten() {
                if treeops::entry_has_files(self.repo.store(), e)? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        let n = if self.first_parent_only { 1 } else { c.parents.len() };
        if let Some(fs) = self.filters(id, c)? {
            // a parent whose diff cannot involve any of the paths makes the commit "same as parent"
            if fs.iter().take(n).any(|f| !self.specs.iter().any(|s| may_contain(f, s))) {
                return Ok(false);
            }
        }
        let mine = self.entries(c.root)?;
        for p in &c.parents[..n] {
            let pr = self.repo.commit_obj(p)?.root;
            if self.entries(pr)? == mine {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl Drop for PathFilter<'_> {
    fn drop(&mut self) {
        if let Some(b) = &self.blooms {
            if b.pending() >= if b.has_file() { BLOOM_FLUSH } else { BLOOM_FLUSH_NEW } {
                self.repo.save_path_blooms(&self.zone, b);
            }
        }
    }
}

/// Parent lookup for history walks: from the commit-graph cache when it is available (one
/// file read, no object decoding), else from the commit objects. Same answers either way.
pub struct ParentIndex<'a> {
    repo: &'a Repo,
    graph: Option<vcs_merge::CommitGraphFile>,
}

impl ParentIndex<'_> {
    pub fn parents(&self, id: &ObjectId) -> Result<Vec<ObjectId>> {
        if let Some(g) = &self.graph {
            if let Ok(p) = vcs_merge::CommitGraph::parents(g, id) {
                return Ok(p);
            }
        }
        Ok(self.repo.commit_obj(id)?.parents)
    }

    /// `from` and its first parents down to a root, newest first.
    pub fn first_parent_chain(&self, from: ObjectId) -> Result<Vec<ObjectId>> {
        let mut v = vec![from];
        let mut cur = from;
        while let Some(p) = self.parents(&cur)?.first() {
            v.push(*p);
            cur = *p;
        }
        Ok(v)
    }

    /// How many commits are reachable from `from` but not from `hide` (both counted as reachable
    /// from themselves): the `n` of `describe`.
    pub fn count_not_reachable_from(&self, from: ObjectId, hide: ObjectId) -> Result<usize> {
        if let Some(n) = self.graph.as_ref().and_then(|g| g.count_reachable_not_from(&from, &hide)) {
            return Ok(n);
        }
        let behind = self.ancestors(hide)?;
        Ok(self.ancestors(from)?.iter().filter(|c| !behind.contains(*c)).count())
    }

    /// `from` and everything reachable from it.
    pub fn ancestors(&self, from: ObjectId) -> Result<std::collections::HashSet<ObjectId>> {
        let mut seen = std::collections::HashSet::new();
        let mut todo = vec![from];
        while let Some(id) = todo.pop() {
            if seen.insert(id) {
                todo.extend(self.parents(&id)?);
            }
        }
        Ok(seen)
    }
}

impl Repo {
    /// A parent lookup that uses the commit-graph cache for the zone of `tips` when it can
    /// (it may build or extend the cache file, like `merge_bases`).
    pub fn parent_index(&self, tips: &[ObjectId]) -> ParentIndex<'_> {
        ParentIndex {
            repo: self,
            graph: self.commit_graph(tips),
        }
    }
}

/// Skips along first parents over commits that provably do not change one path, using the
/// commit-graph for parents and the changed-path filters for "does this commit change it?"
/// (`blame` and friends: the answer for such a commit is the same as for its parent).
pub struct FirstParentSkip<'a> {
    repo: &'a Repo,
    zone: String,
    graph: Option<vcs_merge::CommitGraphFile>,
    blooms: PathBlooms,
    seen: usize,
}

impl<'a> FirstParentSkip<'a> {
    pub fn new(repo: &'a Repo, zone: &str, tip: ObjectId) -> FirstParentSkip<'a> {
        let on = repo.use_commit_graph;
        FirstParentSkip {
            repo,
            zone: zone.to_owned(),
            graph: if on { repo.commit_graph(&[tip]) } else { None },
            blooms: if on { repo.load_path_blooms(zone) } else { PathBlooms::default() },
            seen: 0,
        }
    }

    /// Follow first parents from `id` while the stored filters say the commit leaves the zone-relative
    /// `rel` (a file) equal to its first parent. Returns the commit reached (`id` if none skipped).
    pub fn skip(&self, mut id: ObjectId, rel: &str) -> ObjectId {
        let Some(g) = &self.graph else { return id };
        loop {
            let Ok(ps) = vcs_merge::CommitGraph::parents(g, &id) else {
                return id;
            };
            let Some(first) = ps.first() else { return id };
            match self.blooms.get(&id) {
                Some(fs) if fs.len() == ps.len() && !may_contain(&fs[0], rel) => id = *first,
                _ => return id,
            }
        }
    }

    /// A commit the caller loaded anyway: learn its filters (after a while, or when a file exists).
    pub fn note(&mut self, id: &ObjectId, c: &Commit) {
        if self.graph.is_none() || c.parents.is_empty() || self.blooms.get(id).is_some() {
            return;
        }
        self.seen += 1;
        if self.seen < BLOOM_AFTER && !self.blooms.has_file() {
            return;
        }
        if let Ok(Some(f)) = self.repo.compute_filters(c) {
            self.blooms.insert(*id, f);
        }
    }
}

impl Drop for FirstParentSkip<'_> {
    fn drop(&mut self) {
        if self.blooms.pending() >= if self.blooms.has_file() { BLOOM_FLUSH } else { BLOOM_FLUSH_NEW } {
            self.repo.save_path_blooms(&self.zone, &self.blooms);
        }
    }
}
