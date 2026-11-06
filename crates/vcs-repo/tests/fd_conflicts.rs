//! File/directory conflicts: a path that is a file on one side and a directory on the other is a
//! conflict that merge, cherry-pick, rebase and stash pop can represent and resolve. The directory
//! keeps the path; the file side is parked at `path~ours` / `path~theirs`.
use std::fs;
use vcs_objects::{ObjectId, Signature};
use vcs_repo::{MergeOutcome, RebaseOutcome, Repo, Resolution, StashPopOutcome};

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
fn is_file(r: &Repo, p: &str) -> bool {
    r.worktree().join(p).is_file()
}
fn is_dir(r: &Repo, p: &str) -> bool {
    r.worktree().join(p).is_dir()
}
fn gone(r: &Repo, p: &str) -> bool {
    fs::symlink_metadata(r.worktree().join(p)).is_err()
}
fn commit(r: &Repo, m: &str) -> ObjectId {
    r.add(&["".to_string()]).unwrap();
    r.commit(m, sig()).unwrap().commits[0].2
}
fn fsck(r: &Repo) {
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}
fn tracked(r: &Repo) -> Vec<String> {
    let mut v: Vec<String> = r.tracked_files().unwrap().into_iter().map(|(p, _, _)| p).collect();
    v.sort();
    v
}

/// main adds the FILE `p`; feature adds `p/q.txt`. main is checked out.
fn file_vs_dir() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    write(&r, "base.txt", "base\n");
    commit(&r, "base");
    r.create_branch("root", "feature").unwrap();
    r.checkout("root", "feature").unwrap();
    write(&r, "p/q.txt", "q\n");
    write(&r, "p/r.txt", "r\n");
    commit(&r, "feature adds directory p");
    r.checkout("root", "main").unwrap();
    write(&r, "p", "file p\n");
    commit(&r, "main adds file p");
    (d, r)
}

fn start_merge(r: &Repo) {
    match r.merge("root", "feature", sig()).unwrap() {
        MergeOutcome::Conflicted { paths } => assert_eq!(paths, vec!["p".to_string()]),
        o => panic!("expected Conflicted, got {o:?}"),
    }
}

#[test]
fn merge_stops_with_the_directory_in_place_and_the_file_parked() {
    let (_d, r) = file_vs_dir();
    start_merge(&r);
    assert!(is_dir(&r, "p"));
    assert_eq!(read(&r, "p/q.txt"), "q\n");
    assert_eq!(read(&r, "p~ours"), "file p\n");
    assert!(gone(&r, "p~theirs"), "the directory side has no file to park");
    let list = r.conflicts().unwrap();
    assert_eq!(list.len(), 1);
    assert!(!list[0].resolved);
    let shown = r.conflict_show("p").unwrap();
    assert!(shown.contains("file/directory") && shown.contains("p~ours"), "{shown}");
    // unresolved: commit is refused and the check reports it
    let e = r.commit("early", sig()).unwrap_err().to_string();
    assert!(e.contains("unresolved"), "{e}");
    assert_eq!(r.resolve_check().unwrap().unresolved, vec!["p".to_string()]);
}

#[test]
fn merge_resolve_theirs_keeps_the_directory() {
    let (_d, r) = file_vs_dir();
    start_merge(&r);
    r.resolve("p", &Resolution::Theirs).unwrap();
    assert!(gone(&r, "p~ours"));
    assert!(is_dir(&r, "p"));
    assert!(r.resolve_check().unwrap().is_clean());
    r.commit("merge", sig()).unwrap();
    assert_eq!(tracked(&r), vec!["base.txt", "p/q.txt", "p/r.txt"]);
    assert!(r.status().unwrap().merging.is_none());
    fsck(&r);
}

