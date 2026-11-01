//! Path-level sparse sets inside a zone: status, commit carry-over, switch/merge/rebase/stash/
//! restore behaviour, lazy fetch, workspace format, and randomized equivalence with a full checkout.
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use vcs_objects::{EntryMode, Object, ObjectId, Signature};
use vcs_remote::MemBackend;
use vcs_repo::meta::Workspace;
use vcs_repo::{MergeOutcome, Repo};

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}
fn write(r: &Repo, p: &str, c: &str) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
}
fn read(r: &Repo, p: &str) -> String {
    fs::read_to_string(r.worktree().join(p)).unwrap()
}
fn exists(r: &Repo, p: &str) -> bool {
    r.worktree().join(p).exists()
}
fn commit_all(r: &Repo, m: &str) {
    r.add(&["".into()]).unwrap();
    r.commit(m, sig()).unwrap();
}
fn s(p: &[&str]) -> Vec<String> {
    p.iter().map(|x| x.to_string()).collect()
}
fn clean(r: &Repo) {
    let st = r.status().unwrap();
    for z in &st.zones {
        assert!(z.staged.is_empty() && z.unstaged.is_empty(), "{:?} {:?}", z.staged, z.unstaged);
    }
    assert!(st.untracked.is_empty(), "{:?}", st.untracked);
}
fn fsck(r: &Repo) {
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}
fn head_root(r: &Repo) -> ObjectId {
    let h = r.refs().unwrap().head("root", "main").unwrap();
    r.log("root", None, 1).unwrap().into_iter().find(|(i, _)| *i == h).unwrap().1.root
}
fn tree_entries(r: &Repo, id: ObjectId) -> BTreeMap<String, (EntryMode, ObjectId)> {
    let Object::Tree(t) = r.store().get(&id).unwrap() else { panic!() };
    t.entries().iter().map(|e| (e.name.clone(), (e.mode, e.id))).collect()
}

fn base() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    write(&r, "a/x.txt", "ax\n");
    write(&r, "a/d/y.txt", "ay\n");
    write(&r, "b/x.txt", "bx\n");
    write(&r, "b/d/y.txt", "by\n");
    write(&r, "c.txt", "c\n");
    commit_all(&r, "base");
    (d, r)
}

#[test]
fn workspace_format_roundtrips_and_reads_legacy() {
    let mut ws = Workspace::default();
    ws.subs.insert("root".into(), "main".into());
    let legacy = ws.encode();
    assert_eq!(&legacy[..4], b"AGWS", "no sparse set: legacy bytes, readable by older binaries");
    assert!(Workspace::decode(&legacy).unwrap().sparse.is_empty());
    ws.sparse
        .insert("root".into(), ["a".to_string(), "b/d".to_string()].into_iter().collect());
    let v2 = ws.encode();
    assert_eq!(&v2[..4], b"AGW2");
    let back = Workspace::decode(&v2).unwrap();
    assert_eq!(back.sparse, ws.sparse);
    assert_eq!(back.subs, ws.subs);
    assert!(ws.skips("root", "b/x.txt") && !ws.skips("root", "b/d/y.txt") && !ws.skips("root", "a"));
    assert!(ws.skips("root", "ab/x"), "prefix match is on path components");
    // truncated v2 is corrupt, not silently legacy
    assert!(Workspace::decode(&v2[..v2.len() - 2]).is_err());
}

#[test]
fn set_materializes_only_listed_paths_and_status_is_clean() {
    let (_d, r) = base();
    let rep = r.sparse_set(None, "root", &s(&["a"])).unwrap();
    assert_eq!((rep.materialized, rep.dematerialized), (0, 3));
    assert!(exists(&r, "a/x.txt") && exists(&r, "a/d/y.txt"));
    assert!(!exists(&r, "b") && !exists(&r, "c.txt"));
    clean(&r);
    // `add .` must not treat unmaterialized files as deletions
    let add = r.add(&["".into()]).unwrap();
    assert_eq!(add.removed, 0);
    clean(&r);
    assert!(matches!(r.commit("x", sig()), Err(vcs_repo::Error::NothingToCommit)));
    assert_eq!(r.sparse_list().unwrap()["root"], s(&["a"]));
    fsck(&r);
}

