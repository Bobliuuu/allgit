//! `Repo::fsck`: verify every object (hash + canonical decode) and the graph invariants, on all cores, with memory
//! that is a few dozen bytes per object (not per commit message / tree entry).
//!
//! Shape (all numbers per 10M-commit deep repo, 40M objects):
//! * `ids`: every object id, sorted (1.3 GB). Existence of a referenced id is an interpolation search in it
//!   (ids are uniform hashes), not a pack-index probe per tree entry (1e9 probes before).
//! * `facts`: one atomic word per id (8 B): not yet seen / blob of N bytes / failed / commit in zone Z.
//!   It replaces the whole-set `HashMap<ObjectId, Listed>` and the `BTreeMap<ObjectId, Commit>` of every commit.
//! * Pack objects are read, hashed and decoded in one pass by `Pack::scan` (mapped pack, pack order, every core):
//!   the second whole-pack hash of the old `verify_packs` is the same pass now. Objects it did not see (loose ones,
//!   unreadable packs) go through `store.get` afterwards, in parallel.
//! * The remote closure (every object reachable from a remote-tracking head, a second full read of the repository
//!   in a single thread) is only computed when some object is actually missing.
use crate::error::Result;
use crate::repo::{FsckReport, Repo};
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use vcs_objects::{Object, ObjectId};
use vcs_store::ObjectStore;

/// Objects per work item of the parallel passes; `VCS_FSCK_BATCH` lowers it (tests use tiny batches to cross item
/// boundaries).
const FSCK_BATCH: usize = 4096;

fn fsck_batch() -> usize {
    std::env::var("VCS_FSCK_BATCH")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .map_or(FSCK_BATCH, |n: usize| n.min(FSCK_BATCH))
}

// facts word: low 2 bits tag.
const UNSEEN: u64 = u64::MAX;
const OTHER: u64 = 0;
const FAILED: u64 = 2;
fn blob_fact(len: usize) -> u64 {
    1 | ((len as u64) << 2)
}
fn commit_fact(zone: u32) -> u64 {
    3 | ((zone as u64) << 2)
}

/// Sorted ids with an interpolation lookup (ids are blake3 outputs: the top 64 bits are uniform).
struct IdSet(Vec<ObjectId>);

impl IdSet {
    fn find(&self, id: &ObjectId) -> Option<usize> {
        let ids = &self.0;
        let n = ids.len();
        if n == 0 {
            return None;
        }
        let k = u64::from_be_bytes(id.as_bytes()[..8].try_into().expect("8 bytes"));
        let g = ((k as u128 * n as u128) >> 64) as usize;
        let (lo, hi);
        if ids[g] < *id {
            let mut l = g + 1;
            let mut step = 8;
            hi = loop {
                let h = l + step;
                if h >= n {
                    break n;
                }
                if ids[h - 1] >= *id {
                    break h;
                }
                l = h;
                step *= 2;
            };
            lo = l;
        } else {
            let mut h = g + 1;
            let mut step = 8;
            lo = loop {
                if h <= step {
                    break 0;
                }
                let l = h - step;
                if ids[l] < *id {
                    break l + 1;
                }
                h = l + 1;
                step *= 2;
            };
            hi = h;
        }
        ids[lo..hi].binary_search(id).ok().map(|i| lo + i)
    }
}

/// Zone names, interned: a commit record carries the small number.
#[derive(Default)]
struct Zones {
    names: Mutex<(HashMap<String, u32>, Vec<String>)>,
}

impl Zones {
    fn id(&self, z: &str) -> u32 {
        let mut g = self.names.lock().unwrap();
        if let Some(&i) = g.0.get(z) {
            return i;
        }
        let i = g.1.len() as u32;
        g.1.push(z.to_owned());
        g.0.insert(z.to_owned(), i);
        i
    }
    fn name(&self, i: u32) -> String {
        self.names.lock().unwrap().1[i as usize].clone()
    }
}

/// What one worker found. Merged in a fixed order after the pass.
#[derive(Default)]
struct Acc {
    problems: Vec<String>,
    /// Referenced ids that are in no pack index and no loose file, with the message to report if not refetchable.
    misses: Vec<(ObjectId, String)>,
    chunked: Vec<(ObjectId, vcs_objects::ChunkedBlob)>,
    /// `(index of the commit in ids, zone, start in parents, count)`
    commits: Vec<(usize, u32, usize, usize)>,
    /// indexes in `ids` of the parents of `commits`, flat
    parents: Vec<usize>,
    last_zone: Option<(String, u32)>,
}

