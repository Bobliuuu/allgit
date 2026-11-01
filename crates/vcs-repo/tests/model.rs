//! Randomized model test for the whole local repository.
//!
//! A tiny in-memory reference model (files are single-line strings, so a three-way merge is a
//! per-path decision) is driven by the same random operation sequence as a real repository in a
//! temp directory. After EVERY step the real worktree, index (via `status`), HEAD, branch refs,
//! subscriptions, zones and operation log are compared with the model. Operations the model says
//! must be refused must leave the real repository byte-for-byte unchanged in those respects.
//!
//! Operations: write/delete a file, add, commit, branch, checkout, merge, rebase, restore
//! (worktree and staged), rm, op undo, op restore, zone create / add / remove.
//!
//! Crash points: for a third of the mutating operations, the operation is replayed on a copy of
//! the repository once per atomic state-file write it performs, with that write "crashing" (a torn
//! temp file, target untouched, see `vcs_repo::failpoint`). The crashed repository must pass
//! `fsck`, still answer `status`/`op log`, be in a state "before" or "after" the operation per
//! file (never anything else), have recorded the operation only once everything else landed,
//! and accept a follow-up `add`.
//!
//! Failures are shrunk to a minimal script (see `Op::line`) printed in the panic message; save it
//! under `tests/model_seeds/*.script` to keep it as a regression. The fixed seeds are in
//! `tests/model_seeds/seeds.txt` (`seed steps` per line). `MODEL_SEEDS=<n>` and `MODEL_STEPS=<n>`
//! drive the longer `#[ignore]` run: `cargo test -p vcs-repo --test model -- --ignored`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use vcs_objects::{ObjectId, Signature};
use vcs_repo::{treeops, ChangeKind, MergeOutcome, RebaseOutcome, Repo, ROOT_ZONE};

type Files = BTreeMap<String, String>;

const FILES: &[&str] = &[
    "a.txt",
    "b.txt",
    "d/c.txt",
    "d/e.txt",
    "z1/f.txt",
    "z1/g/h.txt",
    "w/q.txt",
    "w/z2/f.txt",
];
const SPECS: &[&str] = &[
    "",
    "a.txt",
    "b.txt",
    "d",
    "d/c.txt",
    "z1",
    "z1/f.txt",
    "z1/g",
    "w",
    "w/z2",
    "w/z2/f.txt",
    "nope.txt",
];
const BRANCHES: &[&str] = &["main", "b1", "b2"];
const ZONES: &[&str] = &["root", "root", "root", "z1", "z2", "zd"];
const ZONE_DEFS: &[(&str, &str)] = &[
    ("z1", "z1"),
    ("z2", "w/z2"),
    ("zd", "d"),
    ("z1", "w/z2"),
    ("z2", "z1/g"),
    ("zw", "w"),
];

// ------------------------------------------------------------------ operations

#[derive(Clone, Debug, PartialEq)]
enum Op {
    Write(String, u8),
    Delete(String),
    Add(String),
    Commit,
    Branch(String, String),
    Checkout(String, String),
    Merge(String, String),
    Rebase(String, String),
    Restore(String),
    Unstage(String),
    Rm {
        cached: bool,
        force: bool,
        spec: String,
    },
    Undo,
    GoTo(usize),
    ZoneCreate(String, String),
    ZoneRemove(String),
    ZoneAdd(String, String),
    /// Destructive local GC (after dropping an orphan in, and on odd serials after a repack).
    Gc,
}

fn show_spec(s: &str) -> &str {
    if s.is_empty() {
        "."
    } else {
        s
    }
}
fn read_spec(s: &str) -> String {
    if s == "." {
        String::new()
    } else {
        s.to_owned()
    }
}

impl Op {
    /// One-line replayable form.
    fn line(&self) -> String {
        match self {
            Op::Write(p, v) => format!("write {p} {v}"),
            Op::Delete(p) => format!("delete {p}"),
            Op::Add(s) => format!("add {}", show_spec(s)),
            Op::Commit => "commit".into(),
            Op::Branch(z, b) => format!("branch {z} {b}"),
            Op::Checkout(z, b) => format!("checkout {z} {b}"),
            Op::Merge(z, b) => format!("merge {z} {b}"),
            Op::Rebase(z, b) => format!("rebase {z} {b}"),
            Op::Restore(s) => format!("restore {}", show_spec(s)),
            Op::Unstage(s) => format!("unstage {}", show_spec(s)),
            Op::Rm { cached, force, spec } => format!(
                "rm {}{} {}",
                if *cached { "c" } else { "-" },
                if *force { "f" } else { "-" },
                show_spec(spec)
            ),
            Op::Undo => "undo".into(),
            Op::GoTo(n) => format!("op-restore {n}"),
            Op::ZoneCreate(n, p) => format!("zone-create {n} {p}"),
            Op::ZoneRemove(z) => format!("zone-remove {z}"),
            Op::ZoneAdd(z, b) => format!("zone-add {z} {b}"),
            Op::Gc => "gc".into(),
        }
    }

    fn parse(line: &str) -> Option<Op> {
        let w: Vec<&str> = line.split_whitespace().collect();
        let s = |i: usize| w.get(i).map(|x| x.to_string());
        Some(match *w.first()? {
            "write" => Op::Write(s(1)?, w.get(2)?.parse().ok()?),
            "delete" => Op::Delete(s(1)?),
            "add" => Op::Add(read_spec(w.get(1)?)),
            "commit" => Op::Commit,
            "branch" => Op::Branch(s(1)?, s(2)?),
            "checkout" => Op::Checkout(s(1)?, s(2)?),
            "merge" => Op::Merge(s(1)?, s(2)?),
            "rebase" => Op::Rebase(s(1)?, s(2)?),
            "restore" => Op::Restore(read_spec(w.get(1)?)),
            "unstage" => Op::Unstage(read_spec(w.get(1)?)),
            "rm" => Op::Rm {
                cached: w.get(1)?.contains('c'),
                force: w.get(1)?.contains('f'),
                spec: read_spec(w.get(2)?),
            },
            "undo" => Op::Undo,
            "op-restore" => Op::GoTo(w.get(1)?.parse().ok()?),
            "zone-create" => Op::ZoneCreate(s(1)?, s(2)?),
            "zone-remove" => Op::ZoneRemove(s(1)?),
            "zone-add" => Op::ZoneAdd(s(1)?, s(2)?),
            "gc" => Op::Gc,
            _ => return None,
        })
    }

    /// Operations that change repository state files (candidates for crash points).
    fn crash_candidate(&self) -> bool {
        !matches!(self, Op::Write(..) | Op::Delete(_) | Op::Rebase(..) | Op::Gc)
    }
}

