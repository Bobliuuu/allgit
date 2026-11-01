//! Interactive rebase: reorder, drop, reword, squash/fixup, edit, exec, conflicts, skip, abort,
//! and one undoable operation.
use std::fs;
use vcs_objects::{ObjectId, Signature};
use vcs_repo::{Act, RebaseOutcome, RebasePlan, Repo, Step, Stop};

fn sig(ts: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: ts,
    }
}
fn write(r: &Repo, p: &str, c: &str) {
    fs::write(r.worktree().join(p), c).unwrap();
}
fn read(r: &Repo, p: &str) -> String {
    fs::read_to_string(r.worktree().join(p)).unwrap()
}
fn commit(r: &Repo, m: &str) -> ObjectId {
    r.add(&["".to_string()]).unwrap();
    r.commit(m, sig(1000)).unwrap().commits[0].2
}
fn head(r: &Repo) -> ObjectId {
    r.refs().unwrap().head("root", "main").unwrap()
}
fn msgs(r: &Repo) -> Vec<String> {
    r.log("root", Some("main"), 100)
        .unwrap()
        .into_iter()
        .map(|(_, c)| c.message)
        .collect()
}
fn fsck(r: &Repo) {
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}
fn noed(m: &str) -> vcs_repo::Result<String> {
    Ok(m.to_owned())
}

/// base, then one commit per name `c1..cN` each adding a file of that name.
fn setup(n: usize) -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    write(&r, "base", "base\n");
    commit(&r, "base");
    for i in 1..=n {
        write(&r, &format!("c{i}"), &format!("content {i}\n"));
        commit(&r, &format!("commit {i}"));
    }
    (d, r)
}
fn plan(r: &Repo, n: usize) -> RebasePlan {
    r.rebase_plan("root", &format!("HEAD~{n}")).unwrap()
}
fn pick(p: &RebasePlan, i: usize, a: Act) -> Step {
    Step::Pick(a, p.commits[i].id)
}

#[test]
fn reorder_drop_and_undo() {
    let (_d, r) = setup(3);
    let orig = head(&r);
    let ops = r.op_log(100).unwrap().len();
    let p = plan(&r, 3);
    assert_eq!(p.commits.len(), 3);
    // reorder: 3, 1; drop 2
    let steps = vec![pick(&p, 2, Act::Pick), pick(&p, 0, Act::Pick)];
    let out = r.rebase_run(&p, steps, sig(2000), &mut noed).unwrap();
    assert!(matches!(out, RebaseOutcome::Done { replayed: 2, .. }), "{out:?}");
    assert_eq!(msgs(&r), ["commit 1", "commit 3", "base"]);
    assert!(r.worktree().join("c1").exists() && r.worktree().join("c3").exists());
    assert!(!r.worktree().join("c2").exists());
    assert_eq!(r.op_log(100).unwrap().len(), ops + 1);
    fsck(&r);
    r.op_undo().unwrap();
    assert_eq!(head(&r), orig);
    assert_eq!(msgs(&r), ["commit 3", "commit 2", "commit 1", "base"]);
    assert!(r.worktree().join("c2").exists());
    fsck(&r);
}

#[test]
fn unchanged_todo_recreates_identical_commits() {
    let (_d, r) = setup(3);
    let orig = head(&r);
    let p = plan(&r, 3);
    let steps = (0..3).map(|i| pick(&p, i, Act::Pick)).collect();
    r.rebase_run(&p, steps, sig(2000), &mut noed).unwrap();
    assert_eq!(head(&r), orig, "an unchanged pick list must give the same commits");
}

