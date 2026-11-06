//! `rebase --rebase-merges` (merge commits recreated) and merges of criss-cross histories
//! (a virtual merge base). The twin tests against real git live in vcs-cli/tests/gitmap.rs.
use std::fs;
use vcs_objects::{ObjectId, Signature};
use vcs_repo::{MergeOutcome, RebaseOutcome, Repo, Resolution};

fn sig(ts: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: ts,
    }
}
fn setup() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    (d, r)
}
fn write(r: &Repo, p: &str, c: &str) {
    fs::write(r.worktree().join(p), c).unwrap();
}
fn read(r: &Repo, p: &str) -> String {
    fs::read_to_string(r.worktree().join(p)).unwrap()
}
fn exists(r: &Repo, p: &str) -> bool {
    r.worktree().join(p).exists()
}
fn commit(r: &Repo, m: &str) -> ObjectId {
    r.add(&["".to_string()]).unwrap();
    r.commit(m, sig(1000)).unwrap().commits[0].2
}
fn head(r: &Repo, b: &str) -> ObjectId {
    r.refs().unwrap().head("root", b).unwrap()
}
fn fsck(r: &Repo) {
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}
fn parents(r: &Repo, id: &ObjectId) -> Vec<ObjectId> {
    r.commit_of(id).unwrap().parents
}
fn subject(r: &Repo, id: &ObjectId) -> String {
    r.commit_of(id).unwrap().message.lines().next().unwrap().to_owned()
}
fn merge(r: &Repo, b: &str) -> MergeOutcome {
    r.merge("root", b, sig(2000)).unwrap()
}
fn files(r: &Repo) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = fs::read_dir(r.worktree())
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().unwrap().is_file())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), fs::read_to_string(e.path()).unwrap()))
        .collect();
    v.sort();
    v
}

const BASE: &str = "1\n2\n3\n4\n5\n6\n7\n8\n";

/// main: base; feature: a, b, merge(side: s), c; main moved on (h). `feature` is checked out.
fn merge_range() -> (tempfile::TempDir, Repo) {
    let (d, r) = setup();
    write(&r, "f", BASE);
    commit(&r, "base");
    r.create_branch("root", "feature").unwrap();
    r.checkout("root", "feature").unwrap();
    write(&r, "a", "a\n");
    commit(&r, "add a");
    r.create_branch("root", "side").unwrap();
    r.checkout("root", "side").unwrap();
    write(&r, "s", "s\n");
    commit(&r, "add s");
    r.checkout("root", "feature").unwrap();
    write(&r, "b", "b\n");
    commit(&r, "add b");
    assert!(matches!(merge(&r, "side"), MergeOutcome::Merged { .. }));
    write(&r, "c", "c\n");
    commit(&r, "add c");
    r.checkout("root", "main").unwrap();
    write(&r, "h", "h\n");
    commit(&r, "main h");
    r.checkout("root", "feature").unwrap();
    (d, r)
}

#[test]
fn linear_rebase_still_refuses_a_merge_and_points_at_the_flag() {
    let (_d, r) = merge_range();
    let before = head(&r, "feature");
    let e = r.rebase("root", "main", sig(3000)).unwrap_err().to_string();
    assert!(e.contains("parents") && e.contains("--rebase-merges"), "{e}");
    assert_eq!(head(&r, "feature"), before);
}

#[test]
fn merge_commits_are_recreated_and_undone_as_one_operation() {
    let (_d, r) = merge_range();
    let old = head(&r, "feature");
    let old_merge = parents(&r, &old)[0];
    let old_cid = r.commit_of(&old_merge).unwrap().change_id;
    let side = head(&r, "side");
    let out = r.rebase_merges("root", "main", None, false, sig(3000)).unwrap();
    let RebaseOutcome::Done { from, to, replayed } = out else {
        panic!("{out:?}")
    };
    assert_eq!((from, replayed), (old, 5)); // a, s, b, the merge, c
    assert_eq!(to, head(&r, "feature"));
    // c on top of the recreated merge, which has two parents: (b rebased, side)
    let merge_id = parents(&r, &to)[0];
    assert_eq!(
        subject(&r, &merge_id),
        r.commit_of(&old_merge).unwrap().message.lines().next().unwrap()
    );
    assert_eq!(r.commit_of(&merge_id).unwrap().change_id, old_cid, "change id survives");
    let mp = parents(&r, &merge_id);
    assert_eq!(mp.len(), 2);
    assert_eq!(subject(&r, &mp[0]), "add b");
    assert_eq!(subject(&r, &mp[1]), "add s");
    // the side commit sits on the new base too (it descends from the merge base)
    assert_ne!(mp[1], side);
    assert_eq!(subject(&r, &parents(&r, &mp[1])[0]), "add a");
    // the working tree has everything
    let names: Vec<String> = files(&r).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["a", "b", "c", "f", "h", "s"]);
    assert_eq!(head(&r, "side"), side, "the side branch is not moved");
    fsck(&r);
    r.op_undo().unwrap();
    assert_eq!(head(&r, "feature"), old);
    assert!(!exists(&r, "h"));
}

