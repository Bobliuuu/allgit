//! Linked worktrees: layout, branch exclusivity, safe removal, gc and fsck roots (docs/worktrees.md).

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};
use vcs_objects::Signature;
use vcs_repo::Repo;

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
fn commit_all(r: &Repo, m: &str) {
    r.add(&["".into()]).unwrap();
    r.commit(m, sig()).unwrap();
}
fn main_repo(dir: &Path) -> Repo {
    let a = Repo::init(&dir.join("main")).unwrap();
    a.create_zone("engine", "engine").unwrap();
    write(&a, "README", "readme");
    write(&a, "engine/core.rs", "core v1");
    commit_all(&a, "init");
    a
}
const DAY: Duration = Duration::from_secs(86400);
fn later() -> SystemTime {
    SystemTime::now() + 30 * DAY
}

#[test]
fn add_list_commit_in_linked_worktree_shares_refs_and_objects() {
    let d = tempfile::tempdir().unwrap();
    let a = main_repo(d.path());
    let wt_path = d.path().join("feature");
    let info = a.worktree_add(&wt_path, None).unwrap();
    assert_eq!(info.subs, vec![("root".to_string(), "feature".to_string())]);
    // layout: a real .vcs directory with only per-worktree state plus a pointer to the main one
    let vcs = wt_path.join(".vcs");
    assert!(vcs.join("commondir").is_file() && vcs.join("index").is_file() && vcs.join("workspace").is_file());
    assert!(!vcs.join("refs").exists() && !vcs.join("objects").exists());
    assert_eq!(fs::read_to_string(wt_path.join("README")).unwrap(), "readme");
    // only the root zone is checked out; engine is sparse here
    assert!(!wt_path.join("engine").exists());

    let b = Repo::discover(&wt_path).unwrap();
    write(&b, "README", "changed in feature");
    commit_all(&b, "feature work");
    // the branch moved in the shared refs: main sees it, and its own checkout is untouched
    assert!(a.refs().unwrap().head("root", "feature").is_some());
    assert_ne!(a.refs().unwrap().head("root", "feature"), a.refs().unwrap().head("root", "main"));
    assert_eq!(fs::read_to_string(a.worktree().join("README")).unwrap(), "readme");
    assert!(a
        .status()
        .unwrap()
        .zones
        .iter()
        .all(|z| z.staged.is_empty() && z.unstaged.is_empty()));
    // per-worktree workspace: subscribing a zone in the linked one leaves main alone
    b.zone_add("engine", None).unwrap_err(); // engine/main is checked out in the main worktree
    let list = a.worktree_list().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list[0].main && list[0].current && list[1].name.as_deref() == Some("feature"));
    assert!(a.fsck().unwrap().problems.is_empty());
    assert!(b.fsck().unwrap().problems.is_empty());
}

#[test]
fn a_branch_is_checked_out_in_at_most_one_worktree() {
    let d = tempfile::tempdir().unwrap();
    let a = main_repo(d.path());
    // main is on root/main: a second worktree cannot take it
    let e = a.worktree_add(&d.path().join("w1"), Some("main")).unwrap_err().to_string();
    assert!(e.contains("already checked out"), "{e}");
    assert!(!d.path().join("w1").exists(), "failed add must not leave a directory behind");
    a.worktree_add(&d.path().join("w1"), Some("topic")).unwrap();
    let e = a.worktree_add(&d.path().join("w2"), Some("topic")).unwrap_err().to_string();
    assert!(e.contains("already checked out"), "{e}");
    // checkout and branch delete respect it too
    a.create_branch("root", "spare").unwrap();
    a.checkout("root", "spare").unwrap();
    let w1 = Repo::discover(&d.path().join("w1")).unwrap();
    let e = w1.checkout("root", "spare").unwrap_err().to_string();
    assert!(e.contains("already checked out"), "{e}");
    let e = a.branch_delete("root", "topic", true).unwrap_err().to_string();
    assert!(e.contains("checked out in the worktree"), "{e}");
    // after switching away a branch is free again
    a.checkout("root", "main").unwrap();
    w1.checkout("root", "spare").unwrap();
    a.checkout("root", "topic").unwrap();
    assert!(a.fsck().unwrap().problems.is_empty());
}

