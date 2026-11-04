//! Snapshot checkout, zone rename/delete, branch delete: happy paths and refusals.
use std::fs;
use vcs_objects::{ObjectId, Signature};
use vcs_repo::Repo;

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
fn commit_all(r: &Repo, msg: &str) -> Vec<(String, String, ObjectId)> {
    r.add(&["".into()]).unwrap();
    r.commit(msg, sig()).unwrap().commits
}
fn descs(r: &Repo) -> Vec<String> {
    r.op_log(100).unwrap().into_iter().map(|(_, o)| o.description).collect()
}
fn branch_of(r: &Repo, zone: &str) -> String {
    r.workspace().unwrap().subs[zone].clone()
}
fn fsck_ok(r: &Repo) {
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}
fn err(r: Result<impl std::fmt::Debug, vcs_repo::Error>) -> String {
    r.unwrap_err().to_string()
}

/// Two zones, snapshot "v1", then a second round of commits.
fn two_zones() -> (tempfile::TempDir, Repo) {
    let (d, r) = setup();
    r.create_zone("engine", "engine").unwrap();
    write(&r, "engine/a.rs", "e1");
    write(&r, "top.txt", "t1");
    commit_all(&r, "one");
    r.snapshot_create("v1", "", sig()).unwrap();
    write(&r, "engine/a.rs", "e2");
    write(&r, "top.txt", "t2");
    commit_all(&r, "two");
    (d, r)
}

#[test]
fn snapshot_checkout_restores_every_zone_without_losing_branches() {
    let (_d, r) = two_zones();
    let main_heads: Vec<_> = ["root", "engine"]
        .iter()
        .map(|a| r.refs().unwrap().head(a, "main").unwrap())
        .collect();
    let rep = r.snapshot_checkout("v1").unwrap();
    assert_eq!(rep.switched.len(), 2);
    assert_eq!(read(&r, "engine/a.rs"), "e1");
    assert_eq!(read(&r, "top.txt"), "t1");
    assert_eq!(branch_of(&r, "engine"), "snapshot/v1");
    assert_eq!(branch_of(&r, "root"), "snapshot/v1");
    // main branches are untouched, so no commit lost its name
    let refs = r.refs().unwrap();
    assert_eq!(refs.head("engine", "main"), Some(main_heads[1]));
    assert_eq!(refs.head("root", "main"), Some(main_heads[0]));
    let st = r.status().unwrap();
    assert!(st.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty()));
    assert_eq!(descs(&r)[0], "snapshot checkout v1");
    // repeating is a no-op; going back to main works and undo restores the pre-checkout view
    let again = r.snapshot_checkout("v1").unwrap();
    assert!(again.switched.is_empty() && again.unchanged.len() == 2);
    r.op_undo().unwrap();
    fsck_ok(&r);
}

#[test]
fn snapshot_checkout_undo_returns_to_previous_state() {
    let (_d, r) = two_zones();
    r.snapshot_checkout("v1").unwrap();
    r.op_undo().unwrap();
    assert_eq!(read(&r, "engine/a.rs"), "e2");
    assert_eq!(branch_of(&r, "engine"), "main");
    fsck_ok(&r);
}

#[test]
fn snapshot_checkout_refuses_dirty_zone_and_changes_nothing() {
    let (_d, r) = two_zones();
    write(&r, "engine/a.rs", "dirty"); // unstaged
    let e = err(r.snapshot_checkout("v1"));
    assert!(e.contains("uncommitted changes"), "{e}");
    assert_eq!(read(&r, "engine/a.rs"), "dirty");
    assert_eq!(read(&r, "top.txt"), "t2");
    assert_eq!(branch_of(&r, "root"), "main");
    assert!(r.refs().unwrap().head("root", "snapshot/v1").is_none());
    // staged work is refused too
    r.add(&["engine/a.rs".into()]).unwrap();
    assert!(err(r.snapshot_checkout("v1")).contains("uncommitted changes"));
    assert!(err(r.snapshot_checkout("nope")).contains("no such snapshot"));
}

#[test]
fn snapshot_checkout_leaves_unsubscribed_zones_alone_and_refuses_name_clash() {
    let (_d, r) = two_zones();
    r.zone_remove("engine").unwrap();
    let rep = r.snapshot_checkout("v1").unwrap();
    assert_eq!(rep.skipped, ["engine"]);
    assert_eq!(rep.switched.len(), 1);
    assert!(!r.worktree().join("engine").exists());
    // a pre-existing snapshot/v1 branch at another commit blocks the checkout
    let (_d2, r2) = two_zones();
    r2.create_branch("root", "snapshot/v1").unwrap(); // at main head, not the pinned commit
    let e = err(r2.snapshot_checkout("v1"));
    assert!(e.contains("already exists"), "{e}");
    assert_eq!(read(&r2, "top.txt"), "t2");
}