#[test]
fn reword_squash_fixup() {
    let (_d, r) = setup(4);
    let p = plan(&r, 4);
    let mut seen = Vec::new();
    let mut ed = |m: &str| -> vcs_repo::Result<String> {
        seen.push(m.to_owned());
        Ok(format!("EDITED[{}]", m.replace('\n', "|")))
    };
    // 1 reword; 2 pick; 3 squash into 2; 4 fixup into 2/3
    let steps = vec![
        pick(&p, 0, Act::Reword),
        pick(&p, 1, Act::Pick),
        pick(&p, 2, Act::Squash),
        pick(&p, 3, Act::Fixup),
    ];
    r.rebase_run(&p, steps, sig(2000), &mut ed).unwrap();
    assert_eq!(seen, ["commit 1", "commit 2\n\ncommit 3"]);
    assert_eq!(msgs(&r), ["EDITED[commit 2||commit 3]", "EDITED[commit 1]", "base"]);
    for f in ["c1", "c2", "c3", "c4"] {
        assert!(r.worktree().join(f).exists(), "{f}");
    }
    fsck(&r);
}

#[test]
fn squash_first_is_refused_and_nothing_changes() {
    let (_d, r) = setup(2);
    let orig = head(&r);
    let p = plan(&r, 2);
    let e = r.rebase_run(&p, vec![pick(&p, 0, Act::Squash)], sig(1), &mut noed).unwrap_err();
    assert!(e.to_string().contains("without a previous commit"), "{e}");
    assert_eq!(head(&r), orig);
}

#[test]
fn edit_stops_then_continue_with_extra_commit() {
    let (_d, r) = setup(3);
    let orig = head(&r);
    let p = plan(&r, 3);
    let steps = vec![pick(&p, 0, Act::Pick), pick(&p, 1, Act::Edit), pick(&p, 2, Act::Pick)];
    let out = r.rebase_run(&p, steps, sig(2000), &mut noed).unwrap();
    assert!(matches!(out, RebaseOutcome::Stopped { kind: Stop::Edit, .. }), "{out:?}");
    assert_eq!(msgs(&r)[0], "commit 2");
    assert!(!r.worktree().join("c3").exists());
    // a second rebase and nothing-to-continue are refused, commits are allowed
    assert!(r.rebase("root", "main", sig(1)).is_err());
    write(&r, "extra", "x\n");
    commit(&r, "extra work");
    let out = r.rebase_continue(sig(2001)).unwrap();
    assert!(matches!(out, RebaseOutcome::Done { .. }), "{out:?}");
    assert_eq!(msgs(&r), ["commit 3", "extra work", "commit 2", "commit 1", "base"]);
    assert!(r.rebase_state().unwrap().is_none());
    fsck(&r);
    // undo restores the state before the rebase finished: the stop's tip plus the user's own commit
    r.op_undo().unwrap();
    assert_eq!(msgs(&r), ["extra work", "commit 2", "commit 1", "base"]);
    let _ = orig;
}

#[test]
fn continue_refuses_dirty_tree_and_abort_restores() {
    let (_d, r) = setup(2);
    let orig = head(&r);
    let p = plan(&r, 2);
    let out = r
        .rebase_run(&p, vec![pick(&p, 1, Act::Pick), pick(&p, 0, Act::Edit)], sig(2000), &mut noed)
        .unwrap();
    assert!(matches!(out, RebaseOutcome::Stopped { .. }));
    write(&r, "c2", "dirty\n");
    assert!(r.rebase_continue(sig(1)).unwrap_err().to_string().contains("uncommitted"));
    r.rebase_abort().unwrap();
    assert_eq!(head(&r), orig);
    assert_eq!(read(&r, "c1"), "content 1\n");
    assert!(r.rebase_state().unwrap().is_none());
    fsck(&r);
}

#[test]
fn exec_runs_and_failure_stops() {
    let (_d, r) = setup(2);
    let p = plan(&r, 2);
    let steps = vec![
        pick(&p, 0, Act::Pick),
        Step::Exec("test -f c1 && echo ran > exec-ran".into()),
        pick(&p, 1, Act::Pick),
        Step::Exec("false".into()),
    ];
    let out = r.rebase_run(&p, steps, sig(2000), &mut noed).unwrap();
    assert!(
        matches!(&out, RebaseOutcome::Stopped { kind: Stop::Exec, detail, .. } if detail.contains("failed")),
        "{out:?}"
    );
    assert_eq!(read(&r, "exec-ran"), "ran\n");
    fs::remove_file(r.worktree().join("exec-ran")).unwrap();
    // an untracked leftover does not block; continuing finishes
    let out = r.rebase_continue(sig(2001)).unwrap();
    assert!(matches!(out, RebaseOutcome::Done { .. }), "{out:?}");
    fsck(&r);
}