fn parse_script(text: &str) -> Vec<Op> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| Op::parse(l).unwrap_or_else(|| panic!("bad script line {l:?}")))
        .collect()
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
}

fn generate(seed: u64, steps: usize) -> Vec<Op> {
    let mut r = Rng(seed);
    let mut out = Vec::new();
    for _ in 0..steps {
        let k = r.below(100);
        let op = match k {
            0..=19 => Op::Write(r.pick(FILES).into(), r.below(3) as u8),
            20..=24 => Op::Delete(r.pick(FILES).into()),
            25..=38 => Op::Add(r.pick(SPECS).into()),
            39..=52 => Op::Commit,
            53..=58 => Op::Branch(r.pick(ZONES).into(), r.pick(&BRANCHES[1..]).into()),
            59..=65 => Op::Checkout(r.pick(ZONES).into(), r.pick(BRANCHES).into()),
            66..=72 => Op::Merge(r.pick(ZONES).into(), r.pick(BRANCHES).into()),
            73..=76 => Op::Rebase(r.pick(ZONES).into(), r.pick(BRANCHES).into()),
            77..=80 => Op::Restore(r.pick(SPECS).into()),
            81..=83 => Op::Unstage(r.pick(SPECS).into()),
            84..=86 => Op::Rm {
                cached: r.below(2) == 0,
                force: r.below(3) == 0,
                spec: r.pick(SPECS).into(),
            },
            87..=89 => Op::Undo,
            90..=91 => Op::GoTo(r.below(40)),
            92..=94 => {
                let (n, p) = ZONE_DEFS[r.below(ZONE_DEFS.len())];
                Op::ZoneCreate(n.into(), p.into())
            }
            95..=97 => Op::ZoneRemove(r.pick(&ZONES[3..]).into()),
            98 => Op::ZoneAdd(r.pick(&ZONES[3..]).into(), r.pick(BRANCHES).into()),
            _ => Op::Gc,
        };
        out.push(op);
    }
    out
}

fn content(v: u8) -> String {
    format!("v{v}\n")
}

// ------------------------------------------------------------------ reference model

#[derive(Clone, Debug, PartialEq)]
struct MCommit {
    zone: String,
    parents: Vec<usize>,
    files: Files,
    /// Identity of the logical change: equal serial + parents + tree means the same object.
    serial: u64,
}

#[derive(Clone, Debug, PartialEq, Default)]
struct View {
    branches: BTreeMap<(String, String), usize>,
    subs: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Out {
    Done,
    UpToDate,
    FastForward,
    Merged,
    Conflict,
}

#[derive(Clone)]
struct Model {
    zones: BTreeMap<String, String>,
    view: View,
    commits: Vec<MCommit>,
    wt: Files,
    ix: Files,
    ops: Vec<(Option<usize>, View)>,
    head: usize,
}

fn under(path: &str, prefix: &str) -> bool {
    prefix.is_empty() || path == prefix || path.strip_prefix(prefix).is_some_and(|r| r.starts_with('/'))
}

type R<T> = Result<T, ()>;

impl Model {
    fn new() -> Model {
        let mut view = View::default();
        view.subs.insert(ROOT_ZONE.into(), "main".into());
        Model {
            zones: BTreeMap::new(),
            ops: vec![(None, view.clone())],
            view,
            commits: vec![],
            wt: Files::new(),
            ix: Files::new(),
            head: 0,
        }
    }

    fn owner(&self, path: &str) -> String {
        for (z, p) in &self.zones {
            if under(path, p) {
                return z.clone();
            }
        }
        ROOT_ZONE.into()
    }
    fn zone_path(&self, z: &str) -> Option<String> {
        if z == ROOT_ZONE {
            Some(String::new())
        } else {
            self.zones.get(z).cloned()
        }
    }
    fn subscribed(&self, z: &str) -> bool {
        self.view.subs.contains_key(z)
    }
    fn head_of(&self, z: &str) -> Option<usize> {
        let b = self.view.subs.get(z).map(String::as_str).unwrap_or("main");
        self.view.branches.get(&(z.to_owned(), b.to_owned())).copied()
    }
    fn files_of(&self, c: Option<usize>) -> Files {
        c.map(|i| self.commits[i].files.clone()).unwrap_or_default()
    }
    fn head_files(&self, z: &str) -> Files {
        self.files_of(self.head_of(z))
    }
    fn ix_zone(&self, z: &str) -> Files {
        self.ix
            .iter()
            .filter(|(p, _)| self.owner(p) == z)
            .map(|(p, c)| (p.clone(), c.clone()))
            .collect()
    }
    fn staged(&self, z: &str) -> BTreeSet<(String, &'static str)> {
        let (h, i) = (self.head_files(z), self.ix_zone(z));
        let mut s = BTreeSet::new();
        for (p, c) in &i {
            match h.get(p) {
                None => s.insert((p.clone(), "added")),
                Some(o) if o != c => s.insert((p.clone(), "modified")),
                _ => false,
            };
        }
        for p in h.keys() {
            if !i.contains_key(p) {
                s.insert((p.clone(), "deleted"));
            }
        }
        s
    }
    fn unstaged(&self, z: &str) -> BTreeSet<(String, &'static str)> {
        let mut s = BTreeSet::new();
        for (p, c) in self.ix_zone(z) {
            match self.wt.get(&p) {
                None => s.insert((p, "deleted")),
                Some(w) if *w != c => s.insert((p, "modified")),
                _ => false,
            };
        }
        s
    }
    fn dirty(&self, z: &str) -> bool {
        !self.staged(z).is_empty() || !self.unstaged(z).is_empty()
    }

    fn mk_commit(&mut self, c: MCommit) -> usize {
        if let Some(i) = self.commits.iter().position(|x| *x == c) {
            return i;
        }
        self.commits.push(c);
        self.commits.len() - 1
    }
    fn push_op(&mut self) {
        self.ops.push((Some(self.head), self.view.clone()));
        self.head = self.ops.len() - 1;
    }

    fn ancestors(&self, c: usize) -> BTreeSet<usize> {
        let mut seen = BTreeSet::new();
        let mut q = vec![c];
        while let Some(x) = q.pop() {
            if seen.insert(x) {
                q.extend(self.commits[x].parents.iter().copied());
            }
        }
        seen
    }
    /// Maximal common ancestors.
    fn bases(&self, a: usize, b: usize) -> Vec<usize> {
        let common: BTreeSet<usize> = self.ancestors(a).intersection(&self.ancestors(b)).copied().collect();
        common
            .iter()
            .copied()
            .filter(|&c| !common.iter().any(|&d| d != c && self.ancestors(d).contains(&c)))
            .collect()
    }