#[test]
fn zone_rename_empty_zone_keeps_paths_and_is_logged() {
    let (_d, r) = setup();
    write(&r, "top.txt", "t");
    commit_all(&r, "init");
    r.create_zone("engine", "engine").unwrap();
    r.zone_rename("engine", "core").unwrap();
    let zones = r.zones().unwrap();
    assert_eq!(zones.label("engine"), "core");
    assert_eq!(zones.path_of("engine"), Some("engine"));
    assert_eq!(descs(&r)[0], "zone rename engine core");
    // still fully usable under the new name
    write(&r, "engine/a.rs", "e");
    let c = commit_all(&r, "in core");
    assert_eq!(c[0].0, "engine", "commits carry the id");
    assert_eq!(r.log("core", None, 10).unwrap().len(), 1);
    fsck_ok(&r);
}

#[test]
fn zone_rename_with_history_keeps_branches_snapshots_and_log() {
    let (_d, r) = two_zones();
    r.create_branch("engine", "feature").unwrap();
    let before = r.log("engine", None, 10).unwrap();
    assert_eq!(before.len(), 2);
    r.zone_rename("engine", "core").unwrap();
    assert_eq!(r.log("core", None, 10).unwrap(), before, "same commits, same ids");
    assert_eq!(r.log("engine", None, 10).unwrap(), before, "old name still resolves");
    let names: Vec<_> = r.branches("core").unwrap().into_iter().map(|b| b.0).collect();
    assert_eq!(names, ["feature", "main"]);
    // checkout by new name, commit, snapshot checkout and fsck
    r.checkout("core", "feature").unwrap();
    write(&r, "engine/b.rs", "b");
    let c = commit_all(&r, "on feature");
    assert_eq!(c[0].0, "engine");
    r.checkout("core", "main").unwrap();
    let rep = r.snapshot_checkout("v1").unwrap();
    assert!(rep.switched.iter().any(|s| s.0 == "engine"), "{rep:?}");
    assert_eq!(read(&r, "engine/a.rs"), "e1");
    assert_eq!(r.snapshot_get("v1").unwrap().1.zones.len(), 2);
    r.branch_delete("core", "feature", true).unwrap();
    r.zone_add("core", None).unwrap_err(); // already subscribed
    fsck_ok(&r);
}

#[test]
fn zone_rename_back_and_name_reuse_rules() {
    let (_d, r) = two_zones();
    r.create_zone("docs", "docs").unwrap();
    r.zone_rename("engine", "core").unwrap();
    assert!(err(r.zone_rename("docs", "core")).contains("already exists"));
    assert!(err(r.zone_rename("docs", "engine")).contains("already exists"), "ids stay reserved");
    assert!(err(r.create_zone("core", "other")).contains("already exists"));
    assert!(err(r.create_zone("engine", "other")).contains("already exists"));
    assert!(err(r.zone_rename("core", "core")).contains("already exists"));
    r.zone_rename("core", "engine").unwrap();
    assert_eq!(r.zones().unwrap().label("engine"), "engine");
    r.zone_rename("engine", "core").unwrap();
    // deleting a renamed zone removes its name too
    r.zone_delete("core", true).unwrap();
    assert!(r.zones().unwrap().resolve("core").is_none());
    fsck_ok(&r);
}

#[test]
fn zone_rename_refusals() {
    let (_d, r) = two_zones();
    assert!(err(r.zone_rename("root", "x")).contains("root"));
    assert!(err(r.zone_rename("nope", "x")).contains("no such zone"));
    r.create_zone("docs", "docs").unwrap();
    assert!(err(r.zone_rename("docs", "engine")).contains("already exists"));
    assert!(err(r.zone_rename("docs", "bad name")).contains("zone name"));
    write(&r, "engine/a.rs", "dirty");
    assert!(err(r.zone_rename("engine", "core")).contains("engine"), "dirty zone refused");
    assert_eq!(r.zones().unwrap().label("engine"), "engine");
}

#[test]
fn undo_across_rename_works_and_undoing_the_rename_is_refused() {
    let (_d, r) = two_zones();
    r.zone_rename("engine", "core").unwrap();
    assert!(err(r.op_undo()).contains("renamed a zone"));
    write(&r, "engine/a.rs", "e3");
    commit_all(&r, "three");
    assert_eq!(r.log("core", None, 10).unwrap().len(), 3);
    r.op_undo().unwrap(); // undoes the commit made after the rename
    assert_eq!(r.log("core", None, 10).unwrap().len(), 2);
    assert_eq!(r.zones().unwrap().label("engine"), "core");
    // restoring to an operation from before the rename keeps working, and keeps the name
    let before = r
        .op_log(100)
        .unwrap()
        .into_iter()
        .find(|(_, o)| o.description.starts_with("commit"))
        .unwrap()
        .0;
    r.op_restore(&before).unwrap();
    assert_eq!(r.zones().unwrap().label("engine"), "core");
    fsck_ok(&r);
}

