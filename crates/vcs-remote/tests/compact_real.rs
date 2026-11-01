//! Tiered remote compaction (`vcs_remote::compact`) against the REAL publisher, readers and
//! sweeper: exhaustive interleavings, crash-at-every-step, and mutation checks.
//!
//! World: three packs from three publishes (branch `main`: c1 <- c2 <- c3). P1 and P2 are small
//! (one tier) and P3 is bigger, so the plan merges exactly P1 + P2. Compactor phases are the
//! hook points of `compact_with` (Planned, PackPut, IdxPut, Verified, BeforeDelete x4, Done).
//!
//! * Readers: the compactor runs on its own thread and is advanced one phase at a time; the
//!   reader's k-th backend request is preceded by `adv[k]` phases, for EVERY non-decreasing `adv`.
//! * Publishers: a second publisher's step machine is advanced from the compactor's hook, again
//!   for every non-decreasing schedule.
//! * Invariants checked at every phase: I-IDX (every index has its pack) and I-LIVE (every object
//!   of the head's closure is in some index whose pack exists); readers must return the exact
//!   bytes; after a publisher the head's closure is complete (G-REACH).
//! * Mutations (`CompactQuirks`, and a reader with no retry): each must be caught by the same search.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc;
use std::sync::Mutex;
use vcs_objects::*;
use vcs_remote::backend::{Backend, BackendError, Versioned};
use vcs_remote::compact::{compact_with, plan_tiers, CompactOptions, CompactPoint, CompactQuirks, PackInfo};
use vcs_remote::gc::{remote_gc, GcError, RemoteGcOptions};
use vcs_remote::packfetch::{fetch_from_any_pack_retrying, fetch_many_retrying, ReadAhead, RemoteSource};
use vcs_remote::*;
use vcs_store::pack::{build_pack_fmt, PackFormat, PackIndex};
use vcs_store::{MemoryStore, ObjectStore};

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

/// Deterministic incompressible bytes.
fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

struct World {
    store: MemoryStore,
    commits: Vec<ObjectId>,
    /// Every object of the closure of the last commit, with its encoding.
    all: Vec<(ObjectId, Vec<u8>)>,
    /// The backend as three publishes left it.
    snapshot: Vec<(String, Vec<u8>)>,
}

fn commit_over(s: &MemoryStore, files: &[(&str, Vec<u8>)], parent: Option<ObjectId>, tag: u8) -> ObjectId {
    let entries: Vec<TreeEntry> = files
        .iter()
        .map(|(n, c)| TreeEntry {
            name: (*n).into(),
            mode: EntryMode::File,
            id: s.put_blob(c).unwrap(),
        })
        .collect();
    let t = s.put(&Object::Tree(Tree::new(entries).unwrap())).unwrap();
    s.put(&Object::Commit(Commit {
        zone: "z".into(),
        parents: parent.into_iter().collect(),
        root: t,
        change_id: ChangeId::from_bytes([tag; 16]),
        author: sig(),
        message: format!("c{tag}"),
    }))
    .unwrap()
}

fn tx(old: Option<ObjectId>, new: ObjectId) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: "z".into(),
            branch: "main".into(),
            old,
            new,
        }],
        author: sig(),
        message: "m".into(),
        lock_ops: vec![],
    }
}

fn closure(s: &dyn ObjectStore, head: ObjectId) -> Vec<(ObjectId, Vec<u8>)> {
    let mut seen = HashSet::new();
    let mut stack = vec![head];
    let mut out = vec![];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let o = s.get(&id).unwrap();
        match &o {
            Object::Commit(c) => stack.extend(c.parents.iter().copied().chain([c.root])),
            Object::Tree(t) => stack.extend(t.entries().iter().map(|e| e.id)),
            _ => {}
        }
        out.push((id, o.encode()));
    }
    out
}