#[test]
fn merge_resolve_ours_keeps_the_file_and_drops_the_directory() {
    let (_d, r) = file_vs_dir();
    start_merge(&r);
    r.resolve("p", &Resolution::Ours).unwrap();
    assert!(is_file(&r, "p"));
    assert_eq!(read(&r, "p"), "file p\n");
    assert!(gone(&r, "p~ours"));
    r.commit("merge", sig()).unwrap();
    assert_eq!(tracked(&r), vec!["base.txt", "p"]);
    fsck(&r);
}

#[test]
fn merge_hand_arrangement_counts_once_the_parked_file_is_moved() {
    let (_d, r) = file_vs_dir();
    start_merge(&r);
    // keep both: the file under another name
    fs::rename(r.worktree().join("p~ours"), r.worktree().join("p-file.txt")).unwrap();
    assert!(r.resolve("p", &Resolution::Mark).is_err(), "p~ours is still staged");
    r.add(&["p~ours".into(), "p-file.txt".into()]).unwrap();
    assert!(r.resolve_check().unwrap().is_clean());
    r.commit("merge", sig()).unwrap();
    assert_eq!(tracked(&r), vec!["base.txt", "p-file.txt", "p/q.txt", "p/r.txt"]);
    fsck(&r);
}

#[test]
fn merge_abort_restores_the_file() {
    let (_d, r) = file_vs_dir();
    let before = r.refs().unwrap().head("root", "main").unwrap();
    start_merge(&r);
    r.merge_abort().unwrap();
    assert!(is_file(&r, "p"));
    assert_eq!(read(&r, "p"), "file p\n");
    assert!(gone(&r, "p~ours") && gone(&r, "p/q.txt"));
    assert_eq!(r.refs().unwrap().head("root", "main").unwrap(), before);
    assert_eq!(tracked(&r), vec!["base.txt", "p"]);
    let s = r.status().unwrap();
    assert!(s.untracked.is_empty() && s.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty()));
}

#[test]
fn merge_directory_ours_and_file_theirs_parks_theirs() {
    let (_d, r) = file_vs_dir();
    r.checkout("root", "feature").unwrap();
    match r.merge("root", "main", sig()).unwrap() {
        MergeOutcome::Conflicted { paths } => assert_eq!(paths, vec!["p".to_string()]),
        o => panic!("{o:?}"),
    }
    assert!(is_dir(&r, "p"));
    assert_eq!(read(&r, "p~theirs"), "file p\n");
    assert!(gone(&r, "p~ours"));
    r.resolve("p", &Resolution::Theirs).unwrap();
    assert_eq!(read(&r, "p"), "file p\n");
    assert!(gone(&r, "p/q.txt") && gone(&r, "p~theirs"));
    r.commit("merge", sig()).unwrap();
    assert_eq!(tracked(&r), vec!["base.txt", "p"]);
    fsck(&r);
}

#[test]
fn merge_refuses_when_the_parking_name_is_taken() {
    let (_d, r) = file_vs_dir();
    write(&r, "p~ours", "mine\n");
    commit(&r, "occupy the parking name");
    let e = r.merge("root", "feature", sig()).unwrap_err().to_string();
    assert!(e.contains("p~ours") && e.contains("nothing was changed"), "{e}");
    assert!(r.merge_state().unwrap().is_none());
    assert!(is_file(&r, "p"));
}

#[test]
fn cherry_pick_file_directory_conflict_is_resolvable() {
    let (_d, r) = file_vs_dir();
    let pick = r.log("root", Some("feature"), 1).unwrap().remove(0).0;
    match r.cherry_pick_id("root", pick, sig()).unwrap() {
        MergeOutcome::Conflicted { paths } => assert_eq!(paths, vec!["p".to_string()]),
        o => panic!("{o:?}"),
    }
    assert!(is_dir(&r, "p") && is_file(&r, "p~ours"));
    r.resolve("p", &Resolution::Theirs).unwrap();
    r.commit("picked", sig()).unwrap();
    let c = r.log("root", None, 1).unwrap().remove(0).1;
    assert_eq!(c.parents.len(), 1, "a pick makes a single-parent commit");
    assert_eq!(tracked(&r), vec!["base.txt", "p/q.txt", "p/r.txt"]);
    fsck(&r);
}