#[test]
fn zone_delete_empty_zone_ok_and_dirty_refused() {
    let (_d, r) = setup();
    write(&r, "top.txt", "t");
    commit_all(&r, "init");
    r.create_zone("docs", "docs").unwrap();
    write(&r, "docs/readme", "draft"); // untracked, uncommitted work in the zone
    r.add(&["docs/readme".into()]).unwrap();
    let e = err(r.zone_delete("docs", false));
    assert!(e.contains("uncommitted"), "{e}");
    assert_eq!(read(&r, "docs/readme"), "draft");
    assert!(r.zones().unwrap().exists("docs"));
    // once the work is gone from the index it can be deleted; the file is not touched
    let (_d2, r2) = setup();
    write(&r2, "top.txt", "t");
    commit_all(&r2, "init");
    r2.create_zone("docs", "docs").unwrap();
    write(&r2, "docs/notes", "keep me"); // untracked only
    r2.zone_delete("docs", false).unwrap();
    assert!(!r2.zones().unwrap().exists("docs"));
    assert_eq!(read(&r2, "docs/notes"), "keep me");
    assert_eq!(descs(&r2)[0], "zone delete docs");
    fsck_ok(&r2);
}

#[test]
fn zone_delete_with_history_needs_force_and_keeps_commits() {
    let (_d, r) = two_zones();
    let head = r.refs().unwrap().head("engine", "main").unwrap();
    let e = err(r.zone_delete("engine", false));
    assert!(e.contains("--force"), "{e}");
    assert!(r.zones().unwrap().exists("engine"));
    assert_eq!(read(&r, "engine/a.rs"), "e2");
    assert!(err(r.zone_delete("root", true)).contains("root"));
    r.zone_delete("engine", true).unwrap();
    assert!(!r.zones().unwrap().exists("engine"));
    assert!(!r.worktree().join("engine/a.rs").exists());
    assert!(r.refs().unwrap().branches_of("engine").is_empty());
    assert!(r.store().contains(&head).unwrap(), "commits are never deleted");
    assert_eq!(read(&r, "top.txt"), "t2");
    // snapshot checkout now refuses cleanly because the pinned zone is gone
    assert!(err(r.snapshot_checkout("v1")).contains("no longer exists"));
    fsck_ok(&r);
}

#[test]
fn zone_delete_refuses_dirty_even_with_force() {
    let (_d, r) = two_zones();
    write(&r, "engine/a.rs", "dirty");
    assert!(err(r.zone_delete("engine", true)).contains("uncommitted"));
    assert_eq!(read(&r, "engine/a.rs"), "dirty");
    assert!(r.zones().unwrap().exists("engine"));
}

#[test]
fn branch_delete_happy_path_and_undo() {
    let (_d, r) = two_zones();
    r.create_branch("root", "old").unwrap(); // same commit as main
    let h = r.branch_delete("root", "old", false).unwrap();
    assert!(r.refs().unwrap().head("root", "old").is_none());
    assert_eq!(Some(h), r.refs().unwrap().head("root", "main"));
    assert_eq!(descs(&r)[0], "delete branch root/old");
    r.op_undo().unwrap();
    assert_eq!(r.refs().unwrap().head("root", "old"), Some(h));
    fsck_ok(&r);
}

#[test]
fn branch_delete_refusals() {
    let (_d, r) = two_zones();
    assert!(err(r.branch_delete("root", "main", false)).contains("checked-out"));
    assert!(err(r.branch_delete("root", "main", true)).contains("checked-out"));
    assert!(err(r.branch_delete("root", "ghost", false)).contains("no such branch"));
    assert!(err(r.branch_delete("nope", "main", false)).contains("no such zone"));
    // a branch with unique commits is refused without --force, allowed with it
    r.create_branch("root", "feat").unwrap();
    r.checkout("root", "feat").unwrap();
    write(&r, "top.txt", "feat");
    commit_all(&r, "feat work");
    r.checkout("root", "main").unwrap();
    let e = err(r.branch_delete("root", "feat", false));
    assert!(e.contains("not reachable"), "{e}");
    assert!(r.refs().unwrap().head("root", "feat").is_some());
    r.branch_delete("root", "feat", true).unwrap();
    assert!(r.refs().unwrap().head("root", "feat").is_none());
}

#[test]
fn branch_delete_allows_merged_or_snapshotted_branch() {
    let (_d, r) = two_zones();
    r.create_branch("root", "feat").unwrap();
    r.checkout("root", "feat").unwrap();
    write(&r, "top.txt", "feat");
    commit_all(&r, "feat work");
    r.snapshot_create("s2", "", sig()).unwrap(); // pins feat's head
    r.checkout("root", "main").unwrap();
    r.branch_delete("root", "feat", false).unwrap(); // reachable from snapshot s2
}