fn world() -> World {
    let store = MemoryStore::new();
    let c1 = commit_over(&store, &[("f", noise(1, 120))], None, 1);
    let c2 = commit_over(&store, &[("f", noise(2, 120)), ("g", noise(3, 60))], Some(c1), 2);
    let c3 = commit_over(&store, &[("f", noise(4, 120)), ("big", noise(5, 3000))], Some(c2), 3);
    let b = MemBackend::new().with_gc();
    b.set_now_ms(1_000_000);
    let mut old = None;
    for c in [c1, c2, c3] {
        Publisher::new(&b, &store, tx(old, c)).run().unwrap();
        old = Some(c);
    }
    let snapshot = b
        .keys()
        .into_iter()
        .map(|k| {
            let d = b.get(&k).unwrap().unwrap().data;
            (k, d)
        })
        .collect();
    let all = closure(&store, c3);
    World {
        store,
        commits: vec![c1, c2, c3],
        all,
        snapshot,
    }
}

fn fresh(w: &World) -> MemBackend {
    let b = MemBackend::new().with_gc();
    b.set_now_ms(1_000_000);
    for (k, d) in &w.snapshot {
        assert!(b.put_if_absent(k, d).unwrap());
    }
    b
}

fn opts() -> CompactOptions {
    CompactOptions {
        tier_ratio: 2,
        tier_base: 1024,
        dry_run: false,
        owner: "compactor".into(),
        ..Default::default()
    }
}

fn pack_stems(b: &dyn Backend) -> Vec<String> {
    b.list("packs/")
        .unwrap()
        .into_iter()
        .filter_map(|k| k.strip_suffix(".idx").map(str::to_owned))
        .collect()
}

/// I-IDX and I-LIVE.
fn invariants(b: &dyn Backend, ids: &[ObjectId]) -> Result<(), String> {
    let mut idxs = vec![];
    for k in b.list("packs/").unwrap() {
        let Some(stem) = k.strip_suffix(".idx") else { continue };
        if !b.exists(&format!("{stem}.pack")).unwrap() {
            return Err(format!("I-IDX: {stem}.idx exists but its pack does not"));
        }
        let v = b.get(&k).unwrap().ok_or("index vanished")?;
        idxs.push(PackIndex::parse(&v.data, Path::new(stem)).unwrap());
    }
    for id in ids {
        if !idxs.iter().any(|i| i.find(id).is_some()) {
            return Err(format!("I-LIVE: {} is in no complete pack", id.short()));
        }
    }
    Ok(())
}

fn g_reach(b: &dyn Backend) -> Result<(), String> {
    let (m, _) = load_manifest(b).map_err(|e| e.to_string())?;
    let mut seen = HashSet::new();
    let mut stack: Vec<ObjectId> = m.branches.values().copied().collect();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let o = fetch_object(b, &id).map_err(|e| format!("G-REACH: head needs {} which is gone ({e})", id.short()))?;
        match &o {
            Object::Commit(c) => stack.extend(c.parents.iter().copied().chain([c.root])),
            Object::Tree(t) => stack.extend(t.entries().iter().map(|e| e.id)),
            _ => {}
        }
    }
    Ok(())
}

fn ids_of(all: &[(ObjectId, Vec<u8>)]) -> Vec<ObjectId> {
    all.iter().map(|o| o.0).collect()
}

// ---------------------------------------------------------------- plain behaviour

#[test]
fn compaction_merges_the_two_small_packs_and_keeps_everything() {
    let w = world();
    let b = fresh(&w);
    let before = pack_stems(&b);
    assert_eq!(before.len(), 3);
    let infos: Vec<PackInfo> = vcs_remote::compact::load_packs(&b).unwrap().into_iter().map(|p| p.0).collect();
    let plan = plan_tiers(&infos, 2, 1024, u64::MAX);
    assert_eq!(plan.len(), 1, "{infos:?}");
    assert_eq!(plan[0].len(), 2);

    let rep = vcs_remote::compact::compact(&b, &opts()).unwrap();
    assert_eq!((rep.packs_before, rep.packs_after), (3, 2));
    assert_eq!(rep.merges.len(), 1);
    assert_eq!(rep.deleted_packs.len(), 2);
    let after = pack_stems(&b);
    assert_eq!(after.len(), 2);
    for d in &rep.deleted_packs {
        assert!(!b.exists(&format!("{d}.idx")).unwrap() && !b.exists(&format!("{d}.pack")).unwrap());
    }
    invariants(&b, &ids_of(&w.all)).unwrap();
    g_reach(&b).unwrap();
    let got = fetch_many(&b, &ids_of(&w.all)).unwrap();
    let got: HashMap<_, _> = got.into_iter().collect();
    for (id, enc) in &w.all {
        assert_eq!(&got[id], enc);
    }
    // running again: nothing left to merge
    let again = vcs_remote::compact::compact(&b, &opts()).unwrap();
    assert!(again.merges.is_empty());
    assert_eq!(pack_stems(&b).len(), 2);
    // the gc lock is released
    assert!(!b.exists("gc/lock").unwrap());
}