struct Ctx<'a> {
    repo: &'a Repo,
    ids: IdSet,
    facts: Vec<AtomicU64>,
    zones: Zones,
}

impl Ctx<'_> {
    /// Whether the store has `id`. The sorted id list answers almost always; the store is asked for the rest
    /// (an object that appeared after listing, or a lookup the list cannot see).
    fn exists(&self, id: &ObjectId, acc: &mut Acc, what: impl FnOnce() -> String) -> Option<usize> {
        if let Some(i) = self.ids.find(id) {
            return Some(i);
        }
        match self.repo.store.contains(id) {
            Ok(true) => None,
            Ok(false) => {
                acc.misses.push((*id, what()));
                None
            }
            Err(e) => {
                acc.problems.push(format!("{}: {e}", what()));
                None
            }
        }
    }

    /// Check one object, whose bytes are already hashed (pack) or read by `store.get`. Returns the fact word.
    fn judge(&self, idx: usize, id: &ObjectId, obj: Object, acc: &mut Acc) -> u64 {
        match obj {
            Object::Commit(c) => {
                let zone = match &acc.last_zone {
                    Some((z, i)) if *z == c.zone => *i,
                    _ => {
                        let i = self.zones.id(&c.zone);
                        acc.last_zone = Some((c.zone.clone(), i));
                        i
                    }
                };
                self.exists(&c.root, acc, || format!("commit {id}: missing root tree {}", c.root));
                let start = acc.parents.len();
                for p in &c.parents {
                    if let Some(pi) = self.exists(p, acc, || format!("commit {id}: missing parent {p}")) {
                        acc.parents.push(pi);
                    }
                }
                let n = acc.parents.len() - start;
                acc.commits.push((idx, zone, start, n));
                commit_fact(zone)
            }
            Object::Tree(t) => {
                for e in t.entries() {
                    self.exists(&e.id, acc, || format!("tree {id}: missing entry {} -> {}", e.name, e.id));
                }
                OTHER
            }
            Object::Snapshot(s) => {
                for (a, c) in &s.zones {
                    self.exists(c, acc, || format!("snapshot {id}: missing commit {c} for zone {a}"));
                }
                OTHER
            }
            Object::Chunked(m) => {
                acc.chunked.push((*id, m));
                OTHER
            }
            Object::Blob(b) => blob_fact(b.len()),
            _ => OTHER,
        }
    }

    /// A pack object that passed the pack-level checks (framing, codec, hash): decode it and check what it names.
    fn packed(&self, id: &ObjectId, enc: Option<&[u8]>, acc: &mut Acc) {
        let Some(idx) = self.ids.find(id) else { return };
        let Some(enc) = enc else {
            // the pack scan reported the problem; a valid copy elsewhere may still override this
            let _ = self.facts[idx].compare_exchange(UNSEEN, FAILED, Ordering::AcqRel, Ordering::Acquire);
            return;
        };
        // A blob is only measured: its content is never copied.
        let fact = if enc.len() >= 2 && enc[0] == vcs_objects::Kind::Blob as u8 && enc[1] == vcs_objects::FORMAT_VERSION {
            blob_fact(enc.len() - 2)
        } else {
            match Object::decode(enc) {
                Ok(o) => {
                    // claim the object before judging it: an id in two packs is judged once
                    let cur = self.facts[idx].load(Ordering::Acquire);
                    if cur != UNSEEN && cur != FAILED {
                        return;
                    }
                    if self.facts[idx]
                        .compare_exchange(cur, OTHER, Ordering::AcqRel, Ordering::Acquire)
                        .is_err()
                    {
                        return;
                    }
                    let f = self.judge(idx, id, o, acc);
                    self.facts[idx].store(f, Ordering::Release);
                    return;
                }
                Err(e) => {
                    acc.problems.push(format!("{id}: {e}"));
                    FAILED
                }
            }
        };
        let cur = self.facts[idx].load(Ordering::Acquire);
        if cur == UNSEEN || cur == FAILED {
            let _ = self.facts[idx].compare_exchange(cur, fact, Ordering::AcqRel, Ordering::Acquire);
        }
    }

    /// Objects the pack scan did not deliver (loose files, unreadable packs): `store.get`, which hashes and decodes.
    fn fallback(&self, idx: usize, acc: &mut Acc) {
        if self.facts[idx].load(Ordering::Acquire) != UNSEEN {
            return;
        }
        let id = &self.ids.0[idx];
        let fact = match self.repo.store.get(id) {
            Err(e) => {
                acc.problems.push(format!("{id}: {e}"));
                FAILED
            }
            Ok(Object::Blob(b)) => blob_fact(b.len()),
            Ok(o) => self.judge(idx, id, o, acc),
        };
        self.facts[idx].store(fact, Ordering::Release);
    }
}

