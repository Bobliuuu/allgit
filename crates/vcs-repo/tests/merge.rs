use proptest::prelude::*;
use std::fs;
use vcs_objects::Signature;
use vcs_repo::{Error, MergeOutcome, Repo, Status};

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
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
}
fn writeb(r: &Repo, p: &str, c: &[u8]) {
    fs::write(r.worktree().join(p), c).unwrap();
}
fn read(r: &Repo, p: &str) -> String {
    fs::read_to_string(r.worktree().join(p)).unwrap()
}
fn exists(r: &Repo, p: &str) -> bool {
    r.worktree().join(p).exists()
}
fn commit(r: &Repo, m: &str) {
    r.add(&["".to_string()]).unwrap();
    r.commit(m, sig(1000)).unwrap();
}
fn fsck(r: &Repo) {
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}
fn clean(r: &Repo) -> bool {
    let s: Status = r.status().unwrap();
    s.untracked.is_empty() && s.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty()) && s.merging.is_none()
}
fn merge(r: &Repo, b: &str) -> vcs_repo::Result<MergeOutcome> {
    r.merge("root", b, sig(2000))
}
fn state_file_exists(r: &Repo) -> bool {
    r.worktree().join(".vcs/conflicts").exists()
}

/// main and `feature` both start at a base commit with the given files. Leaves `main` checked out.
fn diverge(files: &[(&str, &str)], on_main: impl Fn(&Repo), on_feature: impl Fn(&Repo)) -> (tempfile::TempDir, Repo) {
    let (d, r) = setup();
    for (p, c) in files {
        write(&r, p, c);
    }
    commit(&r, "base");
    r.create_branch("root", "feature").unwrap();
    r.checkout("root", "feature").unwrap();
    on_feature(&r);
    commit(&r, "feature work");
    r.checkout("root", "main").unwrap();
    on_main(&r);
    commit(&r, "main work");
    (d, r)
}

const BASE: &str = "a\nb\nc\nd\ne\nf\ng\nh\n";
const MAIN_EDIT: &str = "a\nMAIN\nc\nd\ne\nf\ng\nh\n";
const FEAT_EDIT: &str = "a\nFEAT\nc\nd\ne\nf\ng\nh\n";

#[test]
fn fast_forward() {
    let (_d, r) = setup();
    write(&r, "f", "1\n");
    commit(&r, "base");
    r.create_branch("root", "feature").unwrap();
    r.checkout("root", "feature").unwrap();
    write(&r, "f", "2\n");
    write(&r, "new", "n\n");
    commit(&r, "feat");
    r.checkout("root", "main").unwrap();
    let out = merge(&r, "feature").unwrap();
    assert!(matches!(out, MergeOutcome::FastForward { .. }));
    assert_eq!(read(&r, "f"), "2\n");
    assert!(clean(&r));
    assert_eq!(merge(&r, "feature").unwrap(), MergeOutcome::UpToDate);
    fsck(&r);
}

#[test]
fn clean_merge_makes_two_parent_commit() {
    let (_d, r) = diverge(
        &[("f", BASE), ("g", "g\n")],
        |r| write(r, "f", "A\nb\nc\nd\ne\nf\ng\nh\n"),
        |r| {
            write(r, "f", "a\nb\nc\nd\ne\nf\ng\nH\n");
            write(r, "new", "n\n");
        },
    );
    let main_head = r.refs().unwrap().head("root", "main").unwrap();
    let feat_head = r.refs().unwrap().head("root", "feature").unwrap();
    let MergeOutcome::Merged { commit } = merge(&r, "feature").unwrap() else {
        panic!()
    };
    assert_eq!(read(&r, "f"), "A\nb\nc\nd\ne\nf\ng\nH\n");
    assert_eq!(read(&r, "new"), "n\n");
    assert!(clean(&r));
    assert_eq!(r.refs().unwrap().head("root", "main"), Some(commit));
    let log = r.log("root", None, 10).unwrap();
    let (_, c) = log.iter().find(|(id, _)| *id == commit).unwrap();
    assert_eq!(c.parents, vec![main_head, feat_head]);
    assert_eq!(c.zone, "root");
    for (id, o) in &log {
        if *id != commit {
            assert_ne!(o.change_id, c.change_id);
        }
    }
    fsck(&r);
}

