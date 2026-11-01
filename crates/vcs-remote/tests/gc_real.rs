//! Remote GC (protocol G1, docs/gc.md) against the REAL publisher and sweeper.
//!
//! This is `research/storage-engine`'s `model_gc.rs` ported to production code. The world is the
//! model's: an old orphan closure C -> D, the current head H0 -> Y (old), and a publisher that
//! either (A) makes a new commit P on top of C the head (re-adopting C by dedup, uploading only P)
//! or (B) makes the old orphan C itself the head (uploads nothing, so only the touch protects it).
//!
//! The publisher is a step machine and the sweeper takes a hook at each of its phases, so a test
//! schedules `c_k` publisher steps before the sweeper's k-th phase, for EVERY non-decreasing
//! choice of the `c_k` (an exhaustive interleaving search over a few hundred thousand schedules).
//! Invariant G-REACH, checked after every phase: every object in the closure of every branch head
//! in the manifest exists. The mutation tests switch off one protocol rule at a time (`GcQuirks`)
//! and require the search to find a schedule that violates G-REACH, i.e. each rule is necessary.
//! Fault-injection tests cover lost deletes, crashed sweepers and an expired/stolen lock.

use std::collections::HashSet;
use vcs_objects::*;
use vcs_remote::gc::{remote_gc_with, RemoteGcOptions};
use vcs_remote::protocol::{object_key, tomb_key, GcQuirks};
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore};

const SEC: u64 = 1000;
const HOUR: u64 = 3600 * SEC;
const DAY: u64 = 24 * HOUR;
const T0: u64 = 1_000_000;

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

struct World {
    store: MemoryStore,
    c: ObjectId,
    d: ObjectId,
    h0: ObjectId,
    p: ObjectId,
}

fn tree_of(s: &MemoryStore, name: &str, content: &[u8]) -> (ObjectId, ObjectId) {
    let blob = s.put_blob(content).unwrap();
    let t = s
        .put(&Object::Tree(
            Tree::new(vec![TreeEntry {
                name: name.into(),
                mode: EntryMode::File,
                id: blob,
            }])
            .unwrap(),
        ))
        .unwrap();
    (blob, t)
}

fn commit(s: &MemoryStore, root: ObjectId, parents: Vec<ObjectId>, tag: u8) -> ObjectId {
    s.put(&Object::Commit(Commit {
        zone: "z".into(),
        parents,
        root,
        change_id: ChangeId::from_bytes([tag; 16]),
        author: sig(),
        message: format!("c{tag}"),
    }))
    .unwrap()
}

fn world() -> World {
    let store = MemoryStore::new();
    let (_, d) = tree_of(&store, "f", b"orphan content");
    let c = commit(&store, d, vec![], 1);
    let (_, y) = tree_of(&store, "f", b"head content");
    let h0 = commit(&store, y, vec![], 2);
    let (_, tp) = tree_of(&store, "f", b"new content");
    let p = commit(&store, tp, vec![c], 3);
    World { store, c, d, h0, p }
}

fn deps_of(o: &Object) -> Vec<ObjectId> {
    match o {
        Object::Commit(c) => c.parents.iter().copied().chain([c.root]).collect(),
        Object::Tree(t) => t.entries().iter().map(|e| e.id).collect(),
        _ => vec![],
    }
}