    /// Move worktree + index of `zone` from `old` to `new`, mirroring the real preflight.
    fn transition(&mut self, zone: &str, old: &Files, new: &Files) -> R<()> {
        for (p, c) in new {
            if old.get(p) != Some(c) {
                if self.owner(p) != zone {
                    return Err(());
                }
                if !old.contains_key(p) && self.wt.contains_key(p) {
                    return Err(());
                }
            }
        }
        for p in old.keys().filter(|p| !new.contains_key(*p)) {
            self.wt.remove(p);
            self.ix.remove(p);
        }
        for (p, c) in new {
            if old.get(p) != Some(c) {
                self.wt.insert(p.clone(), c.clone());
                self.ix.insert(p.clone(), c.clone());
            }
        }
        Ok(())
    }

    fn apply(&mut self, op: &Op, serial: u64) -> R<Out> {
        match op {
            // GC must be invisible to every observable the model tracks.
            Op::Gc => Ok(Out::Done),
            Op::Write(p, v) => {
                if self.subscribed(&self.owner(p)) {
                    self.wt.insert(p.clone(), content(*v));
                }
                Ok(Out::Done)
            }
            Op::Delete(p) => {
                if self.subscribed(&self.owner(p)) {
                    self.wt.remove(p);
                }
                Ok(Out::Done)
            }
            Op::Add(s) => self.add(s).map(|_| Out::Done),
            Op::Commit => self.commit(serial).map(|_| Out::Done),
            Op::Branch(z, b) => {
                if self.zone_path(z).is_none() {
                    return Err(());
                }
                let Some(h) = self.head_of(z) else { return Err(()) };
                let key = (z.clone(), b.clone());
                if self.view.branches.contains_key(&key) {
                    return Err(());
                }
                self.view.branches.insert(key, h);
                self.push_op();
                Ok(Out::Done)
            }
            Op::Checkout(z, b) => {
                let Some(cur) = self.view.subs.get(z).cloned() else {
                    return Err(());
                };
                if cur == *b {
                    return Ok(Out::Done);
                }
                let Some(&target) = self.view.branches.get(&(z.clone(), b.clone())) else {
                    return Err(());
                };
                if self.dirty(z) {
                    return Err(());
                }
                let (old, new) = (self.head_files(z), self.files_of(Some(target)));
                self.transition(z, &old, &new)?;
                self.view.subs.insert(z.clone(), b.clone());
                self.push_op();
                Ok(Out::Done)
            }
            Op::Merge(z, b) => self.merge(z, b, serial),
            Op::Rebase(z, b) => self.rebase(z, b),
            Op::Restore(s) => {
                self.require_subscribed(s)?;
                let hits: Vec<String> = self.ix.keys().filter(|k| under(k, s)).cloned().collect();
                if hits.is_empty() && !s.is_empty() {
                    return Err(());
                }
                for k in hits {
                    if self.wt.get(&k) != self.ix.get(&k) {
                        self.wt.insert(k.clone(), self.ix[&k].clone());
                    }
                }
                self.push_op();
                Ok(Out::Done)
            }
            Op::Unstage(s) => {
                self.require_subscribed(s)?;
                let heads: BTreeMap<String, Files> = self.view.subs.keys().map(|z| (z.clone(), self.head_files(z))).collect();
                let mut cands = BTreeSet::new();
                let mut hit = false;
                for k in self.ix.keys().filter(|k| under(k, s)) {
                    cands.insert(k.clone());
                    hit = true;
                }
                for f in heads.values() {
                    for k in f.keys().filter(|k| under(k, s)) {
                        cands.insert(k.clone());
                        hit = true;
                    }
                }
                if !hit && !s.is_empty() {
                    return Err(());
                }
                for q in cands {
                    let head = heads.get(&self.owner(&q)).and_then(|f| f.get(&q)).cloned();
                    match (head, self.ix.get(&q).cloned()) {
                        (None, Some(_)) => {
                            self.ix.remove(&q);
                        }
                        (Some(h), e) if e.as_ref() != Some(&h) => {
                            self.ix.insert(q, h);
                        }
                        _ => {}
                    }
                }
                self.push_op();
                Ok(Out::Done)
            }
            Op::Rm { cached, force, spec } => {
                self.require_subscribed(spec)?;
                let paths: Vec<String> = self.ix.keys().filter(|k| under(k, spec)).cloned().collect();
                if paths.is_empty() {
                    return Err(());
                }
                if !force {
                    for p in &paths {
                        let in_head = self.head_files(&self.owner(p)).get(p) == self.ix.get(p);
                        let clean = self.wt.get(p) == self.ix.get(p);
                        let wt_changed = !clean;
                        if *cached {
                            if !in_head && wt_changed {
                                return Err(());
                            }
                        } else if !in_head || !clean {
                            return Err(());
                        }
                    }
                }
                for p in &paths {
                    if !cached {
                        self.wt.remove(p);
                    }
                    self.ix.remove(p);
                }
                self.push_op();
                Ok(Out::Done)
            }
            Op::Undo => {
                let Some(parent) = self.ops[self.head].0 else { return Err(()) };
                let target = self.ops[parent].1.clone();
                self.restore_view(&target)?;
                self.push_op();
                Ok(Out::Done)
            }
            Op::GoTo(n) => {
                let target = self.ops[n % self.ops.len()].1.clone();
                self.restore_view(&target)?;
                self.push_op();
                Ok(Out::Done)
            }
            Op::ZoneCreate(name, path) => {
                if self.zones.contains_key(name) || name == ROOT_ZONE {
                    return Err(());
                }
                if self.zones.values().any(|p| under(p, path) || under(path, p)) {
                    return Err(());
                }
                if self.ix.keys().any(|k| under(k, path)) || self.head_files(ROOT_ZONE).keys().any(|k| under(k, path)) {
                    return Err(());
                }
                self.zones.insert(name.clone(), path.clone());
                self.view.subs.insert(name.clone(), "main".into());
                self.push_op();
                Ok(Out::Done)
            }
            Op::ZoneAdd(z, b) => {
                if !self.zones.contains_key(z) || self.subscribed(z) {
                    return Err(());
                }
                let head = self.view.branches.get(&(z.clone(), b.clone())).copied();
                if head.is_none() && b != "main" {
                    return Err(());
                }
                let new = self.files_of(head);
                self.transition(z, &Files::new(), &new)?;
                self.view.subs.insert(z.clone(), b.clone());
                self.push_op();
                Ok(Out::Done)
            }
            Op::ZoneRemove(z) => {
                if z == ROOT_ZONE || !self.subscribed(z) || self.dirty(z) {
                    return Err(());
                }
                let old = self.head_files(z);
                self.transition(z, &old, &Files::new())?;
                self.view.subs.remove(z);
                self.push_op();
                Ok(Out::Done)
            }
        }
    }