#[test]
fn text_conflict_resolve_and_commit() {
    let (_d, r) = diverge(
        &[("f", BASE)],
        |r| write(r, "f", MAIN_EDIT),
        |r| {
            write(r, "f", FEAT_EDIT);
            write(r, "extra", "x\n");
        },
    );
    let main_head = r.refs().unwrap().head("root", "main").unwrap();
    let feat_head = r.refs().unwrap().head("root", "feature").unwrap();
    let MergeOutcome::Conflicted { paths } = merge(&r, "feature").unwrap() else {
        panic!()
    };
    assert_eq!(paths, vec!["f"]);
    let txt = read(&r, "f");
    assert!(
        txt.contains("<<<<<<<") && txt.contains("MAIN") && txt.contains("FEAT") && txt.contains(">>>>>>>"),
        "{txt}"
    );
    assert_eq!(read(&r, "extra"), "x\n"); // clean file was merged in
    assert!(r.status().unwrap().merging.is_some());
    assert!(state_file_exists(&r));
    let e = r.commit("m", sig(3)).unwrap_err().to_string();
    assert!(e.contains("unresolved") && e.contains('f'), "{e}");
    // adding the file with its markers untouched does not resolve it
    r.add(&["f".into()]).unwrap();
    assert!(r.commit("m", sig(3)).is_err());
    assert!(!r.conflicts().unwrap()[0].resolved);
    assert!(merge(&r, "feature").unwrap_err().to_string().contains("in progress"));
    assert!(r.checkout("root", "feature").is_err());
    let shown = r.conflict_show("f").unwrap();
    assert!(shown.contains("unresolved") && shown.contains("<<<<<<<"), "{shown}");
    write(&r, "f", "a\nBOTH\nc\nd\ne\nf\ng\nh\n");
    r.add(&["f".into()]).unwrap();
    assert!(r.conflicts().unwrap()[0].resolved);
    let rep = r.commit("merge feature", sig(3)).unwrap();
    let (_, _, cid) = &rep.commits[0];
    let c = r.log("root", None, 1).unwrap().remove(0).1;
    assert_eq!(c.parents, vec![main_head, feat_head]);
    assert_eq!(r.refs().unwrap().head("root", "main"), Some(*cid));
    assert!(!state_file_exists(&r));
    assert!(clean(&r));
    assert_eq!(read(&r, "f"), "a\nBOTH\nc\nd\ne\nf\ng\nh\n");
    fsck(&r);
}

#[test]
fn delete_modify_resolved_by_keeping() {
    let (_d, r) = diverge(
        &[("f", "1\n"), ("keep", "k\n")],
        |r| fs::remove_file(r.worktree().join("f")).unwrap(),
        |r| write(r, "f", "2\n"),
    );
    let MergeOutcome::Conflicted { paths } = merge(&r, "feature").unwrap() else {
        panic!()
    };
    assert_eq!(paths, vec!["f"]);
    assert_eq!(read(&r, "f"), "2\n"); // the modified side is preserved
    assert!(r.commit("m", sig(3)).is_err());
    r.add(&["f".into()]).unwrap();
    r.commit("keep modified", sig(3)).unwrap();
    assert_eq!(read(&r, "f"), "2\n");
    assert!(clean(&r));
    fsck(&r);
}

#[test]
fn delete_modify_resolved_by_deleting() {
    let (_d, r) = diverge(
        &[("f", "1\n"), ("keep", "k\n")],
        |r| write(r, "f", "main\n"),
        |r| fs::remove_file(r.worktree().join("f")).unwrap(),
    );
    assert!(matches!(merge(&r, "feature").unwrap(), MergeOutcome::Conflicted { .. }));
    assert_eq!(read(&r, "f"), "main\n");
    fs::remove_file(r.worktree().join("f")).unwrap();
    r.add(&["f".into()]).unwrap();
    r.commit("delete", sig(3)).unwrap();
    assert!(!exists(&r, "f"));
    assert!(clean(&r));
    fsck(&r);
}