#[test]
fn commit_carries_unmaterialized_paths_over_unchanged() {
    let (_d, r) = base();
    let before = tree_entries(&r, head_root(&r));
    r.sparse_set(None, "root", &s(&["a/d"])).unwrap();
    write(&r, "a/d/y.txt", "changed\n");
    write(&r, "a/d/new.txt", "new\n");
    commit_all(&r, "edit");
    let after = tree_entries(&r, head_root(&r));
    assert_eq!(after["b"], before["b"], "untouched subtree spliced by id");
    assert_eq!(after["c.txt"], before["c.txt"]);
    assert_ne!(after["a"], before["a"]);
    let a = tree_entries(&r, after["a"].1);
    let old_a = tree_entries(&r, before["a"].1);
    assert_eq!(a["x.txt"], old_a["x.txt"], "sibling of the sparse dir kept");
    clean(&r);
    r.sparse_disable(None, "root").unwrap();
    assert_eq!(read(&r, "b/d/y.txt"), "by\n");
    assert_eq!(read(&r, "a/d/y.txt"), "changed\n");
    clean(&r);
    fsck(&r);
}

#[test]
fn deleting_materialized_file_is_a_deletion_but_siblings_are_not() {
    let (_d, r) = base();
    r.sparse_set(None, "root", &s(&["a"])).unwrap();
    fs::remove_file(r.worktree().join("a/x.txt")).unwrap();
    let st = r.status().unwrap();
    assert_eq!(st.zones[0].unstaged.len(), 1);
    assert_eq!(st.zones[0].unstaged[0].path, "a/x.txt");
    commit_all(&r, "rm");
    let t = tree_entries(&r, head_root(&r));
    assert!(t.contains_key("b") && t.contains_key("c.txt"));
}

#[test]
fn add_and_restore_outside_the_set_are_refused() {
    let (_d, r) = base();
    r.sparse_set(None, "root", &s(&["a"])).unwrap();
    write(&r, "b/x.txt", "stray\n");
    let e = r.add(&s(&["b/x.txt"])).unwrap_err().to_string();
    assert!(e.contains("sparse"), "{e}");
    let e = r.restore(&s(&["b"]), false).unwrap_err().to_string();
    assert!(e.contains("sparse"), "{e}");
    let e = r.rm(&s(&["b/x.txt"]), false, true).unwrap_err().to_string();
    assert!(e.contains("sparse"), "{e}");
    let e = r.mv("b/x.txt", "a/z.txt", false).unwrap_err().to_string();
    assert!(e.contains("sparse"), "{e}");
    let e = r.mv("a/x.txt", "b/z.txt", false).unwrap_err().to_string();
    assert!(e.contains("sparse"), "{e}");
}

#[test]
fn set_add_remove_disable_roundtrip_and_refuse_dirty() {
    let (_d, r) = base();
    r.sparse_set(None, "root", &s(&["a"])).unwrap();
    write(&r, "a/x.txt", "dirty\n");
    assert!(r.sparse_add(None, "root", &s(&["b"])).is_err(), "dirty zone refuses");
    assert!(!exists(&r, "b"));
    commit_all(&r, "d");
    let rep = r.sparse_add(None, "root", &s(&["b/d"])).unwrap();
    assert_eq!(rep.materialized, 1);
    assert_eq!(read(&r, "b/d/y.txt"), "by\n");
    assert!(!exists(&r, "b/x.txt"));
    clean(&r);
    let rep = r.sparse_remove("root", &s(&["a"])).unwrap();
    assert_eq!(rep.dematerialized, 2);
    assert!(!exists(&r, "a"), "emptied directories are pruned");
    assert!(r.sparse_remove("root", &s(&["nope"])).is_err());
    assert!(r.sparse_set(None, "root", &s(&["../x"])).is_err());
    r.sparse_disable(None, "root").unwrap();
    assert!(r.sparse_list().unwrap().is_empty());
    assert_eq!(read(&r, "c.txt"), "c\n");
    clean(&r);
    fsck(&r);
}