#[test]
fn dry_run_writes_nothing_and_reports_the_plan() {
    let w = world();
    let b = fresh(&w);
    let keys = b.keys();
    let mut o = opts();
    o.dry_run = true;
    let rep = vcs_remote::compact::compact(&b, &o).unwrap();
    assert_eq!((rep.packs_before, rep.packs_after, rep.merges.len()), (3, 2, 1));
    assert_eq!(b.keys(), keys);
}

#[test]
fn rollback_flag_writes_a_v1_pack_and_v1_inputs_are_read() {
    let w = world();
    let b = fresh(&w);
    let mut o = opts();
    o.format = PackFormat::V1;
    vcs_remote::compact::compact(&b, &o).unwrap();
    let mut versions: Vec<u8> = vec![];
    for s in pack_stems(&b) {
        let v = b.get(&format!("{s}.idx")).unwrap().unwrap();
        versions.push(PackIndex::parse(&v.data, Path::new(&s)).unwrap().version());
    }
    versions.sort();
    // the untouched pack is whatever the publisher wrote (v2); the merged one is v1
    assert_eq!(versions.first(), Some(&1));
    // now merge a v1 pack with the v2 one into a v2 pack
    let mut o2 = opts();
    o2.all = true;
    o2.format = PackFormat::V2;
    vcs_remote::compact::compact(&b, &o2).unwrap();
    assert_eq!(pack_stems(&b).len(), 1);
    invariants(&b, &ids_of(&w.all)).unwrap();
    let got = fetch_many(&b, &ids_of(&w.all)).unwrap();
    assert_eq!(got.len(), w.all.len());
}

#[test]
fn compacted_pack_has_commits_then_trees_then_files() {
    let w = world();
    let b = fresh(&w);
    let mut o = opts();
    o.format = PackFormat::V2; // v2 entries have one offset each, so file order is observable

    o.all = true;
    vcs_remote::compact::compact(&b, &o).unwrap();
    let stem = pack_stems(&b).remove(0);
    let idx = PackIndex::parse(&b.get(&format!("{stem}.idx")).unwrap().unwrap().data, Path::new(&stem)).unwrap();
    let mut ents: Vec<(u64, ObjectId)> = idx.iter().map(|e| (e.1, e.0)).collect();
    ents.sort();
    let kinds: Vec<u8> = ents.iter().map(|(_, id)| w.all.iter().find(|o| o.0 == *id).unwrap().1[0]).collect();
    // kind bytes: 3 commit, 2 tree, 1 blob
    let mut want = kinds.clone();
    want.sort_by_key(|k| match k {
        3 => 0,
        2 => 1,
        _ => 2,
    });
    assert_eq!(kinds, want, "commits, then trees, then blobs");
    // the commits are in parent order
    let cpos: Vec<usize> = w.commits.iter().map(|c| ents.iter().position(|e| e.1 == *c).unwrap()).collect();
    assert!(cpos[0] < cpos[1] && cpos[1] < cpos[2]);
}

