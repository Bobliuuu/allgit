use std::fs;
use vcs_objects::Signature;
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
fn exists(r: &Repo, p: &str) -> bool {
    r.worktree().join(p).exists()
}
fn s(p: &str) -> Vec<String> {
    vec![p.to_string()]
}
fn commit(r: &Repo, m: &str) {
    r.add(&s("")).unwrap();
    r.commit(m, sig()).unwrap();
}
fn base() -> (tempfile::TempDir, Repo) {
    let (d, r) = setup();
    write(&r, "a.txt", "a\n");
    write(&r, "dir/b.txt", "b\n");
    write(&r, "dir/sub/c.txt", "c\n");
    commit(&r, "base");
    (d, r)
}
fn status(r: &Repo) -> vcs_repo::Status {
    r.status().unwrap()
}
fn fsck(r: &Repo) {
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}
fn last_op(r: &Repo) -> String {
    r.op_log(1).unwrap()[0].1.description.clone()
}

// ---------- restore ----------

#[test]
fn restore_discards_worktree_edit_and_deletion() {
    let (_d, r) = base();
    write(&r, "a.txt", "changed\n");
    fs::remove_file(r.worktree().join("dir/b.txt")).unwrap();
    let rep = r.restore(&s(""), false).unwrap();
    assert_eq!(rep.restored, 2);
    assert_eq!(read(&r, "a.txt"), "a\n");
    assert_eq!(read(&r, "dir/b.txt"), "b\n");
    assert!(status(&r).zones[0].unstaged.is_empty());
    assert!(last_op(&r).starts_with("restore"));
    fsck(&r);
}

#[test]
fn restore_worktree_uses_the_index_not_head() {
    let (_d, r) = base();
    write(&r, "a.txt", "staged\n");
    r.add(&s("a.txt")).unwrap();
    write(&r, "a.txt", "scratch\n");
    r.restore(&s("a.txt"), false).unwrap();
    assert_eq!(read(&r, "a.txt"), "staged\n");
    assert_eq!(status(&r).zones[0].staged.len(), 1, "staged change is kept");
}

#[test]
fn restore_never_touches_untracked_files() {
    let (_d, r) = base();
    write(&r, "dir/new.txt", "mine\n");
    write(&r, "dir/b.txt", "x\n");
    r.restore(&s("dir"), false).unwrap();
    assert_eq!(read(&r, "dir/new.txt"), "mine\n");
    assert_eq!(read(&r, "dir/b.txt"), "b\n");
    // an untracked path is not a restore target
    let e = r.restore(&s("dir/new.txt"), false).unwrap_err().to_string();
    assert!(e.contains("did not match any tracked"), "{e}");
    assert_eq!(read(&r, "dir/new.txt"), "mine\n");
}

#[test]
fn restore_refuses_directory_in_the_way_and_changes_nothing() {
    let (_d, r) = base();
    write(&r, "a.txt", "edit\n");
    fs::remove_file(r.worktree().join("dir/b.txt")).unwrap();
    fs::create_dir_all(r.worktree().join("dir/b.txt/inner")).unwrap();
    let e = r.restore(&s(""), false).unwrap_err().to_string();
    assert!(e.contains("in the way"), "{e}");
    assert_eq!(read(&r, "a.txt"), "edit\n", "validation happens before any write");
}

#[cfg(unix)]
#[test]
fn restore_refuses_to_write_through_symlink() {
    let (_d, r) = base();
    let out = tempfile::tempdir().unwrap();
    fs::remove_dir_all(r.worktree().join("dir")).unwrap();
    std::os::unix::fs::symlink(out.path(), r.worktree().join("dir")).unwrap();
    assert!(r.restore(&s("dir/b.txt"), false).is_err());
    assert!(fs::read_dir(out.path()).unwrap().next().is_none());
}

