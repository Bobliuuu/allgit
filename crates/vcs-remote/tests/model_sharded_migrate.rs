//! Executable model of the single-manifest -> sharded MIGRATION (issue #313, step 1 of
//! docs/manifest-scaling.md "Recommended path"). Nothing here is production code; it checks the
//! cut-over protocol that `shard.rs` implements and that every rule in it is necessary.
//!
//! Store: `manifest` (heads of Z zones, `frozen`, `moved`, CAS version), `shard/<z>` (create-only,
//! absent until copied), `root` (create-only; its existence is THE switch: once it exists the
//! shards are authoritative, before it the manifest is).
//! Migrator (idempotent, any step may crash, the whole migration is re-run): 1. Freeze: CAS
//! `manifest.frozen = true` (bumps the version, so every single-manifest writer that loaded
//! before it fails its CAS). 2. Copy: for each zone, create-only `shard/<z>` = the frozen head.
//! 3. Root: create-only `root`. 4. Moved: CAS `manifest.moved = true` (old binaries refuse a
//! moved manifest with a clear error).
//! Writer: if `root` exists, apply on the shard (precondition on the head); else load the
//! manifest; if frozen, retry; else CAS the manifest.
//! Reader of one zone: root? shard : (manifest value, then re-check root; if it appeared, discard
//! and read the shard).
//! Invariants after EVERY step: M-ATOMIC the authoritative state equals the replay of the
//! committed writes in commit order (no lost update); M-ROOT root implies all shards exist and
//! the manifest is frozen; M-SNAP reader results are a committed prefix not older than the
//! prefix at its start; M-OUTCOME a writer reports success iff its write was applied.
use std::collections::HashSet;

const Z: usize = 2;