#[test]
fn a_gc_sweep_is_refused_while_a_compaction_runs() {
    let w = world();
    let b = fresh(&w);
    let mut seen_locked = false;
    let mut hook = |p: CompactPoint| {
        if p == CompactPoint::PackPut {
            let g = RemoteGcOptions {
                dry_run: false,
                owner: "sweeper".into(),
                ..Default::default()
            };
            seen_locked = matches!(remote_gc(&b, &g), Err(GcError::Locked(_)));
        }
    };
    compact_with(&b, &opts(), CompactQuirks::default(), &mut hook).unwrap();
    assert!(seen_locked, "the sweeper must not run while the compactor holds gc/lock");
    // and a second compactor
    let mut second_locked = false;
    let b2 = fresh(&w);
    let mut hook2 = |p: CompactPoint| {
        if p == CompactPoint::PackPut {
            second_locked = matches!(vcs_remote::compact::compact(&b2, &opts()), Err(GcError::Locked(_)));
        }
    };
    compact_with(&b2, &opts(), CompactQuirks::default(), &mut hook2).unwrap();
    assert!(second_locked);
}

// ---------------------------------------------------------------- crash at every step

/// Fails every mutating call after the first `n`.
struct FailAfter<'a> {
    inner: &'a MemBackend,
    left: Mutex<usize>,
}
impl FailAfter<'_> {
    fn spend(&self) -> Result<(), BackendError> {
        let mut l = self.left.lock().unwrap();
        if *l == 0 {
            return Err(BackendError::Unavailable("crashed".into()));
        }
        *l -= 1;
        Ok(())
    }
}
impl Backend for FailAfter<'_> {
    fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
        self.inner.get(k)
    }
    fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.inner.get_range(k, o, l)
    }
    fn exists(&self, k: &str) -> Result<bool, BackendError> {
        self.inner.exists(k)
    }
    fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
        self.spend()?;
        self.inner.put_if_absent(k, d)
    }
    fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
        self.spend()?;
        self.inner.put_if_match(k, d, t)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
        self.inner.list(p)
    }
    fn delete(&self, k: &str) -> Result<(), BackendError> {
        self.spend()?;
        self.inner.delete(k)
    }
    fn supports_gc(&self) -> bool {
        true
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        self.inner.now_ms()
    }
    fn modified_ms(&self, k: &str) -> Result<Option<u64>, BackendError> {
        self.inner.modified_ms(k)
    }
    fn touch(&self, k: &str) -> Result<bool, BackendError> {
        self.inner.touch(k)
    }
}

#[test]
fn a_crash_at_any_mutating_step_never_loses_an_object() {
    let w = world();
    let ids = ids_of(&w.all);
    // gc/lock put, probe delete, pack, idx, 4 deletes, lock release = 9 mutating calls
    for n in 0..=10 {
        let b = fresh(&w);
        let f = FailAfter {
            inner: &b,
            left: Mutex::new(n),
        };
        let r = vcs_remote::compact::compact(&f, &opts());
        if n < 8 {
            assert!(r.is_err(), "n={n} must have been cut short");
        }
        invariants(&b, &ids).unwrap_or_else(|e| panic!("crash after {n} mutations: {e}"));
        g_reach(&b).unwrap_or_else(|e| panic!("crash after {n} mutations: {e}"));
        // the stale gc/lock of the crashed run expires; a later run (here: after its TTL) finishes the job
        b.advance_ms(7 * 3600 * 1000);
        vcs_remote::compact::compact(&b, &opts()).unwrap_or_else(|e| panic!("rerun after crash {n}: {e}"));
        invariants(&b, &ids).unwrap();
        assert_eq!(pack_stems(&b).len(), 2, "crash {n}: converged to the compacted layout");
        for stem in pack_stems(&b) {
            assert!(b.exists(&format!("{stem}.pack")).unwrap());
        }
    }
}

#[test]
fn a_corrupt_input_aborts_before_anything_is_deleted() {
    let w = world();
    let b = fresh(&w);
    let stem = pack_stems(&b)
        .into_iter()
        .find(|s| {
            let i = PackIndex::parse(&b.get(&format!("{s}.idx")).unwrap().unwrap().data, Path::new(s)).unwrap();
            i.pack_len < 1500
        })
        .unwrap();
    let mut data = b.get(&format!("{stem}.pack")).unwrap().unwrap().data;
    data[20] ^= 0xFF;
    b.tamper(&format!("{stem}.pack"), data);
    let keys = b.keys();
    let r = vcs_remote::compact::compact(&b, &opts());
    assert!(matches!(r, Err(GcError::Damaged(_))), "{r:?}");
    let mut now = b.keys();
    let mut was = keys;
    now.retain(|k| k != "gc/lock");
    was.retain(|k| k != "gc/lock");
    assert_eq!(now, was, "nothing written or deleted");
}

