use std::fs;
use vcs_objects::{ObjectId, Signature};
use vcs_repo::{MergeOutcome, Repo};

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}
fn setup() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    (d, r)
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
fn commit(r: &Repo, m: &str) -> ObjectId {
    r.add(&["".to_string()]).unwrap();
    r.commit(m, sig()).unwrap().commits[0].2
}
fn head(r: &Repo) -> (ObjectId, vcs_objects::Commit) {
    r.log("root", None, 1).unwrap().remove(0)
}
fn clean(r: &Repo) -> bool {
    let s = r.status().unwrap();
    s.untracked.is_empty() && s.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty()) && s.merging.is_none()
}
fn fsck(r: &Repo) {
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}
fn merged(o: MergeOutcome) -> ObjectId {
    match o {
        MergeOutcome::Merged { commit } => commit,
        o => panic!("expected Merged, got {o:?}"),
    }
}

/// main: base(f.txt=1..3). feature: one commit changing line 3 and adding g.txt. main checked out.
fn with_feature() -> (tempfile::TempDir, Repo, ObjectId) {
    let (d, r) = setup();
    write(&r, "f.txt", "1\n2\n3\n");
    commit(&r, "base");
    r.create_branch("root", "feature").unwrap();
    r.checkout("root", "feature").unwrap();
    write(&r, "f.txt", "1\n2\nthree\n");
    write(&r, "g.txt", "g\n");
    let c = commit(&r, "feature work");
    r.checkout("root", "main").unwrap();
    (d, r, c)
}

#[test]
fn cherry_pick_clean_makes_single_parent_commit() {
    let (_d, r, c) = with_feature();
    write(&r, "f.txt", "one\n2\n3\n");
    let before = commit(&r, "main edit");
    let new = merged(r.cherry_pick("root", &c.to_string()[..8], sig()).unwrap());
    let (id, cm) = head(&r);
    assert_eq!(id, new);
    assert_eq!(cm.parents, vec![before]);
    assert!(cm.message.starts_with("feature work"));
    assert!(cm.message.contains(&format!("cherry picked from commit {c}")));
    assert_eq!(read(&r, "f.txt"), "one\n2\nthree\n");
    assert_eq!(read(&r, "g.txt"), "g\n");
    assert!(clean(&r));
    fsck(&r);
}

#[test]
fn cherry_pick_by_branch_name_and_op_undo() {
    let (_d, r, _) = with_feature();
    let before = head(&r).0;
    merged(r.cherry_pick("root", "feature", sig()).unwrap());
    assert!(exists(&r, "g.txt"));
    r.op_undo().unwrap();
    assert_eq!(head(&r).0, before);
    assert!(!exists(&r, "g.txt"));
    assert_eq!(read(&r, "f.txt"), "1\n2\n3\n");
    assert!(clean(&r));
}

#[test]
fn cherry_pick_conflict_persists_then_commits_single_parent() {
    let (_d, r, c) = with_feature();
    write(&r, "f.txt", "1\n2\nTHREE\n");
    let before = commit(&r, "main edit");
    match r.cherry_pick("root", &c.to_string(), sig()).unwrap() {
        MergeOutcome::Conflicted { paths } => assert_eq!(paths, vec!["f.txt".to_string()]),
        o => panic!("{o:?}"),
    }
    assert!(read(&r, "f.txt").contains("<<<<<<<"));
    assert_eq!(read(&r, "g.txt"), "g\n", "clean part of the pick is applied");
    assert!(r.worktree().join(".vcs/pick").exists());
    // cannot commit while unresolved
    assert!(r.commit("x", sig()).is_err());
    write(&r, "f.txt", "1\n2\nresolved\n");
    r.add(&["f.txt".to_string()]).unwrap();
    r.commit("picked with fix", sig()).unwrap();
    let (_, cm) = head(&r);
    assert_eq!(cm.parents, vec![before], "single parent, not a merge commit");
    assert!(clean(&r));
    assert!(!r.worktree().join(".vcs/pick").exists());
    fsck(&r);
}

#[test]
fn cherry_pick_conflict_abort_restores_everything() {
    let (_d, r, c) = with_feature();
    write(&r, "f.txt", "1\n2\nTHREE\n");
    commit(&r, "main edit");
    r.cherry_pick("root", &c.to_string(), sig()).unwrap();
    r.merge_abort().unwrap();
    assert_eq!(read(&r, "f.txt"), "1\n2\nTHREE\n");
    assert!(!exists(&r, "g.txt"));
    assert!(clean(&r));
    assert!(!r.worktree().join(".vcs/pick").exists());
}

#[test]
fn stale_pick_marker_does_not_turn_a_later_merge_into_a_pick() {
    let (_d, r, c) = with_feature();
    write(&r, "f.txt", "1\n2\nTHREE\n");
    let ours = commit(&r, "main edit");
    r.cherry_pick("root", &c.to_string(), sig()).unwrap();
    let marker = fs::read(r.worktree().join(".vcs/pick")).unwrap();
    r.merge_abort().unwrap();
    fs::write(r.worktree().join(".vcs/pick"), marker).unwrap(); // leftover from a crash
    assert!(matches!(
        r.merge("root", "feature", sig()).unwrap(),
        MergeOutcome::Conflicted { .. }
    ));
    write(&r, "f.txt", "1\n2\nboth\n");
    r.add(&["f.txt".to_string()]).unwrap();
    r.commit("merge", sig()).unwrap();
    assert_eq!(head(&r).1.parents, vec![ours, c], "still a two-parent merge commit");
}