#[test]
fn untracked_file_in_the_way_blocks_materialization() {
    let (_d, r) = base();
    r.sparse_set(None, "root", &s(&["a"])).unwrap();
    write(&r, "b/x.txt", "mine\n");
    assert!(r.sparse_add(None, "root", &s(&["b"])).is_err());
    assert_eq!(read(&r, "b/x.txt"), "mine\n");
}

#[test]
fn branch_switch_updates_only_the_materialized_part() {
    let (_d, r) = base();
    r.create_branch("root", "f").unwrap();
    r.checkout("root", "f").unwrap();
    write(&r, "a/x.txt", "af\n");
    write(&r, "b/x.txt", "bf\n");
    fs::remove_file(r.worktree().join("c.txt")).unwrap();
    commit_all(&r, "f");
    r.checkout("root", "main").unwrap();
    r.sparse_set(None, "root", &s(&["a"])).unwrap();
    r.checkout("root", "f").unwrap();
    assert_eq!(read(&r, "a/x.txt"), "af\n");
    assert!(!exists(&r, "b") && !exists(&r, "c.txt"));
    clean(&r);
    // committing on f from the sparse checkout keeps f's b/ and the deletion of c.txt
    write(&r, "a/d/y.txt", "again\n");
    commit_all(&r, "more");
    let t = tree_entries(&r, head_root_of(&r, "f"));
    assert!(!t.contains_key("c.txt"));
    let b = tree_entries(&r, t["b"].1);
    assert_eq!(r.store().get_content(&b["x.txt"].1).unwrap(), b"bf\n");
    r.checkout("root", "main").unwrap();
    assert_eq!(read(&r, "a/x.txt"), "ax\n");
    clean(&r);
    r.sparse_disable(None, "root").unwrap();
    assert_eq!(read(&r, "c.txt"), "c\n");
    assert_eq!(read(&r, "b/x.txt"), "bx\n");
    clean(&r);
}

#[test]
fn merge_clean_outside_set_applies_and_conflict_outside_set_fails_cleanly() {
    let (_d, r) = base();
    r.create_branch("root", "f").unwrap();
    r.checkout("root", "f").unwrap();
    write(&r, "b/d/y.txt", "theirs\n");
    write(&r, "a/d/y.txt", "a-theirs\n");
    commit_all(&r, "f");
    r.checkout("root", "main").unwrap();
    write(&r, "b/x.txt", "ours\n");
    commit_all(&r, "m");
    r.sparse_set(None, "root", &s(&["a"])).unwrap();
    // non-conflicting change outside the set merges without touching the disk
    let out = r.merge("root", "f", sig()).unwrap();
    assert!(matches!(out, MergeOutcome::Merged { .. }), "{out:?}");
    assert!(!exists(&r, "b"));
    assert_eq!(read(&r, "a/d/y.txt"), "a-theirs\n");
    clean(&r);
    let t = tree_entries(&r, head_root(&r));
    let b = tree_entries(&r, t["b"].1);
    assert_eq!(r.store().get_content(&b["x.txt"].1).unwrap(), b"ours\n");
    let bd = tree_entries(&r, b["d"].1);
    assert_eq!(r.store().get_content(&bd["y.txt"].1).unwrap(), b"theirs\n");
    fsck(&r);

    // now a conflict outside the set: refused before anything changes
    r.create_branch("root", "g").unwrap();
    r.checkout("root", "g").unwrap();
    r.sparse_add(None, "root", &s(&["b"])).unwrap();
    write(&r, "b/x.txt", "g-version\n");
    commit_all(&r, "g");
    r.checkout("root", "main").unwrap();
    r.sparse_set(None, "root", &s(&["a"])).unwrap();
    r.sparse_add(None, "root", &s(&["b"])).unwrap();
    write(&r, "b/x.txt", "main-version\n");
    commit_all(&r, "main edit");
    r.sparse_set(None, "root", &s(&["a"])).unwrap();
    let head = r.refs().unwrap().head("root", "main");
    let err = r.merge("root", "g", sig()).unwrap_err().to_string();
    assert!(err.contains("sparse") && err.contains("b/x.txt"), "{err}");
    assert_eq!(r.refs().unwrap().head("root", "main"), head);
    assert!(r.merge_state().unwrap().is_none());
    clean(&r);
    // after adding the path the merge reports a normal conflict
    r.sparse_add(None, "root", &s(&["b"])).unwrap();
    let out = r.merge("root", "g", sig()).unwrap();
    assert!(matches!(out, MergeOutcome::Conflicted { .. }), "{out:?}");
    r.merge_abort().unwrap();
    clean(&r);
}

