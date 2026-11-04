//! Revision ranges (A..B, A...B, ^A, --all), detached HEAD, branch rename and branch-at-rev.
use std::fs;
use vcs_objects::{ObjectId, Signature};
use vcs_repo::{Repo, RevSpec, ROOT_ZONE};

fn sig(ts: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: ts,
    }
}
fn commit(r: &Repo, file: &str, msg: &str, ts: u64) -> ObjectId {
    fs::write(r.worktree().join(file), msg).unwrap();
    r.add(&[file.to_string()]).unwrap();
    r.commit(msg, sig(ts)).unwrap().commits[0].2
}
fn subjects(r: &Repo, spec: &[&str]) -> Vec<String> {
    let s = RevSpec::parse(spec).unwrap();
    r.rev_list(ROOT_ZONE, &s, usize::MAX)
        .unwrap()
        .into_iter()
        .map(|(_, c)| c.message.lines().next().unwrap().to_owned())
        .collect()
}

/// m1 m2 -- m3 -- M -- m4 on main, f1 f2 on feature (from m2, merged into main by M), p1 on topic (from m1).
fn history() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "a", "m1", 1);
    r.create_branch(ROOT_ZONE, "topic").unwrap();
    commit(&r, "b", "m2", 2);
    r.create_branch(ROOT_ZONE, "feature").unwrap();
    r.checkout(ROOT_ZONE, "feature").unwrap();
    commit(&r, "f", "f1", 3);
    commit(&r, "g", "f2", 5);
    r.checkout(ROOT_ZONE, "main").unwrap();
    commit(&r, "c", "m3", 4);
    let m = r.merge(ROOT_ZONE, "feature", sig(6)).unwrap();
    assert!(matches!(m, vcs_repo::MergeOutcome::Merged { .. }));
    commit(&r, "d", "m4", 7);
    r.checkout(ROOT_ZONE, "topic").unwrap();
    commit(&r, "t", "p1", 8);
    r.checkout(ROOT_ZONE, "main").unwrap();
    (d, r)
}

#[test]
fn ranges_match_git_semantics() {
    let (_d, r) = history();
    let merge_msg = subjects(&r, &["main"])[1].clone(); // the merge commit's subject
    assert_eq!(subjects(&r, &["feature..main"]), ["m4", &merge_msg, "m3"]);
    assert_eq!(subjects(&r, &["^feature", "main"]), ["m4", &merge_msg, "m3"]);
    assert_eq!(subjects(&r, &["main", "--not", "feature"]), ["m4", &merge_msg, "m3"]);
    assert_eq!(subjects(&r, &["main..feature"]), Vec::<String>::new());
    assert_eq!(subjects(&r, &["main", "^main"]), Vec::<String>::new());
    // only exclusions: nothing (git: `rev-list ^A` is empty)
    assert_eq!(subjects(&r, &["^feature"]), Vec::<String>::new());
    // A...B: both sides minus the common history
    assert_eq!(subjects(&r, &["topic...main"]), ["p1", "m4", &merge_msg, "f2", "m3", "f1", "m2"]);
    assert_eq!(subjects(&r, &["main...topic"]), subjects(&r, &["topic...main"]));
    // empty side is HEAD (main is checked out)
    assert_eq!(subjects(&r, &["topic.."]), subjects(&r, &["topic..main"]));
    assert_eq!(subjects(&r, &["..topic"]), ["p1"]);
    // commit-id prefixes, ~N and tags work in ranges
    r.tag_create(ROOT_ZONE, "v1", Some("main~3"), None).unwrap();
    assert_eq!(subjects(&r, &["v1..main"]).len(), 5);
    assert_eq!(subjects(&r, &["main~1..main"]), ["m4"]);
    // everything: all branches, deduplicated, log order
    let all = RevSpec {
        all: true,
        ..Default::default()
    };
    let got: Vec<String> = r
        .rev_list(ROOT_ZONE, &all, usize::MAX)
        .unwrap()
        .into_iter()
        .map(|(_, c)| c.message.lines().next().unwrap().to_owned())
        .collect();
    assert_eq!(got, ["p1", "m4", &merge_msg, "f2", "m3", "f1", "m2", "m1"]);
    // limit
    assert_eq!(r.rev_list(ROOT_ZONE, &all, 3).unwrap().len(), 3);
    // unknown revision is an error, not an empty answer
    assert!(r.rev_list(ROOT_ZONE, &RevSpec::parse(&["nope..main"]).unwrap(), 10).is_err());
}