    fn require_subscribed(&self, p: &str) -> R<()> {
        if !p.is_empty() && !self.subscribed(&self.owner(p)) {
            return Err(());
        }
        Ok(())
    }

    fn add(&mut self, spec: &str) -> R<()> {
        self.require_subscribed(spec)?;
        let is_dir = spec.is_empty() || self.wt.keys().any(|k| under(k, spec) && k != spec);
        let is_file = !spec.is_empty() && self.wt.contains_key(spec);
        if is_file {
            self.ix.insert(spec.to_owned(), self.wt[spec].clone());
        } else if is_dir {
            let seen: Vec<String> = self
                .wt
                .keys()
                .filter(|k| under(k, spec) && self.subscribed(&self.owner(k)))
                .cloned()
                .collect();
            for k in &seen {
                self.ix.insert(k.clone(), self.wt[k].clone());
            }
            let gone: Vec<String> = self.ix.keys().filter(|k| under(k, spec) && !seen.contains(k)).cloned().collect();
            for k in gone {
                self.ix.remove(&k);
            }
        } else {
            let gone: Vec<String> = self.ix.keys().filter(|k| under(k, spec)).cloned().collect();
            if gone.is_empty() {
                return Err(());
            }
            for k in gone {
                self.ix.remove(&k);
            }
        }
        Ok(())
    }

    fn commit(&mut self, serial: u64) -> R<()> {
        let subs: Vec<(String, String)> = self.view.subs.iter().map(|(z, b)| (z.clone(), b.clone())).collect();
        let mut any = false;
        for (z, b) in subs {
            let parent = self.view.branches.get(&(z.clone(), b.clone())).copied();
            let entries = self.ix_zone(&z);
            if parent.is_none() && entries.is_empty() {
                continue;
            }
            if parent.is_some() && entries == self.files_of(parent) {
                continue;
            }
            let c = self.mk_commit(MCommit {
                zone: z.clone(),
                parents: parent.into_iter().collect(),
                files: entries,
                serial,
            });
            self.view.branches.insert((z, b), c);
            any = true;
        }
        if !any {
            return Err(());
        }
        self.push_op();
        Ok(())
    }

    /// Per-path three-way merge of single-line files; `Err(paths)` lists the conflicting ones
    /// (the Ok/Err value both carry the merged entries with conflicts left at "ours").
    fn merge3(base: &Files, ours: &Files, theirs: &Files) -> (Files, Vec<String>) {
        let mut out = Files::new();
        let mut conflicts = vec![];
        let keys: BTreeSet<&String> = base.keys().chain(ours.keys()).chain(theirs.keys()).collect();
        for k in keys {
            let (b, o, t) = (base.get(k), ours.get(k), theirs.get(k));
            let r = if o == t {
                o
            } else if b == o {
                t
            } else if b == t {
                o
            } else {
                conflicts.push(k.clone());
                o.or(t)
            };
            if let Some(c) = r {
                out.insert(k.clone(), c.clone());
            }
        }
        (out, conflicts)
    }

    fn merge(&mut self, z: &str, b: &str, serial: u64) -> R<Out> {
        let Some(&theirs) = self.view.branches.get(&(z.to_owned(), b.to_owned())) else {
            return Err(());
        };
        let Some(cur) = self.view.subs.get(z).cloned() else {
            return Err(());
        };
        if self.dirty(z) {
            return Err(());
        }
        let ours = self.view.branches.get(&(z.to_owned(), cur.clone())).copied();
        let old = self.head_files(z);
        let Some(ours) = ours else {
            let new = self.files_of(Some(theirs));
            self.transition(z, &old, &new)?;
            self.view.branches.insert((z.to_owned(), cur), theirs);
            self.push_op();
            return Ok(Out::FastForward);
        };
        let bases = self.bases(ours, theirs);
        let base = match bases.as_slice() {
            [] => return Err(()),
            [x] if *x == theirs => return Ok(Out::UpToDate),
            [x] if *x == ours => {
                let new = self.files_of(Some(theirs));
                self.transition(z, &old, &new)?;
                self.view.branches.insert((z.to_owned(), cur), theirs);
                self.push_op();
                return Ok(Out::FastForward);
            }
            [x] => *x,
            _ => return Err(()),
        };
        let (merged, conflicts) = Self::merge3(&self.files_of(Some(base)), &old, &self.files_of(Some(theirs)));
        if conflicts.is_empty() {
            self.transition(z, &old, &merged)?;
            let c = self.mk_commit(MCommit {
                zone: z.to_owned(),
                parents: vec![ours, theirs],
                files: merged,
                serial,
            });
            self.view.branches.insert((z.to_owned(), cur), c);
            self.push_op();
            return Ok(Out::Merged);
        }
        // Conflicted: the placeholder state must be materializable, then the test aborts.
        let mut placeholder = merged;
        for p in &conflicts {
            placeholder.insert(p.clone(), "<<<conflict>>>\n".into());
        }
        let backup = self.clone();
        self.transition(z, &old, &placeholder)?;
        *self = backup;
        self.push_op(); // merge --abort
        Ok(Out::Conflict)
    }

    fn rebase(&mut self, z: &str, onto: &str) -> R<Out> {
        let Some(branch) = self.view.subs.get(z).cloned() else {
            return Err(());
        };
        if branch == onto {
            return Err(());
        }
        let Some(&onto_id) = self.view.branches.get(&(z.to_owned(), onto.to_owned())) else {
            return Err(());
        };
        if self.dirty(z) {
            return Err(());
        }
        let Some(&head) = self.view.branches.get(&(z.to_owned(), branch.clone())) else {
            return Err(());
        };
        let base = match self.bases(head, onto_id).as_slice() {
            [x] => *x,
            _ => return Err(()),
        };
        if base == onto_id {
            return Ok(Out::UpToDate);
        }
        let mut todo = vec![];
        let mut c = head;
        while c != base {
            if self.commits[c].parents.len() != 1 {
                return Err(());
            }
            todo.push(c);
            c = self.commits[c].parents[0];
        }
        todo.reverse();
        let old = self.head_files(z);
        let backup = self.clone();
        let mut cursor = onto_id;
        let mut cursor_files = self.files_of(Some(onto_id));
        for pick in todo {
            let pc = self.commits[pick].clone();
            let fb = self.files_of(Some(pc.parents[0]));
            let (merged, conflicts) = Self::merge3(&fb, &cursor_files, &pc.files);
            if !conflicts.is_empty() {
                let mut placeholder = merged;
                for p in &conflicts {
                    placeholder.insert(p.clone(), "<<<conflict>>>\n".into());
                }
                self.transition(z, &old, &placeholder)?;
                *self = backup; // rebase --abort restores everything and records nothing
                return Ok(Out::Conflict);
            }
            cursor = self.mk_commit(MCommit {
                zone: z.to_owned(),
                parents: vec![cursor],
                files: merged.clone(),
                serial: pc.serial,
            });
            cursor_files = merged;
        }
        self.transition(z, &old, &cursor_files)?;
        self.view.branches.insert((z.to_owned(), branch), cursor);
        self.push_op();
        Ok(Out::Done)
    }

