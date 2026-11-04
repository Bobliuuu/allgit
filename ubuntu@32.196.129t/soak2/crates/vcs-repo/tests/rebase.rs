//! Rebase: stable change ids, conflict stop/continue/abort, one undoable operation.
use std::fs;
use vcs_objects::{ChangeId, ObjectId, Signature};
use vcs_repo::{MergeOutcome, RebaseOutcome, Repo};

fn sig(ts: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: ts,
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
fn op_descs(r: &Repo) -> Vec<String> {
    r.op_log(100).unwrap().into_iter().map(|(_, o)| o.description).collect()
}
/// (id, change id, message) of the branch history, newest first.
fn hist(r: &Repo, b: &str) -> Vec<(ObjectId, ChangeId, String)> {
    r.log("root", Some(b), 100)
        .unwrap()
        .into_iter()
        .map(|(i, c)| (i, c.change_id, c.message))
        .collect()
}

const BASE: &str = "a\nb\nc\nd\ne\nf\ng\nh\n";

/// base on main; `feature` gets two commits; main gets one afterwards. Leaves `feature` checked out.
fn setup(feature_edit: &str) -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    write(&r, "f", BASE);
    commit(&r, "base");
    r.create_branch("root", "feature").unwrap();
    r.checkout("root", "feature").unwrap();
    write(&r, "f", feature_edit);
    commit(&r, "feature one");
    write(&r, "g", "new file\n");
    commit(&r, "feature two");
    r.checkout("root", "main").unwrap();
    write(&r, "f", "a\nb\nc\nd\ne\nf\ng\nH\n");
    commit(&r, "main work");
    r.checkout("root", "feature").unwrap();
    (d, r)
}

#[test]
fn clean_rebase_keeps_change_ids_and_undo_restores_head() {
    let (_d, r) = setup("A\nb\nc\nd\ne\nf\ng\nh\n");
    let before = hist(&r, "feature");
    let orig = head(&r, "feature");
    let main = head(&r, "main");
    let ops_before = op_descs(&r).len();

    let out = r.rebase("root", "main", sig(2000)).unwrap();
    let new = head(&r, "feature");
    assert_eq!(
        out,
        RebaseOutcome::Done {
            from: orig,
            to: new,
            replayed: 2
        }
    );
    let after = hist(&r, "feature");
    assert_eq!(after.len(), 4); // base, main work, two replayed
    for i in 0..2 {
        assert_eq!(after[i].1, before[i].1, "change id kept");
        assert_eq!(after[i].2, before[i].2, "message kept");
        assert_ne!(after[i].0, before[i].0, "new commit id");
    }
    assert_eq!(after[2].0, main);
    assert_eq!(read(&r, "f"), "A\nb\nc\nd\ne\nf\ng\nH\n");
    assert_eq!(read(&r, "g"), "new file\n");
    assert_eq!(head(&r, "main"), main);
    assert_eq!(op_descs(&r).len(), ops_before + 1);
    assert_eq!(op_descs(&r)[0], "rebase root/feature onto main");
    fsck(&r);

    r.op_undo().unwrap();
    assert_eq!(head(&r, "feature"), orig);
    assert_eq!(read(&r, "f"), "A\nb\nc\nd\ne\nf\ng\nh\n");
    fsck(&r);
}

#[test]
fn fast_forward_up_to_date_and_self_rebase() {
    let (_d, r) = setup("A\nb\nc\nd\ne\nf\ng\nh\n");
    // a branch created at main's head is up to date; rebasing onto itself is refused
    r.checkout("root", "main").unwrap();
    r.create_branch("root", "same").unwrap();
    r.checkout("root", "same").unwrap();
    let ops = op_descs(&r).len();
    assert_eq!(r.rebase("root", "main", sig(1)).unwrap(), RebaseOutcome::UpToDate);
    assert_eq!(op_descs(&r).len(), ops);
    assert!(r.rebase("root", "same", sig(1)).is_err());

    // a branch strictly behind its target fast-forwards without replaying
    r.checkout("root", "feature").unwrap();
    r.rebase("root", "main", sig(1)).unwrap();
    r.checkout("root", "same").unwrap();
    let target = head(&r, "feature");
    let out = r.rebase("root", "feature", sig(1)).unwrap();
    assert!(matches!(out, RebaseOutcome::Done { replayed: 0, .. }), "{out:?}");
    assert_eq!(head(&r, "same"), target);
    assert_eq!(read(&r, "g"), "new file\n");
    fsck(&r);
}

const FEAT_CONFLICT: &str = "a\nb\nc\nd\ne\nf\ng\nFEAT\n";