#[test]
fn binary_conflict() {
    let (_d, r) = diverge(
        &[("b.bin", "x")],
        |r| writeb(r, "b.bin", &[0, 1, 2]),
        |r| writeb(r, "b.bin", &[0, 9, 9]),
    );
    assert!(matches!(merge(&r, "feature").unwrap(), MergeOutcome::Conflicted { .. }));
    assert_eq!(fs::read(r.worktree().join("b.bin")).unwrap(), vec![0, 1, 2]); // ours kept, no markers
    let shown = r.conflict_show("b.bin").unwrap();
    assert!(shown.contains("binary") && shown.contains("modify/modify"), "{shown}");
    assert!(r.commit("m", sig(3)).is_err());
    writeb(&r, "b.bin", &[0, 9, 9]);
    r.add(&["b.bin".into()]).unwrap();
    r.commit("take theirs", sig(3)).unwrap();
    assert!(clean(&r));
    fsck(&r);
}

#[test]
fn add_add_conflict() {
    let (_d, r) = diverge(&[("base", "b\n")], |r| write(r, "n", "main\n"), |r| write(r, "n", "feat\n"));
    let MergeOutcome::Conflicted { paths } = merge(&r, "feature").unwrap() else {
        panic!()
    };
    assert_eq!(paths, vec!["n"]);
    let t = read(&r, "n");
    assert!(t.contains("<<<<<<<") && t.contains("main") && t.contains("feat"), "{t}");
    assert_eq!(r.conflicts().unwrap()[0].kind, vcs_merge::ConflictKind::AddAdd);
    write(&r, "n", "both\n");
    r.add(&["n".into()]).unwrap();
    r.commit("m", sig(3)).unwrap();
    assert!(clean(&r));
    fsck(&r);
}

#[test]
fn dirty_zone_refuses_merge() {
    let (_d, r) = diverge(&[("f", BASE)], |r| write(r, "m", "m\n"), |r| write(r, "x", "x\n"));
    write(&r, "f", "dirty\n");
    let e = merge(&r, "feature").unwrap_err().to_string();
    assert!(e.contains("uncommitted"), "{e}");
    assert!(r.merge_state().unwrap().is_none());
    r.add(&["f".into()]).unwrap();
    assert!(merge(&r, "feature").is_err()); // staged counts too
    assert_eq!(read(&r, "f"), "dirty\n");
    assert!(!state_file_exists(&r));
}

#[test]
fn abort_restores_exact_state() {
    let (_d, r) = diverge(
        &[("f", BASE), ("dir/old", "o\n")],
        |r| write(r, "f", MAIN_EDIT),
        |r| {
            write(r, "f", FEAT_EDIT);
            write(r, "newdir/x", "x\n");
            fs::remove_file(r.worktree().join("dir/old")).unwrap();
        },
    );
    let head = r.refs().unwrap().head("root", "main");
    let before = read(&r, "f");
    assert!(matches!(merge(&r, "feature").unwrap(), MergeOutcome::Conflicted { .. }));
    assert!(exists(&r, "newdir/x") && !exists(&r, "dir/old"));
    write(&r, "f", "half resolved\n");
    r.merge_abort().unwrap();
    assert_eq!(read(&r, "f"), before);
    assert!(!exists(&r, "newdir"));
    assert_eq!(read(&r, "dir/old"), "o\n");
    assert!(clean(&r));
    assert!(!state_file_exists(&r));
    assert_eq!(r.refs().unwrap().head("root", "main"), head);
    assert!(r.merge_abort().is_err());
    // and the merge can be retried afterwards
    assert!(matches!(merge(&r, "feature").unwrap(), MergeOutcome::Conflicted { .. }));
    fsck(&r);
}

#[test]
fn abort_refuses_when_unrelated_files_changed() {
    let (_d, r) = diverge(
        &[("f", BASE), ("other", "o\n")],
        |r| write(r, "f", MAIN_EDIT),
        |r| write(r, "f", FEAT_EDIT),
    );
    merge(&r, "feature").unwrap();
    write(&r, "other", "edited\n");
    let e = r.merge_abort().unwrap_err().to_string();
    assert!(e.contains("other"), "{e}");
    assert_eq!(read(&r, "other"), "edited\n");
    assert!(r.merge_state().unwrap().is_some());
}