#[test]
fn restore_staged_unstages_but_keeps_worktree() {
    let (_d, r) = base();
    write(&r, "a.txt", "new a\n");
    write(&r, "added.txt", "x\n");
    fs::remove_file(r.worktree().join("dir/b.txt")).unwrap();
    r.add(&s("")).unwrap();
    assert_eq!(status(&r).zones[0].staged.len(), 3);
    r.restore(&s(""), true).unwrap();
    let st = status(&r);
    assert!(st.zones[0].staged.is_empty(), "{:?}", st.zones[0].staged);
    assert_eq!(read(&r, "a.txt"), "new a\n", "worktree untouched");
    assert!(st.untracked.contains(&"added.txt".to_string()));
    assert_eq!(st.zones[0].unstaged.len(), 2); // a.txt modified, dir/b.txt deleted
    assert!(last_op(&r).starts_with("restore --staged"));
}

#[test]
fn restore_staged_in_repo_without_commits() {
    let (_d, r) = setup();
    write(&r, "x.txt", "x\n");
    r.add(&s("x.txt")).unwrap();
    r.restore(&s("x.txt"), true).unwrap();
    assert!(status(&r).untracked.contains(&"x.txt".to_string()));
    assert_eq!(read(&r, "x.txt"), "x\n");
}

#[test]
fn restore_unknown_path_and_unsubscribed_zone() {
    let (_d, r) = base();
    assert!(r.restore(&s("nope"), false).is_err());
    assert!(r.restore(&[], false).is_err());
}

// ---------- rm ----------

#[test]
fn rm_removes_clean_file_and_prunes_dirs() {
    let (_d, r) = base();
    let rep = r.rm(&s("dir/sub"), false, false).unwrap();
    assert_eq!(rep.removed, vec!["dir/sub/c.txt".to_string()]);
    assert!(!exists(&r, "dir/sub"));
    assert!(exists(&r, "dir/b.txt"));
    assert_eq!(status(&r).zones[0].staged.len(), 1);
    r.commit("rm", sig()).unwrap();
    fsck(&r);
    assert!(last_op(&r).starts_with("commit"));
}

#[test]
fn rm_refuses_unstaged_and_staged_work_without_force() {
    let (_d, r) = base();
    write(&r, "a.txt", "edited\n");
    let e = r.rm(&s("a.txt"), false, false).unwrap_err().to_string();
    assert!(e.contains("local modifications"), "{e}");
    assert_eq!(read(&r, "a.txt"), "edited\n");
    r.add(&s("a.txt")).unwrap();
    let e = r.rm(&s("a.txt"), false, false).unwrap_err().to_string();
    assert!(e.contains("staged changes"), "{e}");
    assert!(exists(&r, "a.txt"));
    // a refused multi-path rm removes nothing, not even the clean file
    let e = r.rm(&s(""), false, false);
    assert!(e.is_err());
    assert!(exists(&r, "dir/b.txt"));
    r.rm(&s("a.txt"), false, true).unwrap();
    assert!(!exists(&r, "a.txt"));
}

#[test]
fn rm_cached_keeps_the_file() {
    let (_d, r) = base();
    r.rm(&s("a.txt"), true, false).unwrap();
    assert_eq!(read(&r, "a.txt"), "a\n");
    let st = status(&r);
    assert!(st.untracked.contains(&"a.txt".to_string()));
    assert_eq!(st.zones[0].staged.len(), 1);
}

#[test]
fn rm_untracked_is_an_error() {
    let (_d, r) = base();
    write(&r, "u.txt", "u\n");
    assert!(r.rm(&s("u.txt"), false, true).is_err());
    assert!(exists(&r, "u.txt"));
}

#[test]
fn rm_in_unsubscribed_zone_is_refused() {
    let (_d, r) = base();
    r.create_zone("libs", "libs").unwrap();
    write(&r, "libs/l.txt", "l\n");
    commit(&r, "libs");
    r.zone_remove("libs").unwrap();
    let e = r.rm(&s("libs/l.txt"), false, true).unwrap_err().to_string();
    assert!(e.contains("not subscribed"), "{e}");
}

