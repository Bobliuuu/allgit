//! `sync` fast-forwards by diffing the two trees (cost follows the change) and keeps the stat
//! cache usable: the files it had to read to prove the tree clean are not read again afterwards.
use std::fs;
use std::time::Duration;
use vcs_objects::Signature;
use vcs_remote::MemBackend;
use vcs_repo::{Repo, SyncAction};

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

#[test]
fn sync_fast_forward_applies_only_the_change_and_settles_the_stat_cache() {
    let remote = MemBackend::new();
    let d = tempfile::tempdir().unwrap();
    // `b` writes its files and commits: its index entries are all inside the racy window.
    let mut b = Repo::init(&d.path().join("b")).unwrap();
    b.set_index_racy_ns(100_000_000);
    for i in 0..40 {
        write(&b, &format!("d{}/f{i}", i % 5), &format!("content {i}"));
    }
    commit_all(&b, "init");
    b.publish(&remote, None, sig()).unwrap();
    let a = Repo::clone_from(&remote, "mem", &d.path().join("a"), None).unwrap();
    // the window is over in real time, but the index has not recorded that yet
    std::thread::sleep(Duration::from_millis(150));

    // a edits one file, adds one, removes one
    write(&a, "d1/f1", "changed");
    write(&a, "d9/new", "new file");
    fs::remove_file(a.worktree().join("d2/f2")).unwrap();
    a.add(&["".into()]).unwrap();
    a.commit("edit", sig()).unwrap();
    a.publish(&remote, None, sig()).unwrap();

    let rep = b.sync(&remote).unwrap();
    assert!(matches!(rep.actions[0].2, SyncAction::FastForwarded { .. }), "{:?}", rep.actions);
    assert_eq!(fs::read_to_string(b.worktree().join("d1/f1")).unwrap(), "changed");
    assert_eq!(fs::read_to_string(b.worktree().join("d9/new")).unwrap(), "new file");
    assert!(!b.worktree().join("d2/f2").exists());
    assert_eq!(fs::read_to_string(b.worktree().join("d3/f3")).unwrap(), "content 3");

    // `sync` had to read every racy file to prove the tree clean (40 reads). It must record that:
    // afterwards only the files the sync itself wrote (3 at most) can still be unsettled.
    let before = b.counters().content_reads;
    assert!(before >= 30, "expected the dirty check to read the racy files, got {before}");
    let st = b.status().unwrap();
    assert!(st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()), "{st:?}");
    let full = b.status_full().unwrap();
    assert!(full.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()));
    let after_status = b.counters().content_reads;
    assert!(
        after_status - before <= 6,
        "status after sync re-read {} file contents",
        after_status - before
    );

    // a dirty tree is left alone
    write(&b, "d4/f4", "local edit");
    write(&a, "d0/f0", "again");
    a.add(&["".into()]).unwrap();
    a.commit("edit2", sig()).unwrap();
    a.publish(&remote, None, sig()).unwrap();
    let rep = b.sync(&remote).unwrap();
    assert!(matches!(rep.actions[0].2, SyncAction::Dirty), "{:?}", rep.actions);
    assert_eq!(fs::read_to_string(b.worktree().join("d4/f4")).unwrap(), "local edit");
}