/// Upload `id` and its closure straight to the backend (children first).
fn upload(b: &dyn Backend, s: &MemoryStore, id: ObjectId, seen: &mut HashSet<ObjectId>) {
    if !seen.insert(id) {
        return;
    }
    let o = s.get(&id).unwrap();
    for d in deps_of(&o) {
        upload(b, s, d, seen);
    }
    b.put_if_absent(&object_key(&id), &o.encode()).unwrap();
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

fn opts() -> RemoteGcOptions {
    RemoteGcOptions {
        grace_ms: DAY,
        hold_ms: HOUR,
        dry_run: false,
        lock_ttl_ms: 6 * HOUR,
        owner: "sweeper".into(),
        allow_short_grace: false,
    }
}

/// Backend with the orphan C -> D (loose, as a legacy publisher left it) and published head
/// H0, all stamped `T0`; then 10 days pass.
fn setup(w: &World) -> MemBackend {
    setup_with(w, false)
}

/// With `orphan_pack`, the orphan closure is instead a pack + index left by an aborted publish
/// (a publisher that died after PutIdx), exactly the bytes a retry of that publish would write.
fn setup_with(w: &World, orphan_pack: bool) -> MemBackend {
    let b = MemBackend::new().with_gc();
    b.set_now_ms(T0);
    Publisher::new(&b, &w.store, tx(None, w.h0)).run().unwrap();
    if orphan_pack {
        let mut t = tx(None, w.c);
        t.updates[0].branch = "aborted".into();
        let mut p = Publisher::new(&b, &w.store, t);
        for _ in 0..3 {
            p.step().unwrap(); // Collect, PutPack, PutIdx ... then the client dies
        }
        assert_eq!(b.keys().iter().filter(|k| k.starts_with("packs/")).count(), 4, "{:?}", b.keys());
    } else {
        upload(&b, &w.store, w.c, &mut HashSet::new());
    }
    b.advance_ms(10 * DAY);
    b
}

/// G-REACH: the closure of every branch head in the manifest is complete on the backend.
fn g_reach(b: &dyn Backend) -> Result<(), String> {
    let (m, _) = load_manifest(b).map_err(|e| e.to_string())?;
    let mut seen = HashSet::new();
    let mut stack: Vec<ObjectId> = m.branches.values().copied().collect();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let o = fetch_object(b, &id).map_err(|_| format!("G-REACH: head needs {} which is gone", id.short()))?;
        stack.extend(deps_of(&o));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    /// New commit P over the old (loose) orphan C; packed publisher.
    Adopt,
    /// The old loose orphan C itself becomes the head.
    AdoptHead,
    /// Like `Adopt` with the legacy loose-object publisher.
    AdoptLoose,
    /// The orphan closure is a pack left by an aborted publish; the publisher re-uploads
    /// exactly that pack (dedup hit) and makes C the head.
    AdoptPack,
}

impl Variant {
    fn orphan_pack(self) -> bool {
        self == Variant::AdoptPack
    }
    /// Publisher steps worth interleaving before the sweeper finishes (a bit under its total).
    fn max_steps(self) -> usize {
        match self {
            Variant::Adopt => 11,
            Variant::AdoptHead => 9,
            Variant::AdoptLoose | Variant::AdoptPack => 13,
        }
    }
    fn slots(self) -> usize {
        // Start, Candidates, Tombed, Remarked, then Before/After per deleted unit
        if self == Variant::AdoptPack {
            6
        } else {
            8
        }
    }
}

fn publisher<'a>(w: &'a World, b: &'a MemBackend, v: Variant, q: GcQuirks) -> Publisher<'a> {
    let new = if matches!(v, Variant::Adopt | Variant::AdoptLoose) {
        w.p
    } else {
        w.c
    };
    let p = Publisher::new(b, &w.store, tx(Some(w.h0), new))
        .with_hold_ms(HOUR)
        .with_gc_quirks(q);
    if v == Variant::AdoptLoose {
        p.with_loose_objects()
    } else {
        p
    }
}

/// Run one schedule: `cum[k]` = publisher steps completed when the sweeper reaches its k-th
/// phase (`cum[0]` before it starts). `stall_at`: advance the backend clock by `stall` ms on
/// entering that phase (a slow publisher). Err = a protocol violation (the text names it).
fn run_schedule(v: Variant, q: GcQuirks, cum: &[usize], stall: Option<(usize, u64)>) -> Result<(), String> {
    let w = world();
    let b = setup_with(&w, v.orphan_pack());
    let mut p = publisher(&w, &b, v, q);
    let mut done = 0usize;
    let mut slot = 0usize;
    let mut failure: Option<String> = None;
    let enter = |slot: usize, p: &mut Publisher, done: &mut usize, failure: &mut Option<String>| {
        if let Some((s, ms)) = stall {
            if s == slot {
                b.advance_ms(ms);
            }
        }
        let target = cum.get(slot).copied().unwrap_or(*done).max(*done);
        while *done < target && !p.is_finished() {
            if p.step().is_err() {
                break;
            }
            *done += 1;
        }
        if failure.is_none() {
            if let Err(e) = g_reach(&b) {
                *failure = Some(format!("after phase {slot} ({done} publisher steps): {e}"));
            }
        }
    };
    enter(slot, &mut p, &mut done, &mut failure);
    slot += 1;
    let r = remote_gc_with(&b, &opts(), q, &mut |pt| {
        let _ = pt;
        enter(slot, &mut p, &mut done, &mut failure);
        slot += 1;
    });
    if let Err(e) = r {
        return Err(format!("sweeper failed: {e}"));
    }
    // let the publisher finish
    let mut last = None;
    for _ in 0..500 {
        if p.is_finished() {
            break;
        }
        match p.step() {
            Err(e) => {
                last = Some(e.to_string());
                break;
            }
            Ok(Step::Done(o)) => {
                last = Some(format!("{o:?}"));
                break;
            }
            Ok(_) => {}
        }
    }
    if g_reach(&b).is_err() {
        eprintln!(
            "publisher ended: {last:?}; C {} D {} H0 {} P {}; keys {:?}",
            w.c.short(),
            w.d.short(),
            w.h0.short(),
            w.p.short(),
            b.keys()
        );
    }
    if let Some(f) = failure {
        return Err(f);
    }
    g_reach(&b)
}

/// All non-decreasing sequences `0 <= c_0 <= ... <= c_{k-1} <= max`; stops at the first `Err`.
fn search(k: usize, max: usize, f: &mut dyn FnMut(&[usize]) -> Result<(), String>) -> Result<usize, (Vec<usize>, String)> {
    fn rec(
        cur: &mut Vec<usize>,
        k: usize,
        lo: usize,
        max: usize,
        f: &mut dyn FnMut(&[usize]) -> Result<(), String>,
        n: &mut usize,
    ) -> Result<(), (Vec<usize>, String)> {
        if cur.len() == k {
            *n += 1;
            return f(cur).map_err(|e| (cur.clone(), e));
        }
        for c in lo..=max {
            cur.push(c);
            rec(cur, k, c, max, f, n)?;
            cur.pop();
        }
        Ok(())
    }
    let mut n = 0;
    rec(&mut Vec::new(), k, 0, max, f, &mut n).map(|_| n)
}

// Phases hooked: Start, Candidates, Tombed, Remarked, then BeforeDelete/AfterDelete of C and D.

const ALL: [Variant; 4] = [Variant::Adopt, Variant::AdoptHead, Variant::AdoptLoose, Variant::AdoptPack];

fn explore(v: Variant, q: GcQuirks) -> Result<usize, (Vec<usize>, String)> {
    search(v.slots(), v.max_steps(), &mut |c| run_schedule(v, q, c, None))
}

#[test]
fn good_protocol_never_loses_reachable_data_in_any_interleaving() {
    for v in ALL {
        match explore(v, GcQuirks::default()) {
            Ok(n) => eprintln!("{v:?}: {n} schedules, no violation"),
            Err((c, e)) => panic!("{v:?}: schedule {c:?}: {e}"),
        }
    }
}

fn must_break(q: GcQuirks, name: &str) {
    for v in ALL {
        if let Err((c, e)) = explore(v, q) {
            assert!(e.contains("G-REACH"), "{name}: unexpected failure {e}");
            eprintln!("{name}: violated in {v:?} by schedule {c:?}: {e}");
            return;
        }
    }
    panic!("{name}: no schedule violates G-REACH, so this rule is not shown to be necessary");
}

#[test]
fn mutation_publisher_does_not_touch_on_dedup() {
    must_break(
        GcQuirks {
            publisher_no_touch: true,
            ..Default::default()
        },
        "no touch",
    );
}
#[test]
fn mutation_publisher_does_not_check_tomb() {
    must_break(
        GcQuirks {
            publisher_no_check_tomb: true,
            ..Default::default()
        },
        "no tomb check",
    );
}
#[test]
fn mutation_publisher_does_not_recheck_existence_after_tomb_check() {
    must_break(
        GcQuirks {
            publisher_no_check_exists: true,
            ..Default::default()
        },
        "no exists check",
    );
}
#[test]
fn mutation_sweeper_writes_tombs_after_deciding() {
    must_break(
        GcQuirks {
            sweeper_tombs_after_deciding: true,
            ..Default::default()
        },
        "tombs after deciding",
    );
}
#[test]
fn mutation_young_objects_are_not_roots() {
    must_break(
        GcQuirks {
            sweeper_young_not_roots: true,
            ..Default::default()
        },
        "young not roots",
    );
}
#[test]
fn mutation_sweeper_ignores_the_manifest() {
    must_break(
        GcQuirks {
            sweeper_ignores_manifest: true,
            ..Default::default()
        },
        "ignores manifest",
    );
}

/// The hold-time fence: the publisher uploads P and stalls for two days; the sweeper then sees P
/// as old and unreferenced. With the fence the publisher starts over; without it, it CASes a head
/// whose objects are gone.
#[test]
fn mutation_publisher_cas_not_fenced_by_hold_time() {
    let stall = 2 * DAY;
    let run = |q: GcQuirks| {
        search(5, 8, &mut |c| {
            for at in 1..5 {
                run_schedule(Variant::Adopt, q, c, Some((at, stall)))?;
            }
            Ok(())
        })
    };
    let n = run(GcQuirks::default()).unwrap_or_else(|(c, e)| panic!("fenced publisher lost data: {c:?}: {e}"));
    eprintln!("fenced: {n} schedules x 4 stall points, no violation");
    let (c, e) = run(GcQuirks {
        publisher_no_fence: true,
        ..Default::default()
    })
    .expect_err("without the fence a stalled publisher must lose data");
    assert!(e.contains("G-REACH"), "{e}");
    eprintln!("no fence: violated by {c:?}: {e}");
}

// ---------------------------------------------------------------- plain behaviour + fault injection

fn orphan_world() -> (World, MemBackend) {
    let w = world();
    let b = setup(&w);
    (w, b)
}

#[test]
fn deletes_old_unreachable_closure_keeps_head_and_reports() {
    let (w, b) = orphan_world();
    let rep = vcs_remote::remote_gc(&b, &opts()).unwrap();
    assert!(!rep.dry_run);
    assert!(rep.deleted.contains(&w.c) && rep.deleted.contains(&w.d), "{rep:?}");
    for id in &rep.deleted {
        assert!(!b.exists(&object_key(id)).unwrap());
    }
    assert!(fetch_object(&b, &w.h0).is_ok(), "the head lives in a pack and must survive");
    g_reach(&b).unwrap();
    assert!(b.list("tomb/").unwrap().is_empty(), "no tombstones left behind");
    assert!(b.list("gc/").unwrap().is_empty(), "lock released");
    assert!(b.exists("manifest").unwrap());
    // 3 = C, D's blob... C, its tree D and the tree's blob
    assert_eq!(rep.deleted.len(), 3);
    // idempotent
    assert!(vcs_remote::remote_gc(&b, &opts()).unwrap().deleted.is_empty());
}

#[test]
fn dry_run_deletes_nothing_and_takes_no_lock() {
    let (_w, b) = orphan_world();
    let before = b.keys();
    let mut o = opts();
    o.dry_run = true;
    let rep = vcs_remote::remote_gc(&b, &o).unwrap();
    assert_eq!(rep.deleted.len(), 3);
    assert_eq!(b.keys(), before);
}

#[test]
fn young_objects_and_their_dependencies_are_kept() {
    let (w, b) = orphan_world();
    // a publisher uploaded P (young) but has not CASed yet
    let mut seen = HashSet::new();
    seen.insert(w.c);
    seen.insert(w.d);
    upload(&b, &w.store, w.p, &mut seen);
    let rep = vcs_remote::remote_gc(&b, &opts()).unwrap();
    assert!(rep.deleted.is_empty(), "C is a dependency of young P: {:?}", rep.deleted);
    assert!(rep.saved_by_remark >= 1);
}

#[test]
fn history_in_the_applied_window_is_a_root() {
    let (w, b) = orphan_world();
    // move the branch H0 -> C (adopting the orphan), then to P: H0's tx is still in the window
    Publisher::new(&b, &w.store, tx(Some(w.h0), w.p)).run().unwrap();
    b.advance_ms(30 * DAY);
    let rep = vcs_remote::remote_gc(&b, &opts()).unwrap();
    assert!(rep.deleted.is_empty(), "old head H0 is named by a retained tx: {:?}", rep.deleted);
    assert!(fetch_object(&b, &w.h0).is_ok());
}

#[test]
fn refuses_unsafe_grace_unsupported_backends_and_a_damaged_remote() {
    let (w, b) = orphan_world();
    let mut o = opts();
    o.grace_ms = HOUR; // <= 2 * hold
    assert!(matches!(vcs_remote::remote_gc(&b, &o), Err(GcError::UnsafeGrace { .. })));
    assert_eq!(b.keys().iter().filter(|k| k.starts_with("objects/")).count(), 3);
    // a backend without GC support
    struct NoGc(MemBackend);
    macro_rules! fwd {
        () => {};
    }
    fwd!();
    impl Backend for NoGc {
        fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
            self.0.get(k)
        }
        fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
            self.0.get_range(k, o, l)
        }
        fn exists(&self, k: &str) -> Result<bool, BackendError> {
            self.0.exists(k)
        }
        fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
            self.0.put_if_absent(k, d)
        }
        fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
            self.0.put_if_match(k, d, t)
        }
        fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
            self.0.list(p)
        }
        fn delete(&self, k: &str) -> Result<(), BackendError> {
            self.0.delete(k)
        }
    }
    let n = NoGc(b);
    assert!(matches!(vcs_remote::remote_gc(&n, &opts()), Err(GcError::Unsupported)));
    // damaged: the head's tree is missing -> refuse to collect anything
    let b = n.0;
    let (m, _) = load_manifest(&b).unwrap();
    assert!(!m.branches.is_empty());
    for k in b.list("packs/").unwrap() {
        b.delete(&k).unwrap(); // the head's pack is gone
    }
    let before = b.keys();
    assert!(matches!(vcs_remote::remote_gc(&b, &opts()), Err(GcError::Damaged(_))));
    assert_eq!(b.keys(), before, "a damaged remote is left exactly as found");
    let _ = w;
}