#[test]
fn graph_and_object_walk_agree() {
    let (_d, mut r) = history();
    let specs: Vec<Vec<&str>> = vec![
        vec!["feature..main"],
        vec!["topic...main"],
        vec!["main"],
        vec!["topic", "main", "--not", "feature"],
    ];
    for s in &specs {
        let spec = RevSpec::parse(s).unwrap();
        r.set_commit_graph(true);
        let with = r.rev_list_ids(ROOT_ZONE, &spec, usize::MAX).unwrap();
        r.set_commit_graph(false);
        let without = r.rev_list_ids(ROOT_ZONE, &spec, usize::MAX).unwrap();
        assert_eq!(with, without, "{s:?}");
    }
}

#[test]
fn detached_head_lifecycle() {
    let (_d, r) = history();
    let z = ROOT_ZONE;
    let m1 = r.resolve_rev(z, "main~4").unwrap_or_else(|_| r.resolve_rev(z, "topic~1").unwrap());
    r.tag_create(z, "v0", Some(&m1.to_hex()), None).unwrap();
    assert_eq!(r.detached_head(z).unwrap(), None);

    // by tag
    let d = r.checkout_detached(z, "v0").unwrap();
    assert_eq!(d, m1);
    assert_eq!(r.detached_head(z).unwrap(), Some(m1));
    assert_eq!(r.current_branch(z).unwrap(), None);
    assert!(!r.worktree().join("b").exists(), "files of m2 are gone");
    assert!(r.worktree().join("a").exists());
    // not listed as a branch, not in branch heads
    assert!(r.branches(z).unwrap().iter().all(|(b, _, cur)| !b.starts_with('.') && !cur));
    assert!(r.refs().unwrap().branches_of(z).iter().all(|(b, _)| b != ".detached"));
    // commit on the detached HEAD
    let c = commit(&r, "x", "on-detached", 20);
    assert_eq!(r.detached_head(z).unwrap(), Some(c));
    assert_eq!(subjects(&r, &["HEAD"]), ["on-detached", "m1"]);
    // log --all includes it; main does not
    let all = RevSpec {
        all: true,
        ..Default::default()
    };
    assert!(r.rev_list(z, &all, 100).unwrap().iter().any(|(i, _)| *i == c));
    assert!(!r
        .rev_list(z, &RevSpec::parse(&["main"]).unwrap(), 100)
        .unwrap()
        .iter()
        .any(|(i, _)| *i == c));
    // dirty refuses to move
    fs::write(r.worktree().join("x"), "dirty").unwrap();
    assert!(r.checkout_detached(z, "main").is_err());
    assert!(r.checkout(z, "main").is_err());
    fs::write(r.worktree().join("x"), "on-detached").unwrap();
    // detach again at a sha prefix
    let main_head = r.resolve_rev(z, "main").unwrap();
    r.checkout_detached(z, &main_head.to_hex()[..10]).unwrap();
    assert_eq!(r.detached_head(z).unwrap(), Some(main_head));
    assert!(r.worktree().join("b").exists());
    // rescue path: a branch at the detached commit, then leave
    let before_ops = r.op_log(100).unwrap().len();
    r.checkout_detached(z, "main").unwrap();
    r.create_branch_from(z, "rescue", Some("main~1")).unwrap();
    assert_eq!(r.resolve_rev(z, "rescue").unwrap(), r.resolve_rev(z, "main~1").unwrap());
    assert!(r.create_branch_from(z, "rescue", Some("main")).is_err(), "exists");
    assert!(r.op_log(100).unwrap().len() > before_ops);

    // switching to a branch drops the detached ref; commits only it reached are reported
    let c2 = commit(&r, "y", "lost?", 30);
    assert_eq!(r.commits_only_in(z, c2).unwrap(), vec![c2]);
    r.checkout(z, "main").unwrap();
    assert_eq!(r.detached_head(z).unwrap(), None);
    assert!(r.refs().unwrap().detached(z).is_none());
    assert_eq!(r.current_branch(z).unwrap().as_deref(), Some("main"));
    assert!(!r.worktree().join("y").exists());

    // op undo brings the detached HEAD (and its file) back
    r.op_undo().unwrap();
    assert_eq!(r.detached_head(z).unwrap(), Some(c2));
    assert!(r.worktree().join("y").exists());
    // keep the work: a branch at the detached commit, then leave without loss
    r.create_branch_from(z, "keep", Some("HEAD")).unwrap();
    r.checkout(z, "keep").unwrap();
    assert_eq!(r.current_branch(z).unwrap().as_deref(), Some("keep"));
    assert_eq!(subjects(&r, &["keep"])[0], "lost?");
    assert!(r.fsck().unwrap().problems.is_empty(), "{:?}", r.fsck().unwrap().problems);
}