#[test]
fn rebase_stops_on_file_directory_and_continues_after_resolve() {
    let (_d, r) = file_vs_dir();
    r.checkout("root", "feature").unwrap();
    let out = r.rebase("root", "main", sig()).unwrap();
    match out {
        RebaseOutcome::Conflicted { paths, .. } => assert_eq!(paths, vec!["p".to_string()]),
        o => panic!("{o:?}"),
    }
    assert!(is_dir(&r, "p"));
    assert_eq!(read(&r, "p~ours"), "file p\n");
    assert!(r.rebase_continue(sig()).is_err(), "unresolved");
    r.resolve("p", &Resolution::Theirs).unwrap();
    match r.rebase_continue(sig()).unwrap() {
        RebaseOutcome::Done { .. } => {}
        o => panic!("{o:?}"),
    }
    assert_eq!(tracked(&r), vec!["base.txt", "p/q.txt", "p/r.txt"]);
    assert!(r.merge_state().unwrap().is_none() && r.rebase_state().unwrap().is_none());
    fsck(&r);
}

#[test]
fn rebase_file_directory_abort_restores_the_branch() {
    let (_d, r) = file_vs_dir();
    r.checkout("root", "feature").unwrap();
    let before = r.refs().unwrap().head("root", "feature").unwrap();
    assert!(matches!(r.rebase("root", "main", sig()).unwrap(), RebaseOutcome::Conflicted { .. }));
    r.rebase_abort().unwrap();
    assert_eq!(r.refs().unwrap().head("root", "feature").unwrap(), before);
    assert!(is_dir(&r, "p") && gone(&r, "p~ours"));
    assert_eq!(tracked(&r), vec!["base.txt", "p/q.txt", "p/r.txt"]);
}

#[test]
fn stash_pop_with_file_directory_parks_the_file_and_keeps_the_entry() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    write(&r, "p", "one\n");
    write(&r, "keep.txt", "k\n");
    commit(&r, "p is a file");
    write(&r, "p", "stashed edit\n");
    r.stash_push(Some("edit p"), sig()).unwrap();
    // the head turns p into a directory
    fs::remove_file(r.worktree().join("p")).unwrap();
    write(&r, "p/q.txt", "q\n");
    commit(&r, "p is a directory");
    match r.stash_pop(0).unwrap() {
        StashPopOutcome::Conflicted { paths } => assert_eq!(paths, vec!["p".to_string()]),
        o => panic!("{o:?}"),
    }
    assert!(is_dir(&r, "p"));
    assert_eq!(read(&r, "p/q.txt"), "q\n");
    assert_eq!(read(&r, "p~theirs"), "stashed edit\n");
    assert_eq!(r.stash_list().unwrap().len(), 1, "the entry is kept on conflict");
    fsck(&r);
}

#[test]
fn stash_pop_with_directory_in_the_stash_and_file_at_head() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    write(&r, "p/q.txt", "q\n");
    write(&r, "keep.txt", "k\n");
    commit(&r, "p is a directory");
    write(&r, "p/q.txt", "stashed edit\n");
    r.stash_push(None, sig()).unwrap();
    fs::remove_dir_all(r.worktree().join("p")).unwrap();
    write(&r, "p", "file now\n");
    commit(&r, "p is a file");
    match r.stash_pop(0).unwrap() {
        StashPopOutcome::Conflicted { paths } => assert_eq!(paths, vec!["p".to_string()]),
        o => panic!("{o:?}"),
    }
    assert_eq!(read(&r, "p/q.txt"), "stashed edit\n");
    assert_eq!(read(&r, "p~ours"), "file now\n");
    assert_eq!(r.stash_list().unwrap().len(), 1);
}