#[test]
fn lost_delete_leaves_tombstone_and_object_and_a_rerun_finishes() {
    let (w, b) = orphan_world();
    b.fail_next_delete(&object_key(&w.d));
    let e = vcs_remote::remote_gc(&b, &opts()).unwrap_err();
    assert!(matches!(e, GcError::Backend(BackendError::Unavailable(_))), "{e}");
    g_reach(&b).unwrap();
    assert!(
        !b.list("tomb/").unwrap().is_empty(),
        "interrupted sweep leaves tombstones (safe: publishers abort)"
    );
    // a publisher that would re-adopt D is blocked by the stale tombstone, not allowed through
    let mut p = Publisher::new(&b, &w.store, tx(Some(w.h0), w.c)).with_hold_ms(HOUR);
    for _ in 0..40 {
        if p.is_finished() || p.step().is_err() {
            break;
        }
    }
    g_reach(&b).unwrap();
    // next run takes the (expired or free) lock, clears stale tombs and completes
    let lock_gone = b.list("gc/").unwrap().is_empty();
    assert!(lock_gone, "lock released even on failure");
    let rep = vcs_remote::remote_gc(&b, &opts()).unwrap();
    assert!(rep.stale_tombs_cleared >= 1, "{rep:?}");
    assert!(b.list("tomb/").unwrap().is_empty());
    g_reach(&b).unwrap();
}