    fn restore_view(&mut self, target: &View) -> R<()> {
        let new_head = |a: &str| {
            target
                .subs
                .get(a)
                .map(|b| (b.clone(), target.branches.get(&(a.to_owned(), b.clone())).copied()))
        };
        let mut affected = vec![];
        for a in self.view.subs.keys().chain(target.subs.keys()).collect::<BTreeSet<_>>() {
            let cur = self
                .view
                .subs
                .get(a)
                .map(|b| (b.clone(), self.view.branches.get(&(a.clone(), b.clone())).copied()));
            if cur != new_head(a) {
                affected.push(a.clone());
            }
        }
        for a in &affected {
            if self.subscribed(a) && self.dirty(a) {
                return Err(());
            }
        }
        for a in &affected {
            let old = if self.subscribed(a) { self.head_files(a) } else { Files::new() };
            let new = match new_head(a) {
                Some((_, Some(h))) => self.files_of(Some(h)),
                _ => Files::new(),
            };
            self.transition(a, &old, &new)?;
        }
        self.view = target.clone();
        Ok(())
    }
}

// ------------------------------------------------------------------ harness

fn sig(n: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: n,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RealOut {
    Ok(Out),
    Err(String),
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for e in fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let (s, d) = (e.path(), dst.join(e.file_name()));
        if e.file_type().unwrap().is_dir() {
            copy_dir(&s, &d);
        } else {
            fs::copy(&s, &d).unwrap();
        }
    }
}

fn prune_empty_parents(root: &Path, mut dir: PathBuf) {
    while dir != root && fs::remove_dir(&dir).is_ok() {
        dir.pop();
    }
}

/// Run `op` against a real repository. `Write`/`Delete` only touch files.
fn apply_real(repo: &Repo, op: &Op, serial: u64, sub_ok: bool, op_ids: &[ObjectId]) -> RealOut {
    let e = |r: Result<(), vcs_repo::Error>| match r {
        Ok(()) => RealOut::Ok(Out::Done),
        Err(x) => RealOut::Err(x.to_string()),
    };
    let sp = |s: &str| vec![s.to_owned()];
    match op {
        Op::Write(p, v) => {
            if sub_ok {
                let abs = repo.worktree().join(p);
                fs::create_dir_all(abs.parent().unwrap()).unwrap();
                fs::write(abs, content(*v)).unwrap();
            }
            RealOut::Ok(Out::Done)
        }
        Op::Delete(p) => {
            if sub_ok {
                let abs = repo.worktree().join(p);
                if fs::remove_file(&abs).is_ok() {
                    prune_empty_parents(repo.worktree(), abs.parent().unwrap().to_owned());
                }
            }
            RealOut::Ok(Out::Done)
        }
        Op::Add(s) => e(repo.add(&sp(s)).map(|_| ())),
        Op::Commit => e(repo.commit(&format!("m{serial}"), sig(serial)).map(|_| ())),
        Op::Branch(z, b) => e(repo.create_branch(z, b).map(|_| ())),
        Op::Checkout(z, b) => e(repo.checkout(z, b)),
        Op::Merge(z, b) => match repo.merge(z, b, sig(serial)) {
            Ok(MergeOutcome::UpToDate) => RealOut::Ok(Out::UpToDate),
            Ok(MergeOutcome::FastForward { .. }) => RealOut::Ok(Out::FastForward),
            Ok(MergeOutcome::Merged { .. }) => RealOut::Ok(Out::Merged),
            Ok(MergeOutcome::Conflicted { .. }) => match repo.merge_abort() {
                Ok(()) => RealOut::Ok(Out::Conflict),
                Err(x) => RealOut::Err(format!("merge --abort failed: {x}")),
            },
            Err(x) => RealOut::Err(x.to_string()),
        },
        Op::Rebase(z, b) => match repo.rebase(z, b, sig(serial)) {
            Ok(RebaseOutcome::UpToDate) => RealOut::Ok(Out::UpToDate),
            Ok(RebaseOutcome::Done { .. }) => RealOut::Ok(Out::Done),
            Ok(RebaseOutcome::Conflicted { .. }) => match repo.rebase_abort() {
                Ok(()) => RealOut::Ok(Out::Conflict),
                Err(x) => RealOut::Err(format!("rebase --abort failed: {x}")),
            },
            Err(x) => RealOut::Err(x.to_string()),
        },
        Op::Restore(s) => e(repo.restore(&sp(s), false).map(|_| ())),
        Op::Unstage(s) => e(repo.restore(&sp(s), true).map(|_| ())),
        Op::Rm { cached, force, spec } => e(repo.rm(&sp(spec), *cached, *force).map(|_| ())),
        Op::Undo => e(repo.op_undo().map(|_| ())),
        Op::GoTo(n) => e(repo.op_restore(&op_ids[n % op_ids.len()]).map(|_| ())),
        Op::ZoneCreate(n, p) => e(repo.create_zone(n, p)),
        Op::ZoneRemove(z) => e(repo.zone_remove(z)),
        Op::ZoneAdd(z, b) => e(repo.zone_add(z, Some(b))),
        Op::Gc => {
            // Pretend 60 days passed so every unreachable object is past the 14-day grace.
            let now = std::time::SystemTime::now() + std::time::Duration::from_secs(60 * 86400);
            let orphan = repo
                .store()
                .put_blob(format!("orphan {serial}").as_bytes())
                .and_then(|_| repo.store().sync());
            if let Err(x) = orphan {
                return RealOut::Err(x.to_string());
            }
            if serial.is_multiple_of(2) {
                if let Err(x) = repo.repack() {
                    return RealOut::Err(x.to_string());
                }
            }
            match repo.gc_sweep_at(now, vcs_repo::GcOptions::new(std::time::Duration::from_secs(14 * 86400))) {
                Ok(run) => {
                    GC_DELETED.fetch_add(run.deleted.len(), std::sync::atomic::Ordering::Relaxed);
                    RealOut::Ok(Out::Done)
                }
                Err(x) => RealOut::Err(x.to_string()),
            }
        }
    }
}

/// Objects deleted by GC steps across the whole test binary (proves the steps are not vacuous).
static GC_DELETED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Independent reachability walk for the "nothing reachable is ever deleted" check: every
/// model-bound commit with its parents and trees, every file blob, and every logged operation.
fn must_survive(h: &Harness) -> BTreeSet<ObjectId> {
    use vcs_objects::Object;
    let mut seen = BTreeSet::new();
    let mut stack: Vec<ObjectId> = h.oid.values().copied().chain(h.op_ids.iter().copied()).collect();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        match h.repo.store().get(&id).unwrap_or_else(|e| panic!("reachable {id} unreadable: {e}")) {
            Object::Commit(c) => {
                stack.extend(c.parents.iter().copied());
                stack.push(c.root);
            }
            Object::Tree(t) => stack.extend(t.entries().iter().map(|e| e.id)),
            Object::Chunked(m) => stack.extend(m.chunks.iter().map(|c| c.id)),
            Object::Operation(op) => {
                stack.extend(op.parents.iter().copied());
                stack.extend(op.view.branches.values().copied());
            }
            _ => {}
        }
    }
    seen
}