// ---------------------------------------------------------------- interleaving: readers

/// Counts the reader's backend requests and runs `before` ahead of each read.
struct Hooked<'a> {
    inner: &'a MemBackend,
    before: Mutex<Box<dyn FnMut() + Send + 'a>>,
    ops: Mutex<usize>,
}
impl Hooked<'_> {
    fn tick(&self) {
        *self.ops.lock().unwrap() += 1;
        (self.before.lock().unwrap())();
    }
}
impl Backend for Hooked<'_> {
    fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
        self.tick();
        self.inner.get(k)
    }
    fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.tick();
        self.inner.get_range(k, o, l)
    }
    fn exists(&self, k: &str) -> Result<bool, BackendError> {
        self.tick();
        self.inner.exists(k)
    }
    fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
        self.inner.put_if_absent(k, d)
    }
    fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
        self.inner.put_if_match(k, d, t)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
        self.tick();
        self.inner.list(p)
    }
}

enum Msg {
    At,
    Done(Result<usize, String>),
}

/// A compactor on its own thread that runs one phase per `advance`.
struct Baton {
    go: mpsc::Sender<()>,
    rx: mpsc::Receiver<Msg>,
    reached: usize,
    finished: bool,
    started: bool,
}
impl Baton {
    fn advance_to(&mut self, target: usize, check: &mut dyn FnMut() -> Result<(), String>) -> Result<(), String> {
        while self.reached < target && !self.finished {
            self.go.send(()).unwrap();
            self.started = true;
            match self.rx.recv().unwrap() {
                Msg::At => {
                    self.reached += 1;
                    check()?;
                }
                Msg::Done(r) => {
                    self.finished = true;
                    if let Err(e) = r {
                        // an aborted compaction is fine; it must not have broken anything
                        let _ = e;
                    }
                    check()?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
enum Reader {
    FetchMany,
    AnyPack,
    Source,
}

fn read_all(r: Reader, b: &dyn Backend, w: &World, rounds: usize) -> Result<(), String> {
    // the objects the compaction moves: everything not in the big pack's tier is fine to ask for
    // oldest objects first: they live in the packs being merged
    let want: Vec<&(ObjectId, Vec<u8>)> = w.all.iter().rev().filter(|o| !is_big(w, &o.0)).collect();
    match r {
        Reader::FetchMany => {
            let ids: Vec<ObjectId> = want.iter().map(|o| o.0).collect();
            let got = fetch_many_retrying(b, &ids, rounds).map_err(|e| format!("fetch_many: {e}"))?;
            let got: HashMap<_, _> = got.into_iter().collect();
            for (id, enc) in want {
                if got.get(id) != Some(enc) {
                    return Err(format!("fetch_many returned wrong bytes for {}", id.short()));
                }
            }
        }
        Reader::AnyPack => {
            for (id, enc) in want {
                match fetch_from_any_pack_retrying(b, id, rounds) {
                    Ok(Some(v)) if &v == enc => {}
                    other => {
                        return Err(format!(
                            "fetch_from_any_pack({}) = {:?}",
                            id.short(),
                            other.map(|o| o.map(|v| v.len()))
                        ))
                    }
                }
            }
        }
        Reader::Source => {
            let mut src = RemoteSource::new(b, ReadAhead::None).with_race_rounds(rounds);
            for (id, enc) in want {
                match src.fetch(id) {
                    Ok(Some(v)) if &v == enc => {}
                    other => {
                        return Err(format!(
                            "RemoteSource::fetch({}) = {:?}",
                            id.short(),
                            other.map(|o| o.map(|v| v.len()))
                        ))
                    }
                }
            }
        }
    }
    Ok(())
}

fn is_big(w: &World, id: &ObjectId) -> bool {
    w.all.iter().any(|o| o.0 == *id && o.1.len() > 2000)
}

/// All non-decreasing sequences of length `len` over `0..=max`.
fn schedules(len: usize, max: usize) -> Vec<Vec<usize>> {
    fn rec(len: usize, lo: usize, max: usize, cur: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if cur.len() == len {
            out.push(cur.clone());
            return;
        }
        for v in lo..=max {
            cur.push(v);
            rec(len, v, max, cur, out);
            cur.pop();
        }
    }
    let mut out = vec![];
    rec(len, 0, max, &mut vec![], &mut out);
    out
}

const PHASES: usize = 9;

/// Run the compactor and one reader under every schedule. `Some(violation)` for the first bad one.
fn search_readers(w: &World, quirks: CompactQuirks, reader: Reader, rounds: usize, reader_ops: usize) -> Option<String> {
    let ids = ids_of(&w.all);
    for adv in schedules(reader_ops, PHASES) {
        let b = fresh(w);
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (msg_tx, msg_rx) = mpsc::channel::<Msg>();
        let mut verdict: Option<String> = None;
        std::thread::scope(|sc| {
            let bref = &b;
            let m2 = msg_tx.clone();
            sc.spawn(move || {
                if go_rx.recv().is_err() {
                    return;
                }
                let r = compact_with(bref, &opts(), quirks, &mut |_| {
                    let _ = m2.send(Msg::At);
                    let _ = go_rx.recv();
                });
                let _ = m2.send(Msg::Done(r.map(|r| r.merges.len()).map_err(|e| e.to_string())));
            });
            let baton = Mutex::new(Baton {
                go: go_tx,
                rx: msg_rx,
                reached: 0,
                finished: false,
                started: false,
            });
            let violation: Mutex<Option<String>> = Mutex::new(None);
            let k = Mutex::new(0usize);
            let check_ids = ids.clone();
            let mut check = || invariants(&b, &check_ids);
            // the reader
            let hooked = Hooked {
                inner: &b,
                ops: Mutex::new(0),
                before: Mutex::new(Box::new({
                    let adv = adv.clone();
                    let baton = &baton;
                    let violation = &violation;
                    let k = &k;
                    let b = &b;
                    let ids = ids.clone();
                    move || {
                        let mut kk = k.lock().unwrap();
                        let target = adv.get(*kk).copied().unwrap_or(PHASES);
                        *kk += 1;
                        let mut bt = baton.lock().unwrap();
                        if let Err(e) = bt.advance_to(target, &mut || invariants(b, &ids)) {
                            violation.lock().unwrap().get_or_insert(e);
                        }
                    }
                })),
            };
            let res = read_all(reader, &hooked, w, rounds);
            // finish the compaction, checking every state
            let mut bt = baton.lock().unwrap();
            if let Err(e) = bt.advance_to(PHASES + 2, &mut check) {
                violation.lock().unwrap().get_or_insert(e);
            }
            if !bt.started {
                // the thread is parked waiting for its first go; release it so the scope can end
                let _ = bt.go.send(());
            }
            drop(bt);
            verdict = violation.lock().unwrap().take();
            if verdict.is_none() {
                if let Err(e) = res {
                    verdict = Some(format!("reader {reader:?} schedule {adv:?}: {e}"));
                }
            }
        });
        if let Some(v) = verdict {
            return Some(v);
        }
    }
    None
}

#[test]
fn readers_never_miss_an_object_during_a_compaction_under_every_schedule() {
    let w = world();
    for (reader, ops) in [(Reader::FetchMany, 8), (Reader::AnyPack, 6), (Reader::Source, 6)] {
        if let Some(v) = search_readers(&w, CompactQuirks::default(), reader, vcs_remote::packfetch::RACE_ROUNDS, ops) {
            panic!("{v}");
        }
    }
}

#[test]
fn mutation_reader_without_retry_is_caught() {
    let w = world();
    let found = [(Reader::FetchMany, 8), (Reader::AnyPack, 6), (Reader::Source, 6)]
        .into_iter()
        .filter(|(r, ops)| {
            let v = search_readers(&w, CompactQuirks::default(), *r, 1, *ops);
            println!("{r:?}: {v:?}");
            v.is_some()
        })
        .count();
    assert_eq!(
        found, 3,
        "a reader that does not re-list after a vanished pack must fail some schedule"
    );
}

#[test]
fn mutation_delete_before_upload_is_caught() {
    let w = world();
    let q = CompactQuirks {
        delete_before_upload: true,
        ..Default::default()
    };
    let v = search_readers(&w, q, Reader::AnyPack, vcs_remote::packfetch::RACE_ROUNDS, 2).expect("must be caught");
    assert!(v.contains("I-LIVE"), "{v}");
}

#[test]
fn mutation_delete_pack_before_index_is_caught() {
    let w = world();
    let q = CompactQuirks {
        delete_pack_before_idx: true,
        ..Default::default()
    };
    let v = search_readers(&w, q, Reader::AnyPack, vcs_remote::packfetch::RACE_ROUNDS, 2).expect("must be caught");
    assert!(v.contains("I-IDX"), "{v}");
}

#[test]
fn mutation_index_before_pack_is_caught() {
    let w = world();
    let q = CompactQuirks {
        idx_before_pack: true,
        ..Default::default()
    };
    let v = search_readers(&w, q, Reader::AnyPack, vcs_remote::packfetch::RACE_ROUNDS, 2).expect("must be caught");
    assert!(v.contains("I-IDX"), "{v}");
}

#[test]
fn mutation_dropped_object_without_verify_is_caught_and_verify_prevents_the_loss() {
    let w = world();
    let bad = CompactQuirks {
        drop_object: true,
        skip_verify: true,
        ..Default::default()
    };
    let v = search_readers(&w, bad, Reader::AnyPack, vcs_remote::packfetch::RACE_ROUNDS, 2).expect("must be caught");
    assert!(v.contains("I-LIVE"), "{v}");
    // the same drop with the read-back check on: the run aborts, inputs stay, invariants hold everywhere
    let guarded = CompactQuirks {
        drop_object: true,
        ..Default::default()
    };
    assert_eq!(
        search_readers(&w, guarded, Reader::AnyPack, vcs_remote::packfetch::RACE_ROUNDS, 2),
        None
    );
    let b = fresh(&w);
    let q = CompactQuirks {
        drop_object: true,
        ..Default::default()
    };
    let r = compact_with(&b, &opts(), q, &mut |_| {});
    assert!(matches!(r, Err(GcError::Damaged(m)) if m.contains("lacks")));
    assert_eq!(pack_stems(&b).len(), 4, "the inputs and the incomplete output are all still there");
}

// ---------------------------------------------------------------- interleaving: a publisher

/// A publisher adds c4 on top of c3 while the compactor runs; every non-decreasing split of the
/// publisher's steps over the compactor's phases.
fn search_publisher(w: &World, quirks: CompactQuirks, pub_steps: usize) -> Option<String> {
    let ids = ids_of(&w.all);
    let c4 = commit_over(&w.store, &[("f", noise(9, 120)), ("big", noise(5, 3000))], Some(w.commits[2]), 4);
    for adv in schedules(PHASES, pub_steps) {
        let b = fresh(w);
        let mut p = Publisher::new(&b, &w.store, tx(Some(w.commits[2]), c4));
        let mut done = false;
        let mut verdict: Option<String> = None;
        let mut phase = 0usize;
        {
            let run_pub = |upto: usize, p: &mut Publisher, done: &mut bool, verdict: &mut Option<String>, count: &mut usize| {
                while *count < upto && !*done {
                    match p.step() {
                        Ok(Step::Done(_)) => *done = true,
                        Ok(_) => {}
                        Err(e) => {
                            *done = true;
                            verdict.get_or_insert(format!("publisher failed: {e}"));
                        }
                    }
                    *count += 1;
                }
            };
            let mut pub_count = 0usize;
            let mut hook = |_pt: CompactPoint| {
                let target = adv.get(phase).copied().unwrap_or(pub_steps);
                phase += 1;
                run_pub(target, &mut p, &mut done, &mut verdict, &mut pub_count);
                if verdict.is_none() {
                    verdict = invariants(&b, &ids).err().or_else(|| g_reach(&b).err());
                }
            };
            let _ = compact_with(&b, &opts(), quirks, &mut hook);
            // let the publisher finish after the compaction
            run_pub(10_000, &mut p, &mut done, &mut verdict, &mut pub_count);
        }
        if verdict.is_none() {
            verdict = invariants(&b, &ids).err().or_else(|| g_reach(&b).err());
        }
        if let Some(v) = verdict {
            return Some(format!("schedule {adv:?}: {v}"));
        }
    }
    None
}

#[test]
fn a_concurrent_publisher_and_a_compaction_never_lose_an_object_under_every_schedule() {
    let w = world();
    if let Some(v) = search_publisher(&w, CompactQuirks::default(), 8) {
        panic!("{v}");
    }
}

#[test]
fn mutations_are_also_caught_with_a_publisher_present() {
    let w = world();
    for q in [
        CompactQuirks {
            delete_before_upload: true,
            ..Default::default()
        },
        CompactQuirks {
            drop_object: true,
            skip_verify: true,
            ..Default::default()
        },
        CompactQuirks {
            idx_before_pack: true,
            ..Default::default()
        },
    ] {
        assert!(search_publisher(&w, q, 3).is_some(), "{q:?} must be caught");
    }
}

// ---------------------------------------------------------------- the policy

fn sim(n: usize, size: impl Fn(usize) -> u64, ratio: u64, base: u64) -> (usize, usize, f64) {
    let mut packs: Vec<PackInfo> = vec![];
    let (mut published, mut written, mut max_packs) = (0u64, 0u64, 0usize);
    for i in 0..n {
        let s = size(i);
        published += s;
        written += s;
        packs.push(PackInfo {
            stem: format!("p{i}"),
            len: s,
        });
        // compact after every publish, as a policy that runs often would
        let plan = plan_tiers(&packs, ratio, base, u64::MAX);
        for (gi, g) in plan.iter().enumerate() {
            let sum: u64 = packs.iter().filter(|p| g.contains(&p.stem)).map(|p| p.len).sum();
            written += sum;
            packs.retain(|p| !g.contains(&p.stem));
            packs.push(PackInfo {
                stem: format!("m{i}-{gi}"),
                len: sum,
            });
        }
        max_packs = max_packs.max(packs.len());
    }
    (packs.len(), max_packs, written as f64 / published as f64)
}

#[test]
fn a_thousand_publishes_end_with_few_packs_and_bounded_rewrite_cost() {
    // equal-sized publishes (2 KiB packs), then a mix of sizes
    let (n, max, amp) = sim(1000, |_| 2048, 3, 4096);
    println!("uniform: {n} packs at the end, at most {max}, {amp:.2}x bytes written / published");
    assert!(n <= 12 && amp <= 6.5, "{n} {amp}");
    let mixed = [2048u64, 2048, 3000, 9000, 700_000, 2048];
    let (n, max, amp) = sim(1000, |i| mixed[i % mixed.len()], 3, 4096);
    println!("mixed: {n} packs at the end, at most {max}, {amp:.2}x");
    assert!(n <= 12 && amp <= 6.5, "{n} {amp}");
}

#[test]
fn the_plan_is_deterministic_and_respects_the_memory_cap() {
    let packs: Vec<PackInfo> = (0..6)
        .map(|i| PackInfo {
            stem: format!("p{i}"),
            len: 1000 + i as u64,
        })
        .collect();
    let a = plan_tiers(&packs, 3, 4096, u64::MAX);
    let mut rev = packs.clone();
    rev.reverse();
    assert_eq!(a, plan_tiers(&rev, 3, 4096, u64::MAX));
    assert_eq!(a.iter().map(Vec::len).sum::<usize>(), 6);
    let capped = plan_tiers(&packs, 3, 4096, 2500);
    for g in &capped {
        let s: u64 = packs.iter().filter(|p| g.contains(&p.stem)).map(|p| p.len).sum();
        assert!(s <= 2500 && g.len() >= 2);
    }
    // a lone pack and a below-ratio tier are left alone
    assert!(plan_tiers(&packs[..2], 3, 4096, u64::MAX).is_empty());
    let _: BTreeMap<u8, u8> = BTreeMap::new();
    let _ = build_pack_fmt(vec![], PackFormat::V2);
}