#[test]
fn conflict_stops_persists_and_continue_finishes() {
    let (_d, r) = setup(FEAT_CONFLICT);
    let before = hist(&r, "feature");
    let orig = head(&r, "feature");
    let main = head(&r, "main");
    let ops = op_descs(&r).len();

    let out = r.rebase("root", "main", sig(2000)).unwrap();
    assert!(
        matches!(&out, RebaseOutcome::Conflicted { paths, .. } if paths == &["f"]),
        "{out:?}"
    );
    // stopped: branch is at the last good tip (main), conflicts persisted, no op recorded
    assert_eq!(head(&r, "feature"), main);
    assert_eq!(op_descs(&r).len(), ops);
    assert!(read(&r, "f").contains("<<<<<<<"));
    assert_eq!(r.conflicts().unwrap().len(), 1);
    let rs = r.rebase_state().unwrap().unwrap();
    assert_eq!(rs.orig, orig);
    assert_eq!(rs.todo.len(), 1);
    // survives reopening
    let r2 = Repo::discover(r.worktree()).unwrap();
    assert!(r2.rebase_state().unwrap().is_some());

    // guarded commands
    assert!(r.commit("nope", sig(1)).unwrap_err().to_string().contains("rebase --continue"));
    assert!(r.merge("root", "main", sig(1)).is_err());
    assert!(r.merge_abort().is_err());
    assert!(r.rebase("root", "main", sig(1)).is_err());
    assert!(r.op_undo().is_err());
    assert!(r.rebase_continue(sig(1)).unwrap_err().to_string().contains("unresolved"));

    write(&r, "f", "A\nb\nc\nd\ne\nf\ng\nresolved\n");
    r.add(&["f".into()]).unwrap();
    let out = r.rebase_continue(sig(2001)).unwrap();
    let new = head(&r, "feature");
    assert_eq!(
        out,
        RebaseOutcome::Done {
            from: orig,
            to: new,
            replayed: 2
        }
    );
    let after = hist(&r, "feature");
    assert_eq!(after[0].1, before[0].1);
    assert_eq!(after[1].1, before[1].1);
    assert_eq!(after[1].2, "feature one");
    assert_eq!(read(&r, "f"), "A\nb\nc\nd\ne\nf\ng\nresolved\n");
    assert_eq!(read(&r, "g"), "new file\n");
    assert!(r.rebase_state().unwrap().is_none() && r.merge_state().unwrap().is_none());
    assert_eq!(op_descs(&r).len(), ops + 1);
    fsck(&r);

    r.op_undo().unwrap();
    assert_eq!(head(&r, "feature"), orig);
    assert_eq!(read(&r, "f"), FEAT_CONFLICT);
    assert_eq!(read(&r, "g"), "new file\n");
    fsck(&r);
}

#[test]
fn abort_restores_exact_state_without_an_operation() {
    let (_d, r) = setup(FEAT_CONFLICT);
    let orig = head(&r, "feature");
    let ops = op_descs(&r).len();
    r.rebase("root", "main", sig(2000)).unwrap();
    r.rebase_abort().unwrap();
    assert_eq!(head(&r, "feature"), orig);
    assert_eq!(read(&r, "f"), FEAT_CONFLICT);
    assert_eq!(read(&r, "g"), "new file\n");
    assert_eq!(op_descs(&r).len(), ops);
    assert!(r.rebase_state().unwrap().is_none() && r.merge_state().unwrap().is_none());
    let st = r.status().unwrap();
    assert!(st.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty()));
    fsck(&r);
    // and it can be started again
    assert!(matches!(
        r.rebase("root", "main", sig(1)).unwrap(),
        RebaseOutcome::Conflicted { .. }
    ));
    r.rebase_abort().unwrap();
    assert!(r.rebase_abort().is_err());
}

#[test]
fn conflict_in_second_commit_keeps_first_replayed_change() {
    let (_d, r) = setup("A\nb\nc\nd\ne\nf\ng\nh\n");
    // make main touch g so the second feature commit (adds g) conflicts add/add
    r.checkout("root", "main").unwrap();
    write(&r, "g", "main g\n");
    commit(&r, "main g");
    r.checkout("root", "feature").unwrap();
    let before = hist(&r, "feature");
    let out = r.rebase("root", "main", sig(2000)).unwrap();
    assert!(
        matches!(&out, RebaseOutcome::Conflicted { paths, .. } if paths == &["g"]),
        "{out:?}"
    );
    // first pick landed under its original change id
    let h = hist(&r, "feature");
    assert_eq!(h[0].1, before[1].1);
    assert_eq!(h[0].2, "feature one");
    write(&r, "g", "both\n");
    r.add(&["g".into()]).unwrap();
    r.rebase_continue(sig(3)).unwrap();
    let h = hist(&r, "feature");
    assert_eq!((h[0].1, h[1].1), (before[0].1, before[1].1));
    assert_eq!(read(&r, "g"), "both\n");
    fsck(&r);
}

#[test]
fn refuses_merge_commits_and_dirty_trees() {
    let (_d, r) = setup("A\nb\nc\nd\ne\nf\ng\nh\n");
    write(&r, "dirty", "x");
    r.add(&["dirty".into()]).unwrap();
    assert!(r.rebase("root", "main", sig(1)).is_err());
    r.commit("tmp", sig(1)).unwrap();

    // a merge commit inside the range is refused with nothing changed
    r.create_branch("root", "side").unwrap();
    r.checkout("root", "side").unwrap();
    write(&r, "s", "s\n");
    commit(&r, "side");
    r.checkout("root", "feature").unwrap();
    write(&r, "extra", "e\n");
    commit(&r, "feature three");
    assert!(matches!(r.merge("root", "side", sig(5)).unwrap(), MergeOutcome::Merged { .. }));
    let h = head(&r, "feature");
    let err = r.rebase("root", "main", sig(1)).unwrap_err().to_string();
    assert!(err.contains("2 parents"), "{err}");
    assert_eq!(head(&r, "feature"), h);
    fsck(&r);
}