// ---------- mv ----------

#[test]
fn mv_renames_file_and_updates_index() {
    let (_d, r) = base();
    let rep = r.mv("a.txt", "z.txt", false).unwrap();
    assert_eq!(rep.moved, vec![("a.txt".to_string(), "z.txt".to_string())]);
    assert!(!exists(&r, "a.txt"));
    assert_eq!(read(&r, "z.txt"), "a\n");
    let st = status(&r);
    assert_eq!(st.zones[0].staged.len(), 2); // a.txt deleted + z.txt added
    assert!(st.zones[0].unstaged.is_empty() && st.untracked.is_empty());
    r.commit("mv", sig()).unwrap();
    fsck(&r);
    assert!(last_op(&r).starts_with("commit"));
}

#[test]
fn mv_into_existing_directory_and_whole_directory() {
    let (_d, r) = base();
    r.mv("a.txt", "dir", false).unwrap();
    assert_eq!(read(&r, "dir/a.txt"), "a\n");
    r.mv("dir/sub", "moved", false).unwrap();
    assert_eq!(read(&r, "moved/c.txt"), "c\n");
    assert!(!exists(&r, "dir/sub"));
    assert!(status(&r).zones[0].unstaged.is_empty());
    assert!(last_op(&r).starts_with("mv"));
}

#[test]
fn mv_carries_unstaged_edits_and_refuses_overwrite() {
    let (_d, r) = base();
    write(&r, "a.txt", "edited\n");
    r.mv("a.txt", "n.txt", false).unwrap();
    assert_eq!(read(&r, "n.txt"), "edited\n", "edit moves with the file");
    let e = r.mv("n.txt", "dir/b.txt", false).unwrap_err().to_string();
    assert!(e.contains("already exists"), "{e}");
    write(&r, "untracked.txt", "u\n");
    assert!(r.mv("n.txt", "untracked.txt", false).is_err());
    assert_eq!(read(&r, "untracked.txt"), "u\n");
    r.mv("n.txt", "untracked.txt", true).unwrap();
    assert_eq!(read(&r, "untracked.txt"), "edited\n");
}

#[test]
fn mv_errors_leave_everything_unchanged() {
    let (_d, r) = base();
    assert!(r.mv("missing", "x", false).is_err());
    assert!(r.mv("dir", "dir/sub/inner", false).is_err());
    write(&r, "u.txt", "u\n");
    assert!(r.mv("u.txt", "w.txt", false).is_err(), "untracked source");
    assert!(exists(&r, "u.txt") && exists(&r, "dir/b.txt"));
    fs::remove_file(r.worktree().join("a.txt")).unwrap();
    assert!(r.mv("a.txt", "q.txt", false).is_err(), "source missing from worktree");
}

#[test]
fn mv_across_zones_is_refused() {
    let (_d, r) = base();
    r.create_zone("libs", "libs").unwrap();
    write(&r, "libs/l.txt", "l\n");
    commit(&r, "libs");
    let e = r.mv("a.txt", "libs/a.txt", false).unwrap_err().to_string();
    assert!(e.contains("different zones"), "{e}");
    assert!(exists(&r, "a.txt"));
    r.mv("libs/l.txt", "libs/m.txt", false).unwrap();
    assert!(exists(&r, "libs/m.txt"));
}

#[test]
fn op_log_records_each_command() {
    let (_d, r) = base();
    r.restore(&s("a.txt"), false).unwrap();
    r.mv("a.txt", "b.txt", false).unwrap();
    r.rm(&s("b.txt"), true, false).unwrap();
    let descs: Vec<String> = r.op_log(3).unwrap().into_iter().map(|(_, o)| o.description).collect();
    assert!(
        descs[0].starts_with("rm") && descs[1].starts_with("mv") && descs[2].starts_with("restore"),
        "{descs:?}"
    );
    // undo stays safe: it must not lose the working tree
    r.op_undo().unwrap();
    assert_eq!(read(&r, "b.txt"), "a\n");
}