#[test]
fn second_sweeper_is_locked_out_until_the_lock_expires_then_takes_over() {
    let (_w, b) = orphan_world();
    let now = b.now_ms().unwrap();
    b.put_if_absent("gc/lock", format!("other {}", now + HOUR).as_bytes()).unwrap();
    assert!(matches!(vcs_remote::remote_gc(&b, &opts()), Err(GcError::Locked(_))));
    assert!(b.exists(&object_key(&_w.c)).unwrap());
    b.advance_ms(2 * HOUR);
    vcs_remote::remote_gc(&b, &opts()).unwrap();
}

#[test]
fn publisher_touches_what_it_adopts_and_aborts_on_tombstone() {
    let (w, b) = orphan_world();
    let before = b.modified_ms(&object_key(&w.c)).unwrap().unwrap();
    // run a publisher to completion: C adopted as head
    Publisher::new(&b, &w.store, tx(Some(w.h0), w.c)).run().unwrap();
    assert!(
        b.modified_ms(&object_key(&w.c)).unwrap().unwrap() > before,
        "dedup hit must refresh the remote age"
    );
    // with a tombstone present the publisher refuses to proceed (retries, then Unknown)
    let (w2, b2) = orphan_world();
    b2.put_if_absent(&tomb_key(&w2.c), b"gc").unwrap();
    let e = Publisher::new(&b2, &w2.store, tx(Some(w2.h0), w2.c)).run().unwrap_err();
    assert!(matches!(e, PublishError::Unknown(..)), "{e}");
    let (m, _) = load_manifest(&b2).unwrap();
    assert_eq!(m.head("z", "main"), Some(w2.h0), "nothing was published");
}