#[test]
fn rebase_and_stash_respect_the_set() {
    let (_d, r) = base();
    r.create_branch("root", "f").unwrap();
    r.checkout("root", "f").unwrap();
    write(&r, "a/x.txt", "f-a\n");
    commit_all(&r, "f1");
    r.checkout("root", "main").unwrap();
    write(&r, "b/x.txt", "main-b\n");
    commit_all(&r, "m1");
    r.sparse_set(None, "root", &s(&["a"])).unwrap();
    r.checkout("root", "f").unwrap();
    let out = r.rebase("root", "main", sig()).unwrap();
    assert!(matches!(out, vcs_repo::RebaseOutcome::Done { .. }), "{out:?}");
    clean(&r);
    assert_eq!(read(&r, "a/x.txt"), "f-a\n");
    assert!(!exists(&r, "b"));
    let t = tree_entries(&r, head_root_of(&r, "f"));
    let b = tree_entries(&r, t["b"].1);
    assert_eq!(r.store().get_content(&b["x.txt"].1).unwrap(), b"main-b\n");

    // stash: only materialized work is stashed; unmaterialized paths are untouched
    write(&r, "a/x.txt", "wip\n");
    r.stash_push(None, sig()).unwrap();
    assert_eq!(read(&r, "a/x.txt"), "f-a\n");
    clean(&r);
    assert!(matches!(r.stash_pop(0).unwrap(), vcs_repo::StashPopOutcome::Applied));
    assert_eq!(read(&r, "a/x.txt"), "wip\n");
    assert!(!exists(&r, "b"));
    let st = r.status().unwrap();
    assert!(st.zones[0].staged.iter().all(|c| c.path.starts_with("a/")));
}

fn head_root_of(r: &Repo, branch: &str) -> ObjectId {
    let h = r.refs().unwrap().head("root", branch).unwrap();
    match r.store().get(&h).unwrap() {
        Object::Commit(c) => c.root,
        _ => panic!(),
    }
}

#[test]
fn stash_pop_with_changes_outside_the_set_is_refused() {
    let (_d, r) = base();
    write(&r, "b/x.txt", "wip-b\n");
    r.stash_push(None, sig()).unwrap();
    r.sparse_set(None, "root", &s(&["a"])).unwrap();
    let e = r.stash_pop(0).unwrap_err().to_string();
    assert!(e.contains("sparse"), "{e}");
    assert_eq!(r.stash_list().unwrap().len(), 1, "stash entry kept");
    clean(&r);
}

#[test]
fn zone_remove_drops_the_set_and_paths_must_be_inside_the_zone() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    r.create_zone("lib", "lib").unwrap();
    write(&r, "lib/a/x", "1");
    write(&r, "lib/b/y", "2");
    commit_all(&r, "init");
    r.sparse_set(None, "lib", &s(&["lib/a"])).unwrap();
    assert!(exists(&r, "lib/a/x") && !exists(&r, "lib/b"));
    assert!(r.sparse_set(None, "lib", &s(&["other"])).is_err(), "path outside the zone");
    r.zone_remove("lib").unwrap();
    assert!(r.sparse_list().unwrap().is_empty());
    r.zone_add("lib", None).unwrap();
    assert!(exists(&r, "lib/b/y"), "re-subscribing materializes the whole zone");
}