#[test]
fn remove_refuses_dirty_worktrees_and_deletes_clean_ones() {
    let d = tempfile::tempdir().unwrap();
    let a = main_repo(d.path());
    let p = d.path().join("w");
    a.worktree_add(&p, Some("w")).unwrap();
    let w = Repo::discover(&p).unwrap();
    write(&w, "README", "dirty");
    let e = a.worktree_remove(&p, false).unwrap_err().to_string();
    assert!(e.contains("not clean") && e.contains("unstaged"), "{e}");
    w.add(&["README".into()]).unwrap();
    assert!(a.worktree_remove(&p, false).unwrap_err().to_string().contains("staged"));
    w.commit("ok", sig()).unwrap();
    write(&w, "scratch.txt", "untracked");
    assert!(a.worktree_remove(&p, false).unwrap_err().to_string().contains("untracked"));
    fs::remove_file(p.join("scratch.txt")).unwrap();
    // cannot remove from inside, or the main worktree
    assert!(w.worktree_remove(&p, false).is_err());
    assert!(w.worktree_remove(a.worktree(), true).is_err());
    a.worktree_remove(&p, false).unwrap();
    assert!(!p.exists());
    assert_eq!(a.worktree_list().unwrap().len(), 1);
    // the branch and its commit survive; the branch is free again
    assert!(a.refs().unwrap().head("root", "w").is_some());
    a.checkout("root", "w").unwrap();
    assert!(a.fsck().unwrap().problems.is_empty());
    // a stash lives only in its worktree: refuse unless forced
    let q = d.path().join("q");
    a.worktree_add(&q, Some("q")).unwrap();
    let wq = Repo::discover(&q).unwrap();
    write(&wq, "README", "stash me");
    wq.stash_push(None, sig()).unwrap();
    assert!(a.worktree_remove(&q, false).unwrap_err().to_string().contains("stash"));
    a.worktree_remove(&q, true).unwrap();
}

#[test]
fn gc_never_deletes_objects_only_another_worktree_reaches() {
    let d = tempfile::tempdir().unwrap();
    let a = main_repo(d.path());
    let p = d.path().join("w");
    a.worktree_add(&p, Some("w")).unwrap();
    let w = Repo::discover(&p).unwrap();

    // (1) a blob reachable only from the linked worktree's index (staged, never committed)
    write(&w, "staged.txt", "content only the linked index knows");
    w.add(&["staged.txt".into()]).unwrap();
    let staged = vcs_objects::blob_id(b"content only the linked index knows");
    assert!(a.store().contains(&staged).unwrap());
    // (2) objects reachable only from the linked worktree's stash
    write(&w, "README", "stashed edit");
    w.stash_push(None, sig()).unwrap();
    let stash_ids: Vec<_> = w
        .stash_list()
        .unwrap()
        .iter()
        .flat_map(|e| e.zones.iter().map(|z| z.work))
        .collect();
    assert!(!stash_ids.is_empty());

    // Run gc from the MAIN worktree: before this change its roots were main's index and stash only.
    let plan = a.gc_plan_at(later(), 14 * DAY).unwrap();
    let doomed: Vec<_> = plan.would_delete.iter().map(|x| x.0).collect();
    assert!(!doomed.contains(&staged), "staged blob of another worktree is a gc candidate");
    for s in &stash_ids {
        assert!(!doomed.contains(s), "stash commit of another worktree is a gc candidate");
    }
    let run = a.gc_sweep_at(later(), vcs_repo::GcOptions::new(14 * DAY)).unwrap();
    assert!(!run.deleted.iter().any(|x| x.0 == staged));
    assert!(a.store().contains(&staged).unwrap());
    for s in &stash_ids {
        assert!(a.store().contains(s).unwrap());
    }
    assert!(a.fsck().unwrap().problems.is_empty());
    // the linked worktree still works: its index blob is intact, the stash pops
    assert!(w.status().unwrap().zones.iter().all(|z| z.staged.len() <= 1));
    w.stash_pop(0).unwrap();

    // control: once the worktree is force-removed its private objects ARE collectable
    w.stash_push(None, sig()).unwrap();
    let stash_ids: Vec<_> = w
        .stash_list()
        .unwrap()
        .iter()
        .flat_map(|e| e.zones.iter().map(|z| z.work))
        .collect();
    a.worktree_remove(&p, true).unwrap();
    let doomed: Vec<_> = a.gc_plan_at(later(), 14 * DAY).unwrap().would_delete.iter().map(|x| x.0).collect();
    assert!(doomed.contains(&staged));
    assert!(stash_ids.iter().all(|s| doomed.contains(s)));
}