// ---------------------------------------------------------------- packs

fn packs_of(b: &MemBackend) -> Vec<String> {
    b.keys()
        .into_iter()
        .filter(|k| k.starts_with("packs/") && k.ends_with(".idx"))
        .collect()
}

#[test]
fn orphan_pack_from_an_aborted_publish_is_deleted_when_old_and_kept_when_young_or_needed() {
    let w = world();
    let b = setup_with(&w, true);
    assert_eq!(packs_of(&b).len(), 2);
    // a second aborted pack written just now: young, and it DEPENDS on the old orphan (P over C)
    let mut t = tx(None, w.p);
    t.updates[0].branch = "aborted2".into();
    let mut p = Publisher::new(&b, &w.store, t);
    for _ in 0..3 {
        p.step().unwrap();
    }
    assert_eq!(packs_of(&b).len(), 3);
    let before = b.keys();
    let rep = vcs_remote::remote_gc(&b, &opts()).unwrap();
    assert!(
        rep.deleted_packs.is_empty() && rep.deleted.is_empty(),
        "young P needs old C: {rep:?}"
    );
    assert!(rep.saved_by_remark >= 1 && rep.kept_young >= 1);
    assert_eq!(b.keys(), before);
    // once everything is old, both aborted packs go; the head's pack stays
    b.advance_ms(30 * DAY);
    let rep = vcs_remote::remote_gc(&b, &opts()).unwrap();
    assert_eq!(rep.deleted_packs.len(), 2, "{rep:?}");
    assert_eq!(packs_of(&b).len(), 1);
    g_reach(&b).unwrap();
    assert!(b.list("tomb/").unwrap().is_empty());
    assert_eq!(b.keys().iter().filter(|k| k.ends_with(".pack")).count(), 1, "no half packs left");
}