#[test]
fn sync_and_sparse_add_fetch_only_needed_blobs() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let a = Repo::init(&d.path().join("a")).unwrap();
    a.create_zone("engine", "engine").unwrap();
    let body = |i: usize, v: &str| format!("file {i} {v} {}", "x".repeat(2000));
    for v in ["v1", "v2"] {
        for i in 0..6 {
            write(&a, &format!("engine/d{}/f{i}.rs", i % 2), &body(i, v));
        }
        commit_all(&a, v);
    }
    a.publish(&remote, None, sig()).unwrap();
    let blob = |i: usize, v: &str| Object::Blob(body(i, v).into_bytes()).id();
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), Some(&["engine".to_string()])).unwrap();
    b.set_remote("mem").ok();
    b.sparse_set(None, "engine", &s(&["engine/d0"])).unwrap();
    for i in 0..6 {
        write(&a, &format!("engine/d{}/f{i}.rs", i % 2), &body(i, "v3"));
    }
    commit_all(&a, "v3");
    a.publish(&remote, None, sig()).unwrap();
    let rep = b.sync(&remote).unwrap();
    assert!(
        matches!(rep.actions[0].2, vcs_repo::SyncAction::FastForwarded { .. }),
        "{:?}",
        rep.actions
    );
    for i in 0..6 {
        let have = b.store().contains(&blob(i, "v3")).unwrap();
        assert_eq!(have, i % 2 == 0, "file {i}: only d0 blobs are fetched");
    }
    assert_eq!(read(&b, "engine/d0/f0.rs"), body(0, "v3"));
    assert!(!exists(&b, "engine/d1"));
    clean(&b);
    // widening fetches exactly the newly needed blobs
    b.sparse_add(Some(&remote), "engine", &s(&["engine/d1"])).unwrap();
    assert_eq!(read(&b, "engine/d1/f1.rs"), body(1, "v3"));
    clean(&b);
    fsck(&b);
}

// ---------- randomized equivalence ----------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn files_under(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    for e in fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        if rel == ".vcs" {
            continue;
        }
        if e.file_type().unwrap().is_dir() {
            files_under(root, &p, out);
        } else {
            out.insert(rel, fs::read_to_string(&p).unwrap());
        }
    }
}

const UNIVERSE: &[&str] = &[
    "a/x.txt",
    "a/y.txt",
    "a/d/p.txt",
    "b/x.txt",
    "b/d/q.txt",
    "b/e/r.txt",
    "c.txt",
    "a/d/e/s.txt",
];
const SET: &[&str] = &["a/d", "a/y.txt", "b/e"];

fn in_set(p: &str) -> bool {
    SET.iter().any(|s| p == *s || p.starts_with(&format!("{s}/")))
}