#[test]
fn cherry_pick_refuses_dirty_merge_commits_and_unknown_revs() {
    let (_d, r, c) = with_feature();
    assert!(r.cherry_pick("root", "nonsense", sig()).is_err());
    assert!(r.cherry_pick("root", "deadbeef", sig()).is_err());
    write(&r, "f.txt", "dirty\n");
    let e = r.cherry_pick("root", &c.to_string(), sig()).unwrap_err().to_string();
    assert!(e.contains("uncommitted"), "{e}");
    assert_eq!(read(&r, "f.txt"), "dirty\n");
    r.restore(&["f.txt".to_string()], false).unwrap();
    // a real merge commit is refused
    write(&r, "h.txt", "h\n");
    commit(&r, "main side");
    let m = merged(r.merge("root", "feature", sig()).unwrap());
    assert_eq!(head(&r).1.parents.len(), 2);
    for e in [
        r.revert("root", &m.to_string(), sig()),
        r.cherry_pick("root", &m.to_string(), sig()),
    ] {
        assert!(e.unwrap_err().to_string().contains("merge commit"));
    }
}

#[test]
fn cherry_pick_that_is_already_applied_is_an_error() {
    let (_d, r, c) = with_feature();
    merged(r.cherry_pick("root", &c.to_string(), sig()).unwrap());
    let e = r.cherry_pick("root", &c.to_string(), sig()).unwrap_err().to_string();
    assert!(e.contains("changes nothing"), "{e}");
}

#[test]
fn revert_clean_undoes_modification_and_addition() {
    let (_d, r, c) = with_feature();
    r.merge("root", "feature", sig()).unwrap(); // fast-forward onto the feature commit
    assert!(exists(&r, "g.txt"));
    let new = merged(r.revert("root", &c.to_string(), sig()).unwrap());
    let (id, cm) = head(&r);
    assert_eq!(id, new);
    assert_eq!(cm.parents, vec![c]);
    assert!(cm.message.starts_with("Revert \"feature work\""));
    assert!(cm.message.contains(&c.to_string()));
    assert_eq!(read(&r, "f.txt"), "1\n2\n3\n");
    assert!(!exists(&r, "g.txt"));
    assert!(clean(&r));
    fsck(&r);
    r.op_undo().unwrap();
    assert_eq!(read(&r, "f.txt"), "1\n2\nthree\n");
    assert!(exists(&r, "g.txt"));
}

#[test]
fn revert_of_root_commit_empties_the_tree() {
    let (_d, r) = setup();
    write(&r, "a.txt", "a\n");
    let first = commit(&r, "first");
    write(&r, "b.txt", "b\n");
    commit(&r, "second");
    merged(r.revert("root", &first.to_string(), sig()).unwrap());
    assert!(!exists(&r, "a.txt"));
    assert!(exists(&r, "b.txt"));
}

#[test]
fn revert_conflict_then_resolve_and_abort() {
    let (_d, r) = setup();
    write(&r, "f.txt", "1\n2\n3\n");
    commit(&r, "base");
    write(&r, "f.txt", "1\n2\nthree\n");
    let target = commit(&r, "change three");
    write(&r, "f.txt", "1\n2\nTHREE!\n");
    let tip = commit(&r, "change again");
    match r.revert("root", &target.to_string(), sig()).unwrap() {
        MergeOutcome::Conflicted { paths } => assert_eq!(paths, vec!["f.txt".to_string()]),
        o => panic!("{o:?}"),
    }
    assert!(read(&r, "f.txt").contains("<<<<<<<"));
    r.merge_abort().unwrap();
    assert_eq!(read(&r, "f.txt"), "1\n2\nTHREE!\n");
    assert!(clean(&r));
    // again, now resolve
    r.revert("root", &target.to_string(), sig()).unwrap();
    write(&r, "f.txt", "1\n2\n3\n");
    r.add(&["f.txt".to_string()]).unwrap();
    r.commit("Revert change three", sig()).unwrap();
    let (_, cm) = head(&r);
    assert_eq!(cm.parents, vec![tip]);
    assert!(clean(&r));
    fsck(&r);
}

#[test]
fn pick_is_blocked_during_a_merge_in_progress() {
    let (_d, r, c) = with_feature();
    write(&r, "f.txt", "1\n2\nTHREE\n");
    commit(&r, "main edit");
    assert!(matches!(
        r.merge("root", "feature", sig()).unwrap(),
        MergeOutcome::Conflicted { .. }
    ));
    let e = r.cherry_pick("root", &c.to_string(), sig()).unwrap_err().to_string();
    assert!(e.contains("already in progress"), "{e}");
}