#[test]
fn a_pack_with_any_live_object_is_kept_whole() {
    let w = world();
    let b = setup_with(&w, false);
    // one pack holding both the live head and an unreachable object
    let live = Object::Blob(b"x".to_vec());
    let extra = w.store.get(&w.c).unwrap();
    let built = vcs_store::pack::build_pack(vec![
        (w.h0, w.store.get(&w.h0).unwrap().encode()),
        (w.c, extra.encode()),
        (live.id(), live.encode()),
    ]);
    b.put_if_absent(&format!("packs/{}.pack", built.name), &built.pack).unwrap();
    b.put_if_absent(&format!("packs/{}.idx", built.name), &built.idx).unwrap();
    b.advance_ms(30 * DAY);
    let rep = vcs_remote::remote_gc(&b, &opts()).unwrap();
    assert!(rep.packs_with_garbage_kept >= 1, "{rep:?}");
    assert!(b.exists(&format!("packs/{}.pack", built.name)).unwrap());
    g_reach(&b).unwrap();
}

#[test]
fn stray_pack_without_index_is_deleted_only_when_old() {
    let w = world();
    let b = setup_with(&w, false);
    b.put_if_absent("packs/pack-dead.pack", b"junk").unwrap(); // old after the advance below
    b.advance_ms(30 * DAY);
    b.put_if_absent("packs/pack-fresh.pack", b"junk").unwrap();
    let rep = vcs_remote::remote_gc(&b, &opts()).unwrap();
    assert_eq!(rep.deleted_packs, vec!["packs/pack-dead".to_string()]);
    assert!(b.exists("packs/pack-fresh.pack").unwrap() && !b.exists("packs/pack-dead.pack").unwrap());
    g_reach(&b).unwrap();
}

#[test]
fn republishing_an_old_orphan_pack_touches_it() {
    let w = world();
    let b = setup_with(&w, true);
    let stem = packs_of(&b)
        .into_iter()
        .find(|k| k.ends_with(".idx"))
        .map(|k| k.trim_end_matches(".idx").to_string());
    // the aborted pack is the one holding C
    let orphan: Vec<String> = packs_of(&b)
        .into_iter()
        .map(|k| k.trim_end_matches(".idx").to_string())
        .filter(|st| {
            let idx = b.get(&format!("{st}.idx")).unwrap().unwrap();
            vcs_store::pack::PackIndex::parse(&idx.data, std::path::Path::new(st))
                .unwrap()
                .find(&w.c)
                .is_some()
        })
        .collect();
    assert_eq!(orphan.len(), 1, "{stem:?}");
    let before = b.modified_ms(&format!("{}.idx", orphan[0])).unwrap().unwrap();
    Publisher::new(&b, &w.store, tx(Some(w.h0), w.c)).run().unwrap();
    assert!(b.modified_ms(&format!("{}.pack", orphan[0])).unwrap().unwrap() > before);
    assert!(b.modified_ms(&format!("{}.idx", orphan[0])).unwrap().unwrap() > before);
    assert!(vcs_remote::remote_gc(&b, &opts()).unwrap().deleted_packs.is_empty());
    g_reach(&b).unwrap();
}