fn equivalence_run(seed: u64) {
    let d = tempfile::tempdir().unwrap();
    let full = Repo::init(&d.path().join("full")).unwrap();
    let sp = Repo::init(&d.path().join("sp")).unwrap();
    let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
    for p in UNIVERSE {
        for r in [&full, &sp] {
            write(r, p, &format!("init {p}\nl2\nl3\nl4\n"));
        }
    }
    for r in [&full, &sp] {
        commit_all(r, "init");
    }
    sp.sparse_set(None, "root", &s(SET)).unwrap();
    let branches = ["main", "f1", "f2"];
    for b in &branches[1..] {
        for r in [&full, &sp] {
            r.create_branch("root", b).unwrap();
        }
    }
    let mut cur = "main".to_string();
    for step in 0..14 {
        let target = branches[rng.below(3)];
        if target != cur {
            for r in [&full, &sp] {
                r.checkout("root", target).unwrap();
            }
            cur = target.to_string();
        }
        if rng.below(4) == 0 {
            // merge another branch: same outcome class in both repos
            let other = branches[rng.below(3)];
            if other == cur {
                continue;
            }
            let fo = full.merge("root", other, sig()).unwrap();
            match fo {
                MergeOutcome::Conflicted { paths } => {
                    let so = sp.merge("root", other, sig());
                    if paths.iter().all(|p| in_set(p)) {
                        assert!(matches!(so, Ok(MergeOutcome::Conflicted { .. })), "seed {seed} step {step}: {so:?}");
                        sp.merge_abort().unwrap();
                    } else {
                        assert!(so.is_err(), "seed {seed} step {step}: conflict outside the set must be refused");
                    }
                    full.merge_abort().unwrap();
                }
                MergeOutcome::Merged { commit } => {
                    let _ = commit;
                    assert!(
                        matches!(sp.merge("root", other, sig()), Ok(MergeOutcome::Merged { .. })),
                        "seed {seed} step {step}"
                    );
                }
                MergeOutcome::FastForward { to, .. } => {
                    let _ = to;
                    assert!(
                        matches!(sp.merge("root", other, sig()), Ok(MergeOutcome::FastForward { .. })),
                        "seed {seed} step {step}"
                    );
                }
                MergeOutcome::UpToDate => {
                    assert!(matches!(sp.merge("root", other, sig()), Ok(MergeOutcome::UpToDate)));
                }
                MergeOutcome::Squashed { .. } => unreachable!("plain merge never squashes"),
            }
        } else {
            // edit 1-3 files, commit. Out-of-set edits are made in the sparse repo by temporarily
            // widening the set (the documented workflow), then narrowing it again.
            let n = 1 + rng.below(3);
            let mut edits = Vec::new();
            for _ in 0..n {
                let p = UNIVERSE[rng.below(UNIVERSE.len())];
                let op = rng.below(4);
                edits.push((p, op, rng.next()));
            }
            let outside: Vec<String> = edits.iter().filter(|(p, ..)| !in_set(p)).map(|(p, ..)| p.to_string()).collect();
            if !outside.is_empty() {
                sp.sparse_disable(None, "root").unwrap();
            }
            for r in [&full, &sp] {
                for (p, op, v) in &edits {
                    let abs = r.worktree().join(p);
                    match op {
                        0 if abs.exists() => fs::remove_file(&abs).unwrap(),
                        _ => write(
                            r,
                            p,
                            &format!(
                                "init {p}\nl2 {}\nl3\nl4 {}\n",
                                if *op == 1 { *v % 5 } else { 0 },
                                if *op == 2 { *v % 5 } else { 0 }
                            ),
                        ),
                    }
                }
            }
            let fa = full.add(&["".into()]).unwrap();
            let _ = fa;
            sp.add(&["".into()]).unwrap();
            let fc = full.commit(&format!("c{step}"), sig());
            let sc = sp.commit(&format!("c{step}"), sig());
            assert_eq!(fc.is_ok(), sc.is_ok(), "seed {seed} step {step}");
            if !outside.is_empty() {
                sp.sparse_set(None, "root", &s(SET)).unwrap();
            }
        }
        // invariants after every step
        for b in branches {
            assert_eq!(
                head_root_of(&full, b),
                head_root_of(&sp, b),
                "seed {seed} step {step}: branch {b} must have identical trees"
            );
        }
        let mut fw = BTreeMap::new();
        files_under(full.worktree(), full.worktree(), &mut fw);
        fw.retain(|p, _| in_set(p));
        let mut sw = BTreeMap::new();
        files_under(sp.worktree(), sp.worktree(), &mut sw);
        assert_eq!(fw, sw, "seed {seed} step {step}: sparse worktree == full restricted to the set");
        clean(&sp);
    }
    fsck(&sp);
}

#[test]
fn sparse_operations_equal_full_checkout_restricted_to_the_set() {
    for seed in 1..=12 {
        equivalence_run(seed);
    }
}