#[test]
fn up_to_date_and_fast_forward() {
    let (_d, r) = merge_range();
    r.checkout("root", "main").unwrap();
    r.create_branch("root", "tip").unwrap();
    r.checkout("root", "tip").unwrap();
    assert_eq!(
        r.rebase_merges("root", "main", None, false, sig(3)).unwrap(),
        RebaseOutcome::UpToDate
    );
}

/// root: g (8 lines). topic: t1 edits line 2. main: m1. topic merges main, t2. main: m2 edits line 2.
fn conflict_merge() -> (tempfile::TempDir, Repo) {
    let (d, r) = setup();
    write(&r, "g", BASE);
    commit(&r, "root");
    r.create_branch("root", "topic").unwrap();
    r.checkout("root", "topic").unwrap();
    write(&r, "g", "1\nTOPIC\n3\n4\n5\n6\n7\n8\n");
    commit(&r, "t1");
    r.checkout("root", "main").unwrap();
    write(&r, "m1", "m1\n");
    commit(&r, "m1");
    r.checkout("root", "topic").unwrap();
    assert!(matches!(merge(&r, "main"), MergeOutcome::Merged { .. }));
    write(&r, "t2", "t2\n");
    commit(&r, "t2");
    r.checkout("root", "main").unwrap();
    write(&r, "g", "1\nMAIN\n3\n4\n5\n6\n7\n8\n");
    commit(&r, "m2");
    r.checkout("root", "topic").unwrap();
    (d, r)
}

#[test]
fn conflict_in_a_recreated_merge_continue() {
    let (_d, r) = conflict_merge();
    let old = head(&r, "topic");
    let t1 = parents(&r, &parents(&r, &old)[0])[0];
    let out = r.rebase_merges("root", "main", None, false, sig(3000)).unwrap();
    let RebaseOutcome::Conflicted { paths, .. } = out else {
        panic!("{out:?}")
    };
    assert_eq!(paths, ["g"]);
    assert!(read(&r, "g").contains("<<<<<<<"));
    assert!(r.rebase_state().unwrap().is_some());
    // the branch sits on the kept first parent while stopped
    assert_eq!(head(&r, "topic"), t1);
    let e = r.rebase_continue(sig(3001)).unwrap_err().to_string();
    assert!(e.contains("unresolved"), "{e}");
    write(&r, "g", "1\nBOTH\n3\n4\n5\n6\n7\n8\n");
    r.resolve("g", &Resolution::Mark).unwrap();
    r.add(&["g".to_string()]).unwrap();
    let out = r.rebase_continue(sig(3002)).unwrap();
    let RebaseOutcome::Done { to, .. } = out else { panic!("{out:?}") };
    assert_eq!(subject(&r, &to), "t2");
    let m = parents(&r, &to)[0];
    let mp = parents(&r, &m);
    assert_eq!(mp[0], t1, "the cousin t1 keeps its place");
    assert_eq!(subject(&r, &mp[1]), "m2", "the upstream parent is replaced by the new base");
    assert_eq!(read(&r, "g"), "1\nBOTH\n3\n4\n5\n6\n7\n8\n");
    assert!(read(&r, "t2") == "t2\n" && exists(&r, "m1"));
    assert!(r.rebase_state().unwrap().is_none() && r.merge_state().unwrap().is_none());
    fsck(&r);
}

#[test]
fn conflict_in_a_recreated_merge_abort_and_skip() {
    let (_d, r) = conflict_merge();
    let old = head(&r, "topic");
    r.rebase_merges("root", "main", None, false, sig(3000)).unwrap();
    r.rebase_abort().unwrap();
    assert_eq!(head(&r, "topic"), old);
    assert_eq!(read(&r, "g"), "1\nTOPIC\n3\n4\n5\n6\n7\n8\n");
    assert!(r.rebase_state().unwrap().is_none());
    fsck(&r);
    // skip: the merge is dropped, t2 lands on the first parent
    r.rebase_merges("root", "main", None, false, sig(3000)).unwrap();
    let out = r.rebase_skip(sig(3001), &mut |m: &str| Ok(m.to_owned())).unwrap();
    let RebaseOutcome::Done { to, .. } = out else { panic!("{out:?}") };
    assert_eq!(subject(&r, &to), "t2");
    assert_eq!(subject(&r, &parents(&r, &to)[0]), "t1");
    assert_eq!(read(&r, "g"), "1\nTOPIC\n3\n4\n5\n6\n7\n8\n");
    assert!(!exists(&r, "m1"));
    fsck(&r);
}

#[test]
fn rebase_cousins_replays_the_commit_before_the_merge_base_too() {
    let (_d, r) = conflict_merge();
    // main2 is a clean target: m2 conflicts with t1, so drop that edit from the picture
    r.checkout("root", "main").unwrap();
    write(&r, "g", BASE);
    commit(&r, "m3 restores g");
    r.checkout("root", "topic").unwrap();
    let out = r.rebase_merges("root", "main", None, true, sig(3000)).unwrap();
    let RebaseOutcome::Done { to, .. } = out else { panic!("{out:?}") };
    // t1 was replayed onto main, so the merge of main into it is "already up to date" and, as in git,
    // disappears: t2 sits directly on the replayed t1
    let t1 = parents(&r, &to)[0];
    assert_eq!(subject(&r, &t1), "t1");
    assert_eq!(parents(&r, &t1).len(), 1);
    assert_eq!(subject(&r, &parents(&r, &t1)[0]), "m3 restores g");
    assert_eq!(read(&r, "g"), "1\nTOPIC\n3\n4\n5\n6\n7\n8\n");
}