#[test]
fn conflicting_reorder_stops_skip_and_continue() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    write(&r, "f", "1\n");
    commit(&r, "base");
    write(&r, "f", "1\n2\n");
    commit(&r, "add 2");
    write(&r, "f", "1\n2\n3\n");
    commit(&r, "add 3");
    write(&r, "g", "g\n");
    commit(&r, "add g");
    let orig = head(&r);
    let p = plan(&r, 3);
    // "add 3" before "add 2": conflicts (3 depends on 2)
    let steps = vec![pick(&p, 1, Act::Pick), pick(&p, 0, Act::Pick), pick(&p, 2, Act::Pick)];
    let out = r.rebase_run(&p, steps.clone(), sig(2000), &mut noed).unwrap();
    assert!(
        matches!(&out, RebaseOutcome::Conflicted { paths, .. } if paths == &["f"]),
        "{out:?}"
    );
    // skip the conflicting pick ("add 3"); "add 2" then applies cleanly, then "add g"
    let out = r.rebase_skip(sig(2001), &mut noed).unwrap();
    assert!(matches!(out, RebaseOutcome::Done { .. }), "{out:?}");
    assert_eq!(msgs(&r), ["add g", "add 2", "base"]);
    assert_eq!(read(&r, "f"), "1\n2\n");
    r.op_undo().unwrap();
    assert_eq!(head(&r), orig);
    assert_eq!(read(&r, "f"), "1\n2\n3\n");

    // resolve instead: continue commits the resolution under the original message
    let out = r.rebase_run(&p, steps, sig(2002), &mut noed).unwrap();
    assert!(matches!(out, RebaseOutcome::Conflicted { .. }));
    write(&r, "f", "1\n3\n");
    r.add(&["f".to_string()]).unwrap();
    let out = r.rebase_continue(sig(2003)).unwrap();
    // "add 2" now conflicts against the resolved file; resolve again
    assert!(matches!(out, RebaseOutcome::Conflicted { .. }), "{out:?}");
    write(&r, "f", "1\n2\n3\n");
    r.add(&["f".to_string()]).unwrap();
    let out = r.rebase_continue(sig(2004)).unwrap();
    assert!(matches!(out, RebaseOutcome::Done { .. }), "{out:?}");
    assert_eq!(msgs(&r), ["add g", "add 2", "add 3", "base"]);
    assert_eq!(read(&r, "f"), "1\n2\n3\n");
    fsck(&r);
    r.op_undo().unwrap();
    assert_eq!(head(&r), orig);
}

#[test]
fn skip_is_only_for_conflicts() {
    let (_d, r) = setup(2);
    let p = plan(&r, 2);
    let steps = vec![pick(&p, 0, Act::Edit), pick(&p, 1, Act::Pick)];
    let out = r.rebase_run(&p, steps, sig(2000), &mut noed).unwrap();
    assert!(matches!(out, RebaseOutcome::Stopped { kind: Stop::Edit, .. }), "{out:?}");
    assert!(r.rebase_skip(sig(1), &mut noed).is_err());
    r.rebase_abort().unwrap();
    assert_eq!(msgs(&r), ["commit 2", "commit 1", "base"]);
}

#[test]
fn nothing_to_do_and_unknown_commit() {
    let (_d, r) = setup(2);
    assert!(r.rebase_plan("root", "HEAD").is_err());
    let p = plan(&r, 2);
    let (_d2, r2) = setup(2);
    let q = plan(&r2, 2);
    assert!(r
        .rebase_run(&p, vec![Step::Pick(Act::Pick, q.commits[0].id)], sig(1), &mut noed)
        .is_err());
}