fn op_chain_ids(repo: &Repo) -> Vec<ObjectId> {
    repo.op_log(usize::MAX).unwrap().into_iter().map(|(i, _)| i).collect()
}

struct Harness {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: Repo,
    model: Model,
    serial: u64,
    /// model commit index -> real id, and back
    oid: HashMap<usize, ObjectId>,
    by_oid: HashMap<ObjectId, usize>,
    /// real id of each model operation
    op_ids: Vec<ObjectId>,
    crashes: bool,
    crash_trials: usize,
}

fn flat_of(repo: &Repo, oid: &ObjectId) -> (String, Vec<ObjectId>, Files) {
    let vcs_objects::Object::Commit(c) = repo.store().get(oid).unwrap() else {
        panic!("not a commit")
    };
    let prefix = repo.zones().unwrap().path_of(&c.zone).unwrap_or("").to_owned();
    let flat = treeops::flatten(repo.store(), Some(c.root), &prefix).unwrap();
    let files = flat
        .into_iter()
        .map(|(p, (_, id))| (p, String::from_utf8(repo.store().get_blob(&id).unwrap()).unwrap()))
        .collect();
    (c.zone.clone(), c.parents.clone(), files)
}

impl Harness {
    fn new(crashes: bool) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("r");
        let repo = Repo::init(&root).unwrap();
        let first = repo.op_head().unwrap().unwrap();
        Harness {
            _dir: dir,
            root,
            repo,
            model: Model::new(),
            serial: 0,
            oid: HashMap::new(),
            by_oid: HashMap::new(),
            op_ids: vec![first],
            crashes,
            crash_trials: 0,
        }
    }

    /// Bind model commit `idx` to real commit `oid`, verifying content and ancestry recursively.
    fn bind(&mut self, idx: usize, oid: ObjectId) -> Result<(), String> {
        if let Some(o) = self.oid.get(&idx) {
            return if *o == oid {
                Ok(())
            } else {
                Err(format!("model commit {idx} is {o} but repo says {oid}"))
            };
        }
        if let Some(j) = self.by_oid.get(&oid) {
            return Err(format!("real commit {oid} already bound to model commit {j}, not {idx}"));
        }
        let mc = self.model.commits[idx].clone();
        let (zone, parents, files) = flat_of(&self.repo, &oid);
        if zone != mc.zone {
            return Err(format!("commit {oid}: zone {zone} != model {}", mc.zone));
        }
        if files != mc.files {
            return Err(format!("commit {oid} ({zone}): files {files:?} != model {:?}", mc.files));
        }
        if parents.len() != mc.parents.len() {
            return Err(format!("commit {oid}: {} parents != model {}", parents.len(), mc.parents.len()));
        }
        self.oid.insert(idx, oid);
        self.by_oid.insert(oid, idx);
        for (mp, rp) in mc.parents.iter().zip(parents) {
            self.bind(*mp, rp)?;
        }
        Ok(())
    }

    fn check_state(&mut self) -> Result<(), String> {
        let repo = &self.repo;
        // zones, subscriptions
        let zones = repo.zones().map_err(|e| e.to_string())?;
        let real_zones: BTreeMap<String, String> = zones
            .names()
            .filter(|n| *n != ROOT_ZONE)
            .map(|n| (n.to_owned(), zones.path_of(n).unwrap().to_owned()))
            .collect();
        if real_zones != self.model.zones {
            return Err(format!("zones {real_zones:?} != model {:?}", self.model.zones));
        }
        let ws = repo.workspace().map_err(|e| e.to_string())?;
        if ws.subs != self.model.view.subs {
            return Err(format!("subscriptions {:?} != model {:?}", ws.subs, self.model.view.subs));
        }
        // refs / HEAD
        let refs = repo.refs().map_err(|e| e.to_string())?;
        let keys: BTreeSet<_> = refs.branches.keys().cloned().collect();
        let mkeys: BTreeSet<_> = self.model.view.branches.keys().cloned().collect();
        if keys != mkeys {
            return Err(format!("branch set {keys:?} != model {mkeys:?}"));
        }
        let heads: Vec<((String, String), usize)> = self.model.view.branches.iter().map(|(k, v)| (k.clone(), *v)).collect();
        for (k, idx) in heads {
            self.bind(idx, refs.branches[&k]).map_err(|e| format!("{}/{}: {e}", k.0, k.1))?;
        }
        let repo = &self.repo;
        // worktree
        let mut disk = Files::new();
        let mut dirs = BTreeSet::new();
        walk(&self.root, &self.root, &mut disk, &mut dirs);
        if disk != self.model.wt {
            return Err(format!("worktree {disk:?} != model {:?}", self.model.wt));
        }
        let want_dirs: BTreeSet<String> = disk
            .keys()
            .flat_map(|p| {
                let mut v = vec![];
                let mut cur = p.as_str();
                while let Some((d, _)) = cur.rsplit_once('/') {
                    v.push(d.to_owned());
                    cur = d;
                }
                v
            })
            .collect();
        if dirs != want_dirs {
            return Err(format!("directories {dirs:?} != expected {want_dirs:?}"));
        }
        // index (through status)
        let st = repo.status().map_err(|e| e.to_string())?;
        let szones: BTreeSet<&String> = st.zones.iter().map(|z| &z.zone).collect();
        let mzones: BTreeSet<&String> = self.model.view.subs.keys().collect();
        if szones != mzones {
            return Err(format!("status zones {szones:?} != model {mzones:?}"));
        }
        let kind = |k: ChangeKind| match k {
            ChangeKind::Added => "added",
            ChangeKind::Modified => "modified",
            ChangeKind::Deleted => "deleted",
        };
        for z in &st.zones {
            let staged: BTreeSet<(String, &str)> = z.staged.iter().map(|c| (c.path.clone(), kind(c.kind))).collect();
            let unstaged: BTreeSet<(String, &str)> = z.unstaged.iter().map(|c| (c.path.clone(), kind(c.kind))).collect();
            if staged != self.model.staged(&z.zone) {
                return Err(format!(
                    "zone {} staged {staged:?} != model {:?}",
                    z.zone,
                    self.model.staged(&z.zone)
                ));
            }
            if unstaged != self.model.unstaged(&z.zone) {
                return Err(format!(
                    "zone {} unstaged {unstaged:?} != model {:?}",
                    z.zone,
                    self.model.unstaged(&z.zone)
                ));
            }
            let want_branch = &self.model.view.subs[&z.zone];
            if &z.branch != want_branch {
                return Err(format!("zone {} on branch {} != model {want_branch}", z.zone, z.branch));
            }
        }
        let want_untracked: BTreeSet<&String> = self
            .model
            .wt
            .keys()
            .filter(|p| !self.model.ix.contains_key(*p) && self.model.subscribed(&self.model.owner(p)))
            .collect();
        let mut covered = BTreeSet::new();
        for u in &st.untracked {
            if let Some(dir) = u.strip_suffix('/') {
                let hit: Vec<&&String> = want_untracked.iter().filter(|p| under(p, dir)).collect();
                if hit.is_empty() {
                    return Err(format!("untracked dir {u} has no untracked files in the model"));
                }
                covered.extend(hit.into_iter().map(|p| (**p).clone()));
            } else {
                if !want_untracked.contains(u) {
                    return Err(format!("untracked {u} is not untracked in the model"));
                }
                covered.insert(u.clone());
            }
        }
        let want: BTreeSet<String> = want_untracked.into_iter().cloned().collect();
        if covered != want {
            return Err(format!("untracked {:?} != model {want:?}", st.untracked));
        }
        // operation log
        let log = op_chain_ids(repo);
        let mut chain = vec![];
        let mut cur = Some(self.model.head);
        while let Some(i) = cur {
            chain.push(self.op_ids[i]);
            cur = self.model.ops[i].0;
        }
        if log != chain {
            return Err(format!("operation log has {} entries, model chain {}", log.len(), chain.len()));
        }
        let view = repo.current_view().map_err(|e| e.to_string())?;
        let op = repo.op_get(&log[0]).map_err(|e| e.to_string())?;
        if op.view != view {
            return Err("operation head's view differs from the on-disk view".into());
        }
        let fsck = repo.fsck().map_err(|e| e.to_string())?;
        if !fsck.problems.is_empty() {
            return Err(format!("fsck: {:?}", fsck.problems));
        }
        Ok(())
    }

    fn step(&mut self, i: usize, op: &Op) -> Result<(), String> {
        self.serial += 1;
        let serial = self.serial;
        let pre = self.model.clone();
        let sub_ok = match op {
            Op::Write(p, _) | Op::Delete(p) => self.model.subscribed(&self.model.owner(p)),
            _ => true,
        };
        let trial = self.crashes && op.crash_candidate() && i % 3 < 1;
        let snap = if trial {
            let d = tempfile::tempdir().unwrap();
            copy_dir(&self.root, &d.path().join("r"));
            Some(d)
        } else {
            None
        };
        let survivors = if matches!(op, Op::Gc) { Some(must_survive(self)) } else { None };
        vcs_repo::failpoint::reset();
        let real = apply_real(&self.repo, op, serial, sub_ok, &self.op_ids);
        let writes = vcs_repo::failpoint::writes();
        let mut m2 = self.model.clone();
        let exp = m2.apply(op, serial);
        match (&real, &exp) {
            (RealOut::Ok(r), Ok(m)) if r == m => {}
            (RealOut::Err(_), Err(())) => {}
            _ => return Err(format!("step {i} `{}`: real {real:?} but model {exp:?}", op.line())),
        }
        if exp.is_ok() {
            self.model = m2;
        }
        // the operation log moves exactly when the model says an operation was recorded
        let head = self.repo.op_head().unwrap().unwrap();
        if self.model.ops.len() > self.op_ids.len() {
            if head == self.op_ids[pre.head] {
                return Err(format!("step {i} `{}`: model recorded an operation the repo did not", op.line()));
            }
            self.op_ids.push(head);
        } else if head != self.op_ids[self.model.head] {
            return Err(format!("step {i} `{}`: repo recorded an operation the model did not", op.line()));
        }
        if let Some(keep) = survivors {
            for id in keep {
                if !self.repo.store().contains(&id).map_err(|e| e.to_string())? {
                    return Err(format!("step {i} `gc`: reachable object {id} was deleted"));
                }
            }
            let rep = self.repo.fsck().map_err(|e| e.to_string())?;
            if !rep.problems.is_empty() {
                return Err(format!("step {i} `gc`: fsck after gc: {:?}", rep.problems));
            }
        }
        self.check_state().map_err(|e| format!("step {i} `{}`: {e}", op.line()))?;
        if let Some(snap) = snap {
            let conflict_involved = matches!(exp, Ok(Out::Conflict));
            if writes > 0 && !conflict_involved {
                self.crash_trials += self.crash_trial(i, op, serial, snap.path(), writes, &pre)?;
            }
        }
        Ok(())
    }

    fn crash_trial(&mut self, i: usize, op: &Op, serial: u64, snap: &Path, writes: usize, pre: &Model) -> Result<usize, String> {
        let post = self.model.clone();
        let pre_ops = pre.ops.len();
        let recorded = post.ops.len() > pre_ops;
        for n in 0..writes {
            let d = tempfile::tempdir().unwrap();
            let root = d.path().join("r");
            copy_dir(&snap.join("r"), &root);
            {
                let repo = Repo::discover(&root).unwrap();
                vcs_repo::failpoint::reset();
                vcs_repo::failpoint::arm(Some(n));
                let res = apply_real(&repo, op, serial, true, &self.op_ids[..pre_ops]);
                vcs_repo::failpoint::arm(None);
                match res {
                    RealOut::Err(m) if m.contains("injected crash") => {}
                    other => {
                        return Err(format!(
                            "step {i} `{}`: crash at write {n}/{writes} was not reported: {other:?}",
                            op.line()
                        ))
                    }
                }
            }
            // "reboot": a fresh process opens the directory
            let repo = Repo::discover(&root).map_err(|e| format!("step {i} crash {n}: reopen: {e}"))?;
            let fail = |m: String| format!("step {i} `{}` crash at write {n}/{writes}: {m}", op.line());
            let rep = repo.fsck().map_err(|e| fail(format!("fsck: {e}")))?;
            if !rep.problems.is_empty() {
                return Err(fail(format!("fsck problems {:?}", rep.problems)));
            }
            repo.status().map_err(|e| fail(format!("status: {e}")))?;
            let log = repo.op_log(usize::MAX).map_err(|e| fail(format!("op log: {e}")))?;
            let zones = repo.zones().map_err(|e| fail(e.to_string()))?;
            let rzones: BTreeMap<String, String> = zones
                .names()
                .filter(|x| *x != ROOT_ZONE)
                .map(|x| (x.to_owned(), zones.path_of(x).unwrap().to_owned()))
                .collect();
            let subs = repo.workspace().map_err(|e| fail(e.to_string()))?.subs;
            let refs = repo.refs().map_err(|e| fail(e.to_string()))?;
            // describe refs by content so crashed copies (fresh change ids) compare to the model
            let real_heads: BTreeMap<(String, String), (String, usize, Files)> = refs
                .branches
                .iter()
                .map(|(k, o)| {
                    let (z, p, f) = flat_of(&repo, o);
                    (k.clone(), (z, p.len(), f))
                })
                .collect();
            let describe = |m: &Model| -> BTreeMap<(String, String), (String, usize, Files)> {
                m.view
                    .branches
                    .iter()
                    .map(|(k, c)| {
                        (
                            k.clone(),
                            (m.commits[*c].zone.clone(), m.commits[*c].parents.len(), m.commits[*c].files.clone()),
                        )
                    })
                    .collect()
            };
            let all_post = describe(&post) == real_heads && post.view.subs == subs && post.zones == rzones;
            let refs_ok = real_heads == describe(pre) || real_heads == describe(&post);
            let subs_ok = subs == pre.view.subs || subs == post.view.subs;
            let zones_ok = rzones == pre.zones || rzones == post.zones;
            if !(refs_ok && subs_ok && zones_ok) {
                return Err(fail(format!(
                    "state is neither before nor after the operation (refs ok {refs_ok}, subs ok {subs_ok}, zones ok {zones_ok}): refs {real_heads:?}, subs {subs:?}, zones {rzones:?}"
                )));
            }
            if recorded {
                if log.len() == post.ops_chain_len() && !all_post {
                    return Err(fail("operation was recorded but its effects are not all in place".into()));
                }
                if log.len() != pre.ops_chain_len() && log.len() != post.ops_chain_len() {
                    return Err(fail(format!(
                        "op log length {} is neither {} nor {}",
                        log.len(),
                        pre.ops_chain_len(),
                        post.ops_chain_len()
                    )));
                }
            } else if log.len() != pre.ops_chain_len() {
                return Err(fail("op log changed although the operation records nothing".into()));
            }
            repo.add(&[String::new()]).map_err(|e| fail(format!("add after crash: {e}")))?;
        }
        Ok(writes)
    }
}