#[test]
fn branch_rename() {
    let (_d, r) = history();
    let z = ROOT_ZONE;
    let tip = r.resolve_rev(z, "topic").unwrap();
    // not checked out
    assert_eq!(r.branch_rename(z, "topic", "side", false).unwrap(), Some(tip));
    assert!(r.resolve_rev(z, "topic").is_err());
    assert_eq!(r.resolve_rev(z, "side").unwrap(), tip);
    // checked out: the workspace follows, files stay
    let main = r.resolve_rev(z, "main").unwrap();
    assert_eq!(r.branch_rename(z, "main", "trunk", false).unwrap(), Some(main));
    assert_eq!(r.current_branch(z).unwrap().as_deref(), Some("trunk"));
    assert!(r.worktree().join("d").exists());
    // refusals
    assert!(r.branch_rename(z, "trunk", "feature", false).is_err(), "target exists");
    assert!(r.branch_rename(z, "nope", "x", false).is_err());
    assert!(r.branch_rename(z, "feature", ".bad", false).is_err());
    // force replaces another branch but never the checked-out one
    assert!(r.branch_rename(z, "feature", "trunk", true).is_err());
    r.branch_rename(z, "feature", "side", true).unwrap();
    assert!(r.resolve_rev(z, "feature").is_err());
    // restoring to before the first rename brings back both refs and the workspace
    let ops = r.op_log(100).unwrap();
    let before = ops
        .iter()
        .find(|(_, o)| o.description.contains("rename branch root/topic"))
        .unwrap();
    let prev = before.1.parents[0];
    r.op_restore(&prev).unwrap();
    assert!(r.resolve_rev(z, "feature").is_ok());
    assert!(r.resolve_rev(z, "topic").is_ok());
    assert_eq!(r.current_branch(z).unwrap().as_deref(), Some("main"));
    assert!(r.fsck().unwrap().problems.is_empty());
}

#[test]
fn rename_the_unborn_branch() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    assert_eq!(r.branch_rename(ROOT_ZONE, "main", "trunk", false).unwrap(), None);
    assert_eq!(r.current_branch(ROOT_ZONE).unwrap().as_deref(), Some("trunk"));
    commit(&r, "a", "first", 1);
    assert_eq!(r.refs().unwrap().branches_of(ROOT_ZONE).len(), 1);
    assert_eq!(r.refs().unwrap().branches_of(ROOT_ZONE)[0].0, "trunk");
}