#[test]
fn conflicts_file_means_merging_across_reopen() {
    let (_d, r) = diverge(&[("f", BASE)], |r| write(r, "f", MAIN_EDIT), |r| write(r, "f", FEAT_EDIT));
    merge(&r, "feature").unwrap();
    let r2 = Repo::discover(r.worktree()).unwrap(); // "new process"
    let m = r2.status().unwrap().merging.expect("merging");
    assert_eq!(m.unresolved().len(), 1);
    assert!(r2.commit("m", sig(3)).is_err());
    // a corrupt state file is an error, not silently ignored
    fs::write(r.worktree().join(".vcs/conflicts"), b"junk").unwrap();
    assert!(matches!(r2.status(), Err(Error::Corrupt(_))));
}

#[test]
fn stale_state_file_is_ignored_after_head_moved() {
    let (_d, r) = diverge(&[("f", BASE)], |r| write(r, "f", MAIN_EDIT), |r| write(r, "f", FEAT_EDIT));
    merge(&r, "feature").unwrap();
    let saved = fs::read(r.worktree().join(".vcs/conflicts")).unwrap();
    write(&r, "f", "resolved\n");
    r.add(&["f".into()]).unwrap();
    r.commit("m", sig(3)).unwrap();
    fs::write(r.worktree().join(".vcs/conflicts"), saved).unwrap(); // crash before cleanup
    assert!(r.merge_state().unwrap().is_none());
    write(&r, "f", "more\n");
    r.add(&["f".into()]).unwrap();
    r.commit("later", sig(4)).unwrap();
}

#[test]
fn criss_cross_is_an_error() {
    let (_d, r) = setup();
    write(&r, "f", "0\n");
    commit(&r, "root");
    r.create_branch("root", "other").unwrap();
    write(&r, "x", "1\n");
    commit(&r, "x");
    r.checkout("root", "other").unwrap();
    write(&r, "y", "1\n");
    commit(&r, "y");
    r.create_branch("root", "other2").unwrap(); // at y
    assert!(matches!(merge(&r, "main").unwrap(), MergeOutcome::Merged { .. })); // other = merge(y, x)
    r.checkout("root", "main").unwrap();
    assert!(matches!(merge(&r, "other2").unwrap(), MergeOutcome::Merged { .. })); // main = merge(x, y)
    let e = merge(&r, "other").unwrap_err().to_string();
    assert!(e.contains("criss-cross"), "{e}");
    assert!(clean(&r));
    fsck(&r);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]
    #[test]
    fn merged_tree_has_both_sides_changes(
        ops in proptest::collection::vec(0u8..3, 6),
        left_add in any::<bool>(),
        right_add in any::<bool>(),
    ) {
        let files: Vec<(String, String)> = (0..6).map(|i| (format!("d/f{i}"), format!("base {i}\n"))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let ops2 = ops.clone();
        let (_d, r) = diverge(
            &refs,
            |r| {
                for (i, o) in ops.iter().enumerate() {
                    if *o == 1 { write(r, &format!("d/f{i}"), &format!("main {i}\n")); }
                }
                write(r, "m_touch", "m\n"); // main always diverges
                if left_add { write(r, "main_new", "m\n"); }
            },
            |r| {
                for (i, o) in ops2.iter().enumerate() {
                    if *o == 2 { write(r, &format!("d/f{i}"), &format!("feat {i}\n")); }
                }
                write(r, "f_touch", "f\n");
                if right_add { write(r, "feat_new", "f\n"); }
            },
        );
        let out = merge(&r, "feature").unwrap();
        let conflicted = matches!(out, MergeOutcome::Conflicted { .. });
        prop_assert!(!conflicted);
        for (i, o) in ops.iter().enumerate() {
            let want = match o { 1 => format!("main {i}\n"), 2 => format!("feat {i}\n"), _ => format!("base {i}\n") };
            prop_assert_eq!(read(&r, &format!("d/f{i}")), want);
        }
        prop_assert_eq!(exists(&r, "main_new"), left_add);
        prop_assert_eq!(exists(&r, "feat_new"), right_add);
        prop_assert!(clean(&r)); // index == committed merge tree == working tree
        prop_assert!(r.fsck().unwrap().problems.is_empty());
    }
}