/// Run `f` over `0..n` in work items of `item` indexes on every core; each worker keeps its own `Acc`.
fn par_items(n: usize, item: usize, f: &(dyn Fn(usize, &mut Acc) + Sync)) -> Vec<Acc> {
    let threads = std::thread::available_parallelism().map_or(1, |t| t.get()).max(1);
    let next = AtomicUsize::new(0);
    let work = |next: &AtomicUsize| {
        let mut acc = Acc::default();
        loop {
            let lo = next.fetch_add(item, Ordering::Relaxed);
            if lo >= n {
                break;
            }
            for i in lo..(lo + item).min(n) {
                f(i, &mut acc);
            }
        }
        acc
    };
    if threads == 1 || n < 64 {
        return vec![work(&next)];
    }
    std::thread::scope(|sc| {
        let hs: Vec<_> = (0..threads).map(|_| sc.spawn(|| work(&next))).collect();
        hs.into_iter().map(|h| h.join().expect("fsck worker panicked")).collect()
    })
}

impl Repo {
    /// Ids the remote is known to have: everything reachable, through objects we hold, from a
    /// remote-tracking head (whose whole closure was verified and fetched). Missing ids are
    /// included as leaves.
    fn remote_closure(&self) -> Result<BTreeSet<ObjectId>> {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<ObjectId> = self.remote_refs()?.branches.values().copied().collect();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Ok(o) = self.store.get(&id) {
                crate::gc::children(&o, &mut stack);
            }
        }
        Ok(seen)
    }

    /// Verify every object (hash + canonical decode) and graph invariants.
    pub fn fsck(&self) -> Result<FsckReport> {
        let mut rep = FsckReport::default();
        let ids = self.store.list()?;
        rep.objects = ids.len();
        let ctx = Ctx {
            repo: self,
            facts: (0..ids.len()).map(|_| AtomicU64::new(UNSEEN)).collect(),
            ids: IdSet(ids),
            zones: Zones::default(),
        };
        // Packs: hash + decode in pack order on every core. Then whatever no pack delivered, through `store.get`.
        let (pack_problems, mut accs) = self.store.scan_packs(Acc::default, |acc, id, enc| ctx.packed(id, enc, acc));
        accs.extend(par_items(ctx.ids.0.len(), fsck_batch(), &|i, acc| ctx.fallback(i, acc)));

        // The remote closure is only worth computing once something is found missing.
        let mut refetchable: Option<BTreeSet<ObjectId>> = None;
        let mut evictable: BTreeSet<ObjectId> = BTreeSet::new();
        let mut object_problems: Vec<String> = Vec::new();
        let mut misses: Vec<(ObjectId, String)> = Vec::new();
        let mut chunked = Vec::new();
        for a in &mut accs {
            object_problems.append(&mut a.problems);
            misses.append(&mut a.misses);
            chunked.append(&mut a.chunked);
        }
        object_problems.sort();
        rep.problems.append(&mut object_problems);
        misses.sort();
        // A missing object is an evictable cache miss only if the remote is known to have it.
        let mut missing = |this: &Self, id: &ObjectId, what: String, problems: &mut Vec<String>| -> Result<()> {
            if this.store.contains(id)? {
                return Ok(());
            }
            if refetchable.is_none() {
                refetchable = Some(this.remote_closure()?);
            }
            if refetchable.as_ref().is_some_and(|r| r.contains(id)) {
                evictable.insert(*id);
            } else {
                problems.push(what);
            }
            Ok(())
        };
        for (id, what) in misses {
            missing(self, &id, what, &mut rep.problems)?;
        }

        // Chunk manifests: every chunk must be a plain blob of the recorded length.
        chunked.sort_by_key(|(id, _)| *id);
        for (id, m) in &chunked {
            for c in &m.chunks {
                missing(
                    self,
                    &c.id,
                    format!("chunk manifest {id}: missing chunk {}", c.id),
                    &mut rep.problems,
                )?;
                let state = match ctx.ids.find(&c.id) {
                    Some(i) => ctx.facts[i].load(Ordering::Acquire),
                    None if self.store.contains(&c.id)? => match self.store.get(&c.id) {
                        Ok(Object::Blob(b)) => blob_fact(b.len()),
                        Ok(_) => OTHER,
                        Err(_) => FAILED,
                    },
                    None => continue,
                };
                match state {
                    s if s & 3 == 1 && s != UNSEEN && (s >> 2) == c.len as u64 => {}
                    FAILED => {} // reported when the chunk itself is checked
                    _ => rep
                        .problems
                        .push(format!("chunk manifest {id}: chunk {} is not a blob of {} bytes", c.id, c.len)),
                }
            }
        }

        // Commit parents live in the zone of their child.
        let mut zone_problems = Vec::new();
        for a in &accs {
            for &(idx, zone, start, n) in &a.commits {
                for &pi in &a.parents[start..start + n] {
                    let f = ctx.facts[pi].load(Ordering::Acquire);
                    if f != UNSEEN && f & 3 == 3 && (f >> 2) as u32 != zone {
                        zone_problems.push(format!(
                            "commit {}: parent {} is in zone {} not {}",
                            ctx.ids.0[idx],
                            ctx.ids.0[pi],
                            ctx.zones.name((f >> 2) as u32),
                            ctx.zones.name(zone)
                        ));
                    }
                }
            }
        }
        zone_problems.sort();
        rep.problems.append(&mut zone_problems);

        // No index of any worktree may name a blob that is not in the store.
        let views = self.fsck_worktrees(&mut rep.problems);
        let linked = views.len() > 1;
        for (wt, v) in &views {
            let mut absent: Vec<(String, ObjectId)> = Vec::new();
            let scanned = (|| -> Result<()> {
                let img = v.index_image()?;
                let find = |id: &ObjectId| ctx.ids.find(id).is_some();
                match v.index_view(&img)? {
                    // Streaming over the index blocks on every core: no per-entry map of the index in memory.
                    Some(view) => {
                        let parts = view.par_scan(
                            std::thread::available_parallelism().map_or(1, |t| t.get()),
                            &Vec::<(String, ObjectId)>::new,
                            &|out, p, e| {
                                if !find(&e.id) {
                                    out.push((p.to_owned(), e.id));
                                }
                            },
                        )?;
                        absent.extend(parts.into_iter().flatten());
                    }
                    None => {
                        for (p, e) in &img.to_index()?.entries {
                            if !find(&e.id) {
                                absent.push((p.clone(), e.id));
                            }
                        }
                    }
                }
                Ok(())
            })();
            if let Err(e) = scanned {
                rep.problems.push(format!("worktree {wt}: index: {e}"));
                continue;
            }
            for (p, id) in absent {
                let what = if linked {
                    format!("index {p} (worktree {wt}): missing blob {id}")
                } else {
                    format!("index {p}: missing blob {id}")
                };
                missing(self, &id, what, &mut rep.problems)?;
            }
        }
        let refs = self.refs()?;
        for ((a, b), h) in &refs.branches {
            let commit_zone = ctx
                .ids
                .find(h)
                .map(|i| ctx.facts[i].load(Ordering::Acquire))
                .filter(|f| *f != UNSEEN && f & 3 == 3)
                .map(|f| ctx.zones.name((f >> 2) as u32));
            match commit_zone {
                None => {
                    if refetchable.is_none() {
                        refetchable = Some(self.remote_closure()?);
                    }
                    if refetchable.as_ref().is_some_and(|r| r.contains(h)) && !self.store.contains(h)? {
                        evictable.insert(*h);
                    } else {
                        rep.problems.push(format!("ref {a}/{b}: points to missing/non-commit {h}"));
                    }
                }
                Some(z) if &z != a => rep.problems.push(format!("ref {a}/{b}: commit belongs to zone {z}")),
                _ => {}
            }
        }
        rep.evictable = evictable.into_iter().collect();
        self.fsck_ops(&mut rep.problems);
        self.fsck_commit_graph(&mut rep.problems);
        self.fsck_path_filters(&mut rep.problems);
        self.fsck_reach_cache(&mut rep.problems);
        rep.problems.extend(pack_problems);
        Ok(rep)
    }
}
