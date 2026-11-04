use std::fs;
use vcs_objects::Signature;
use vcs_repo::{Repo, StashPopOutcome};

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
fn commit_all(r: &Repo, m: &str) {
    r.add(&["".to_string()]).unwrap();
    r.commit(m, sig()).unwrap();
}

#[test]
fn ignored_files_are_hidden_from_status_and_add() {
    let (_d, r) = setup();
    write(&r, ".vcsignore", "*.o\ntarget/\n!keep.o\n");
    write(&r, "a.c", "x");
    write(&r, "a.o", "bin");
    write(&r, "keep.o", "kept");
    write(&r, "target/out", "bin");
    write(&r, "sub/.vcsignore", "/local\n");
    write(&r, "sub/local", "l");
    write(&r, "sub/deep/local", "l");
    let st = r.status().unwrap();
    let mut u = st.untracked.clone();
    u.sort();
    assert_eq!(u, vec![".vcsignore", "a.c", "keep.o", "sub/"]);
    r.add(&["".to_string()]).unwrap();
    let ix: Vec<String> = r.status().unwrap().zones[0].staged.iter().map(|c| c.path.clone()).collect();
    assert_eq!(ix, vec![".vcsignore", "a.c", "keep.o", "sub/.vcsignore", "sub/deep/local"]);
    // explicit ignored path is skipped, with a report
    let rep = r.add(&["a.o".to_string()]).unwrap();
    assert_eq!(rep.staged, 0);
    assert_eq!(rep.skipped, vec!["a.o (ignored)"]);
    assert!(r.is_ignored("target/out", false).unwrap());
    assert!(!r.is_ignored("a.c", false).unwrap());
}

#[test]
fn tracked_files_are_never_ignored() {
    let (_d, r) = setup();
    write(&r, "gen/x.txt", "1");
    commit_all(&r, "c");
    write(&r, ".vcsignore", "gen/\n*.txt\n");
    write(&r, "gen/new.txt", "n");
    write(&r, "gen/x.txt", "2");
    let st = r.status().unwrap();
    assert_eq!(st.untracked, vec![".vcsignore"]);
    assert_eq!(st.zones[0].unstaged.len(), 1);
    r.add(&["".to_string()]).unwrap();
    let st = r.status().unwrap();
    let paths: Vec<_> = st.zones[0].staged.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, vec![".vcsignore", "gen/x.txt"]);
    assert!(!r.is_ignored("gen/x.txt", false).unwrap());
}

#[test]
fn stash_round_trip() {
    let (_d, r) = setup();
    write(&r, "a.txt", "one\n");
    write(&r, "b.txt", "bee\n");
    commit_all(&r, "base");
    write(&r, "a.txt", "two\n");
    write(&r, "new.txt", "fresh\n");
    r.add(&["new.txt".to_string()]).unwrap();
    fs::remove_file(r.worktree().join("b.txt")).unwrap();
    write(&r, "untracked.txt", "u");
    let zs = r.stash_push(Some("my work"), sig()).unwrap();
    assert_eq!(zs, vec!["root"]);
    assert_eq!(read(&r, "a.txt"), "one\n");
    assert_eq!(read(&r, "b.txt"), "bee\n");
    assert!(!r.worktree().join("new.txt").exists());
    assert!(r.worktree().join("untracked.txt").exists());
    let st = r.status().unwrap();
    assert!(st.zones[0].staged.is_empty() && st.zones[0].unstaged.is_empty());
    assert_eq!(r.stash_list().unwrap()[0].message, "my work");
    assert_eq!(r.stash_pop(0).unwrap(), StashPopOutcome::Applied);
    assert_eq!(read(&r, "a.txt"), "two\n");
    assert_eq!(read(&r, "new.txt"), "fresh\n");
    assert!(!r.worktree().join("b.txt").exists());
    assert!(r.stash_list().unwrap().is_empty());
    assert!(r.stash_push(None, sig()).is_ok());
    assert!(r.fsck().unwrap().problems.is_empty());
}

#[test]
fn stash_applies_onto_moved_head_and_orders_entries() {
    let (_d, r) = setup();
    write(&r, "a.txt", "1\n2\n3\n4\n5\n6\n7\n8\n");
    commit_all(&r, "base");
    write(&r, "a.txt", "1\n2\n3\n4\n5\n6\n7\nEIGHT\n");
    r.stash_push(Some("first"), sig()).unwrap();
    write(&r, "a.txt", "ONE\n2\n3\n4\n5\n6\n7\n8\n");
    commit_all(&r, "head moves");
    write(&r, "a.txt", "ONE\n2\n3\n4\n5\n6\n7\n8\nnine\n");
    r.stash_push(Some("second"), sig()).unwrap();
    let l = r.stash_list().unwrap();
    assert_eq!((l[0].message.as_str(), l[1].message.as_str()), ("second", "first"));
    assert_eq!(r.stash_pop(1).unwrap(), StashPopOutcome::Applied);
    assert_eq!(read(&r, "a.txt"), "ONE\n2\n3\n4\n5\n6\n7\nEIGHT\n");
    let d = r.stash_drop(0).unwrap();
    assert_eq!(d.message, "second");
    assert!(r.stash_pop(0).is_err());
}

#[test]
fn conflicting_pop_keeps_entry_and_writes_markers() {
    let (_d, r) = setup();
    write(&r, "a.txt", "base\n");
    write(&r, "ok.txt", "ok\n");
    commit_all(&r, "base");
    write(&r, "a.txt", "stashed\n");
    write(&r, "ok.txt", "ok2\n");
    r.stash_push(None, sig()).unwrap();
    write(&r, "a.txt", "upstream\n");
    commit_all(&r, "upstream");
    match r.stash_pop(0).unwrap() {
        StashPopOutcome::Conflicted { paths } => assert_eq!(paths, vec!["a.txt"]),
        o => panic!("{o:?}"),
    }
    let a = read(&r, "a.txt");
    assert!(a.contains("<<<<<<<") && a.contains("stashed") && a.contains("upstream"), "{a}");
    assert_eq!(read(&r, "ok.txt"), "ok2\n");
    assert_eq!(r.stash_list().unwrap().len(), 1, "entry kept");
    let st = r.status().unwrap();
    assert_eq!(st.zones[0].unstaged.len(), 1);
    assert_eq!(st.zones[0].unstaged[0].path, "a.txt");
    assert_eq!(st.zones[0].staged.len(), 1);
    assert_eq!(st.zones[0].staged[0].path, "ok.txt");
    // dirty zone refuses a second pop
    assert!(r.stash_pop(0).is_err());
    r.stash_drop(0).unwrap();
    assert!(r.fsck().unwrap().problems.is_empty());
}

#[test]
fn stash_with_nothing_to_save_errors_and_gc_keeps_entries() {
    let (_d, r) = setup();
    write(&r, "a.txt", "1\n");
    commit_all(&r, "base");
    assert!(r.stash_push(None, sig()).is_err());
    write(&r, "a.txt", "2\n");
    r.stash_push(None, sig()).unwrap();
    let rep = r.gc_plan(std::time::Duration::ZERO).unwrap();
    let e = &r.stash_list().unwrap()[0];
    assert!(!rep.would_delete.iter().any(|(id, _)| *id == e.zones[0].work));
}