#[test]
fn gc_refuses_while_a_worktree_directory_is_missing() {
    let d = tempfile::tempdir().unwrap();
    let a = main_repo(d.path());
    let p = d.path().join("w");
    a.worktree_add(&p, Some("w")).unwrap();
    fs::remove_dir_all(&p).unwrap(); // removed by hand
    let e = a.gc_plan_at(later(), 14 * DAY).unwrap_err().to_string();
    assert!(e.contains("missing or damaged"), "{e}");
    assert!(a.fsck().unwrap().problems.iter().any(|x| x.contains("missing or damaged")));
    a.worktree_remove(&p, false).unwrap();
    a.gc_plan_at(later(), 14 * DAY).unwrap();
    assert!(a.fsck().unwrap().problems.is_empty());
}

#[test]
fn fsck_checks_every_worktrees_index_and_duplicate_checkouts() {
    let d = tempfile::tempdir().unwrap();
    let a = main_repo(d.path());
    let p = d.path().join("w");
    a.worktree_add(&p, Some("w")).unwrap();
    let w = Repo::discover(&p).unwrap();
    write(&w, "only-here.txt", "private blob");
    w.add(&["only-here.txt".into()]).unwrap();
    assert!(a.fsck().unwrap().problems.is_empty());
    let hex = vcs_objects::blob_id(b"private blob").to_hex();
    let f = a.worktree().join(".vcs/objects").join(&hex[..2]).join(&hex[2..]);
    assert!(f.is_file(), "loose object layout changed: {}", f.display());
    fs::remove_file(f).unwrap();
    let probs = a.fsck().unwrap().problems;
    assert!(
        probs.iter().any(|x| x.contains("only-here.txt") && x.contains("missing blob")),
        "{probs:?}"
    );
}

#[test]
fn legacy_repository_layout_is_unchanged() {
    let d = tempfile::tempdir().unwrap();
    let a = main_repo(d.path());
    let vcs = a.worktree().join(".vcs");
    for f in ["refs", "zones", "index", "workspace", "objects", "format", "op_head"] {
        assert!(vcs.join(f).exists(), "{f}");
    }
    assert!(!vcs.join("commondir").exists() && !vcs.join("worktrees").exists());
    assert_eq!(a.worktree_list().unwrap().len(), 1);
    a.gc_plan_at(later(), 14 * DAY).unwrap();
}

#[test]
fn nested_and_non_empty_paths_are_refused() {
    let d = tempfile::tempdir().unwrap();
    let a = main_repo(d.path());
    assert!(a.worktree_add(&a.worktree().join("inside"), Some("x")).is_err());
    let full = d.path().join("full");
    fs::create_dir_all(&full).unwrap();
    fs::write(full.join("f"), "x").unwrap();
    assert!(a.worktree_add(&full, Some("y")).is_err());
    assert!(fs::read_to_string(full.join("f")).unwrap() == "x");
    // an existing empty directory is fine
    let empty = d.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    a.worktree_add(&empty, Some("z")).unwrap();
}