#[derive(Clone, Copy)]
struct Flags {
    freeze: bool,
    shards_before_root: bool,
    reader_rechecks_root: bool,
}
const GOOD: Flags = Flags {
    freeze: true,
    shards_before_root: true,
    reader_rechecks_root: true,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct St {
    heads: [u8; Z],
    frozen: bool,
    moved: bool,
    ver: u32,
    shard: [Option<u8>; Z],
    root: bool,
    order: Vec<usize>,
    w: Vec<W>,
    m: u8, // migrator pc: 0 freeze, 1..=Z copy, Z+1 root, Z+2 moved, Z+3 done (with shards_before_root=false: root first)
    m_crashed: bool,
    faulted: bool,
    r: Option<R>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct W {
    z: usize,
    old: u8,
    new: u8,
    ph: u8, // 0 start, 1 cas(loaded ver), 2 done ok, 3 done fail
    ver: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct R {
    k0: Option<usize>,
    ph: u8, // 0 check root, 1 read manifest, 2 recheck, 3 read shard, 4 done
    v: u8,
    res: Option<u8>,
}

fn replay(st: &St, ws: &[(usize, u8, u8)], upto: usize) -> [u8; Z] {
    let mut h = [0u8; Z];
    for i in &st.order[..upto] {
        let (z, old, new) = ws[*i];
        assert_eq!(h[z], old, "M-ATOMIC: write {i} applied over a stale value (lost update)");
        h[z] = new;
    }
    h
}

fn authoritative(st: &St) -> Vec<u8> {
    if st.root {
        (0..Z)
            .map(|z| st.shard[z].expect("M-ROOT: root exists but a shard is missing (lost update window)"))
            .collect()
    } else {
        st.heads.to_vec()
    }
}

fn check(st: &St, ws: &[(usize, u8, u8)]) {
    if st.root {
        assert!(st.shard.iter().all(|s| s.is_some()), "M-ROOT: root exists but a shard is missing");
    }
    let want = replay(st, ws, st.order.len());
    assert_eq!(authoritative(st), want.to_vec(), "M-ATOMIC: authoritative state != replay");
    if let Some(r) = &st.r {
        if let Some(v) = r.res {
            let ok = (r.k0.unwrap()..=st.order.len()).any(|k| replay(st, ws, k)[0] == v);
            assert!(ok, "M-SNAP: reader saw stale or torn value {v}");
        }
    }
}

fn migrator_step(st: &mut St, f: &Flags) {
    // pc layout: freeze, copy*Z, root, moved   (or freeze, root, copy*Z, moved when !shards_before_root)
    let pc = st.m as usize;
    let root_pc = if f.shards_before_root { Z + 1 } else { 1 };
    let copy_lo = if f.shards_before_root { 1 } else { 2 };
    if pc == 0 {
        if f.freeze && !st.frozen {
            st.frozen = true;
            st.ver += 1;
        }
    } else if pc == root_pc {
        st.root = true;
    } else if pc >= copy_lo && pc < copy_lo + Z {
        let z = pc - copy_lo;
        if st.shard[z].is_none() {
            st.shard[z] = Some(st.heads[z]);
        }
    } else {
        st.moved = true;
        st.ver += 1;
    }
    st.m += 1;
}
const MDONE: u8 = (Z + 3) as u8;

fn writer_step(st: &mut St, i: usize) -> bool {
    let w = st.w[i].clone();
    match w.ph {
        0 => {
            if st.root {
                // sharded apply: one create/CAS on the shard
                let cur = st.shard[w.z].unwrap_or(0);
                if cur == w.old {
                    st.shard[w.z] = Some(w.new);
                    st.order.push(i);
                    st.w[i].ph = 2;
                } else {
                    st.w[i].ph = 3;
                }
                true
            } else if st.frozen {
                false // retry later
            } else {
                st.w[i].ph = 1;
                st.w[i].ver = st.ver;
                true
            }
        }
        1 => {
            if st.ver != w.ver {
                st.w[i].ph = 0; // lost CAS: reload
            } else if st.heads[w.z] == w.old {
                st.heads[w.z] = w.new;
                st.ver += 1;
                st.order.push(i);
                st.w[i].ph = 2;
            } else {
                st.w[i].ph = 3;
            }
            true
        }
        _ => false,
    }
}

fn reader_step(st: &mut St, f: &Flags) -> bool {
    let Some(mut r) = st.r.clone() else { return false };
    if r.ph == 4 {
        return false;
    }
    if r.k0.is_none() {
        r.k0 = Some(st.order.len());
    }
    match r.ph {
        0 => r.ph = if st.root { 3 } else { 1 },
        1 => {
            r.v = st.heads[0];
            r.ph = if f.reader_rechecks_root { 2 } else { 4 };
            if r.ph == 4 {
                r.res = Some(r.v);
            }
        }
        2 => {
            if st.root {
                r.ph = 3;
            } else {
                r.res = Some(r.v);
                r.ph = 4;
            }
        }
        _ => {
            r.res = Some(st.shard[0].unwrap_or(0));
            r.ph = 4;
        }
    }
    st.r = Some(r);
    true
}

struct Stats {
    seen: HashSet<St>,
    nodes: u64,
}

fn finish(mut st: St, ws: &[(usize, u8, u8)], f: &Flags) {
    st.m_crashed = false;
    let mut guard = 0;
    while st.m < MDONE {
        migrator_step(&mut st, f);
        check(&st, ws);
    }
    loop {
        let mut any = false;
        for i in 0..st.w.len() {
            if writer_step(&mut st, i) {
                any = true;
                check(&st, ws);
            }
        }
        while reader_step(&mut st, f) {
            any = true;
            check(&st, ws);
        }
        guard += 1;
        assert!(guard < 50);
        if !any {
            break;
        }
    }
    for (i, w) in st.w.iter().enumerate() {
        assert!(w.ph >= 2, "writer {i} never finished after migration completed");
        assert_eq!(w.ph == 2, st.order.contains(&i), "M-OUTCOME writer {i}");
    }
    assert!(st.root && st.moved);
}

fn explore(st: &St, ws: &[(usize, u8, u8)], f: &Flags, stats: &mut Stats) {
    if !stats.seen.insert(st.clone()) {
        return;
    }
    stats.nodes += 1;
    let mut any = false;
    for i in 0..st.w.len() {
        let mut n = st.clone();
        if writer_step(&mut n, i) {
            any = true;
            check(&n, ws);
            explore(&n, ws, f, stats);
        }
    }
    if st.m < MDONE && !st.m_crashed {
        let mut n = st.clone();
        migrator_step(&mut n, f);
        any = true;
        check(&n, ws);
        explore(&n, ws, f, stats);
        if !st.faulted {
            // crash: the migrator disappears here; it is re-run from step 0 at the end
            let mut c = st.clone();
            c.m_crashed = true;
            c.faulted = true;
            c.m = 0;
            explore(&c, ws, f, stats);
        }
    }
    let mut n = st.clone();
    if reader_step(&mut n, f) {
        any = true;
        check(&n, ws);
        explore(&n, ws, f, stats);
    }
    let _ = any;
    finish(st.clone(), ws, f);
}

fn run(f: Flags, reader: bool) -> u64 {
    let ws = vec![(0usize, 0u8, 1u8), (1, 0, 1), (0, 1, 2)];
    let st = St {
        heads: [0; Z],
        frozen: false,
        moved: false,
        ver: 0,
        shard: [None; Z],
        root: false,
        order: vec![],
        w: ws
            .iter()
            .map(|&(z, old, new)| W {
                z,
                old,
                new,
                ph: 0,
                ver: 0,
            })
            .collect(),
        m: 0,
        m_crashed: false,
        faulted: false,
        r: reader.then_some(R {
            k0: None,
            ph: 0,
            v: 0,
            res: None,
        }),
    };
    let mut stats = Stats {
        seen: HashSet::new(),
        nodes: 0,
    };
    explore(&st, &ws, &f, &mut stats);
    stats.nodes
}

#[test]
fn migration_loses_no_write_and_survives_a_crash_at_every_step() {
    let n = run(GOOD, true);
    println!("migration: {n} distinct states");
    assert!(n > 1000);
}

#[test]
#[should_panic(expected = "M-ATOMIC")]
fn mutation_no_freeze_loses_a_manifest_write() {
    run(Flags { freeze: false, ..GOOD }, false);
}

#[test]
#[should_panic(expected = "M-ROOT")]
fn mutation_root_before_shards_exposes_missing_shards() {
    run(
        Flags {
            shards_before_root: false,
            ..GOOD
        },
        false,
    );
}

/// Negative result of the model: the reader's re-check of `root` is NOT needed for safety (a read
/// that overlaps the cut-over may return the frozen manifest's value, which is a committed prefix
/// not older than its start); the code may skip it. Recorded as a passing test.
#[test]
fn reader_recheck_of_root_is_not_needed() {
    let n = run(
        Flags {
            reader_rechecks_root: false,
            ..GOOD
        },
        true,
    );
    println!("no recheck: {n} distinct states");
}