#[test]
fn onto_with_an_upstream_and_root() {
    let (_d, r) = merge_range();
    // replay only what `side` lacks of feature (a, b, merge, c minus nothing) onto main
    let out = r.rebase_merges("root", "main", Some(Some("side")), false, sig(3000)).unwrap();
    assert!(matches!(out, RebaseOutcome::Done { .. }), "{out:?}");
    fsck(&r);
    r.op_undo().unwrap();
    // --root: everything, root commit included
    let out = r.rebase_merges("root", "main", Some(None), false, sig(3001)).unwrap();
    assert!(matches!(out, RebaseOutcome::Done { .. }), "{out:?}");
    fsck(&r);
}

/// Criss-cross: X deletes e, Y deletes d; each branch merges the other; then main re-adds both.
fn criss_cross() -> (tempfile::TempDir, Repo) {
    let (d, r) = setup();
    write(&r, "d", "d\n");
    write(&r, "e", "e\n");
    write(&r, "f", BASE);
    commit(&r, "root");
    r.create_branch("root", "other").unwrap();
    r.checkout("root", "other").unwrap();
    fs::remove_file(r.worktree().join("e")).unwrap();
    commit(&r, "x deletes e");
    r.create_branch("root", "xs").unwrap();
    r.checkout("root", "main").unwrap();
    fs::remove_file(r.worktree().join("d")).unwrap();
    commit(&r, "y deletes d");
    r.create_branch("root", "ys").unwrap();
    r.checkout("root", "other").unwrap();
    assert!(matches!(merge(&r, "ys"), MergeOutcome::Merged { .. }));
    r.checkout("root", "main").unwrap();
    assert!(matches!(merge(&r, "xs"), MergeOutcome::Merged { .. }));
    (d, r)
}

#[test]
fn criss_cross_merge_uses_a_virtual_base() {
    let (_d, r) = criss_cross();
    assert_eq!(r.merge_bases(&head(&r, "main"), &head(&r, "other")).unwrap().len(), 2);
    write(&r, "d", "d again\n");
    write(&r, "e", "e again\n");
    write(&r, "f", "1\nMAIN\n3\n4\n5\n6\n7\n8\n");
    commit(&r, "main re-adds");
    r.checkout("root", "other").unwrap();
    write(&r, "f", "1\n2\n3\n4\n5\n6\n7\nOTHER\n");
    commit(&r, "other edit");
    r.checkout("root", "main").unwrap();
    // with either single base this would be a modify/delete conflict on d or e
    assert!(matches!(merge(&r, "other"), MergeOutcome::Merged { .. }));
    assert_eq!(read(&r, "d"), "d again\n");
    assert_eq!(read(&r, "e"), "e again\n");
    assert_eq!(read(&r, "f"), "1\nMAIN\n3\n4\n5\n6\n7\nOTHER\n");
    fsck(&r);
}

#[test]
fn criss_cross_with_conflicting_bases_uses_marker_text_as_the_base() {
    let (_d, r) = setup();
    write(&r, "f", BASE);
    commit(&r, "root");
    r.create_branch("root", "other").unwrap();
    r.checkout("root", "other").unwrap();
    write(&r, "f", "1\nX\n3\n4\n5\n6\n7\n8\n");
    commit(&r, "x");
    r.create_branch("root", "xs").unwrap();
    r.checkout("root", "main").unwrap();
    write(&r, "f", "1\nY\n3\n4\n5\n6\n7\n8\n");
    commit(&r, "y");
    r.create_branch("root", "ys").unwrap();
    // each side merges the other and resolves differently
    r.checkout("root", "other").unwrap();
    assert!(matches!(merge(&r, "ys"), MergeOutcome::Conflicted { .. }));
    write(&r, "f", "1\nXY\n3\n4\n5\n6\n7\n8\n");
    r.resolve("f", &Resolution::Mark).unwrap();
    r.add(&["f".to_string()]).unwrap();
    r.commit("merge y into other", sig(5)).unwrap();
    r.checkout("root", "main").unwrap();
    assert!(matches!(merge(&r, "xs"), MergeOutcome::Conflicted { .. }));
    write(&r, "f", "1\nYX\n3\n4\n5\n6\n7\n8\n");
    r.resolve("f", &Resolution::Mark).unwrap();
    r.add(&["f".to_string()]).unwrap();
    r.commit("merge x into main", sig(6)).unwrap();
    // the two merges disagree on line 2: a real conflict, but the merge runs (no refusal)
    let out = merge(&r, "other");
    let MergeOutcome::Conflicted { paths } = out else {
        panic!("{out:?}")
    };
    assert_eq!(paths, ["f"]);
    r.merge_abort().unwrap();
    fsck(&r);
}