impl Model {
    fn ops_chain_len(&self) -> usize {
        let mut n = 0;
        let mut cur = Some(self.head);
        while let Some(i) = cur {
            n += 1;
            cur = self.ops[i].0;
        }
        n
    }
}

fn walk(root: &Path, dir: &Path, files: &mut Files, dirs: &mut BTreeSet<String>) {
    for e in fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        if rel == ".vcs" {
            continue;
        }
        if e.file_type().unwrap().is_dir() {
            dirs.insert(rel);
            walk(root, &p, files, dirs);
        } else {
            files.insert(rel, fs::read_to_string(&p).unwrap());
        }
    }
}

/// Run a script; `Err` describes the first divergence.
fn run_script(ops: &[Op], crashes: bool) -> Result<usize, String> {
    let mut h = Harness::new(crashes);
    for (i, op) in ops.iter().enumerate() {
        h.step(i, op)?;
    }
    Ok(h.crash_trials)
}

/// Delta-debugging shrink: drop chunks of the script while it still fails.
fn shrink(mut ops: Vec<Op>, crashes: bool) -> Vec<Op> {
    let mut chunk = ops.len() / 2;
    while chunk >= 1 {
        let mut i = 0;
        while i < ops.len() {
            let mut cand = ops.clone();
            cand.drain(i..(i + chunk).min(ops.len()));
            if run_script(&cand, crashes).is_err() {
                ops = cand;
            } else {
                i += chunk;
            }
        }
        chunk /= 2;
    }
    ops
}

fn check(name: &str, ops: Vec<Op>, crashes: bool) -> usize {
    match run_script(&ops, crashes) {
        Ok(n) => n,
        Err(first) => {
            let small = shrink(ops, crashes);
            let err = run_script(&small, crashes).unwrap_err();
            let script: Vec<String> = small.iter().map(Op::line).collect();
            panic!(
                "{name}: model divergence\n  first failure: {first}\n  shrunk to {} steps: {err}\n--- replay script (save as tests/model_seeds/<name>.script) ---\n{}\n---",
                small.len(),
                script.join("\n")
            );
        }
    }
}

fn seeds_file() -> Vec<(u64, usize)> {
    let text = include_str!("model_seeds/seeds.txt");
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let mut it = l.split_whitespace();
            (it.next().unwrap().parse().unwrap(), it.next().unwrap().parse().unwrap())
        })
        .collect()
}

#[test]
fn fixed_seeds_match_the_model() {
    let mut trials = 0;
    for (seed, steps) in seeds_file() {
        trials += check(&format!("seed {seed}"), generate(seed, steps), true);
    }
    assert!(trials > 0, "no crash points were exercised");
    let deleted = GC_DELETED.load(std::sync::atomic::Ordering::Relaxed);
    assert!(deleted > 0, "gc steps never deleted anything: the gc check is vacuous");
    eprintln!("crash points exercised: {trials}; objects deleted by gc steps: {deleted}");
}

#[test]
fn checked_in_scripts_replay() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/model_seeds");
    let mut n = 0;
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "script") {
            let ops = parse_script(&fs::read_to_string(&p).unwrap());
            check(&p.display().to_string(), ops, true);
            n += 1;
        }
    }
    eprintln!("replayed {n} scripts");
}

#[test]
fn script_format_round_trips() {
    for seed in 0..20 {
        for op in generate(seed, 200) {
            assert_eq!(Op::parse(&op.line()), Some(op.clone()), "{}", op.line());
        }
    }
}

/// Longer exploration: `MODEL_SEEDS` (default 300) seeds of `MODEL_STEPS` (default 200) steps,
/// starting at `MODEL_FROM` (default 1000).
#[test]
#[ignore = "long randomized run; see the module docs"]
fn long_random_runs_match_the_model() {
    let env = |k: &str, d: u64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let (n, steps, from) = (env("MODEL_SEEDS", 300), env("MODEL_STEPS", 200) as usize, env("MODEL_FROM", 1000));
    for seed in from..from + n {
        check(&format!("seed {seed}"), generate(seed, steps), true);
    }
}
