//! Checkout and status running against the in-memory `WorktreeFs` fake: the whole zone is
//! materialized, edited, switched and sparsified without a single working-tree file on disk.
use std::fs;
use std::sync::Arc;
use vcs_objects::Signature;
use vcs_repo::worktree_fs::MemFs;
use vcs_repo::{ChangeKind, Repo};

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}
fn s(p: &[&str]) -> Vec<String> {
    p.iter().map(|x| x.to_string()).collect()
}

/// A repo whose zone `lib` has two branches, with `lib` unsubscribed afterwards and the
/// working-tree side switched to a fresh `MemFs`.
fn setup() -> (tempfile::TempDir, Repo, Arc<MemFs>) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    r.create_zone("lib", "lib").unwrap();
    for (p, c) in [("lib/a.txt", "a\n"), ("lib/d/b.txt", "b\n"), ("lib/d/e/c.txt", "c\n")] {
        let abs = r.worktree().join(p);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(abs, c).unwrap();
    }
    r.add(&s(&[""])).unwrap();
    r.commit("base", sig()).unwrap();
    r.create_branch("lib", "f").unwrap();
    r.checkout("lib", "f").unwrap();
    fs::write(r.worktree().join("lib/a.txt"), "a-f\n").unwrap();
    fs::remove_file(r.worktree().join("lib/d/b.txt")).unwrap();
    fs::write(r.worktree().join("lib/new.txt"), "n\n").unwrap();
    r.add(&s(&[""])).unwrap();
    r.commit("f", sig()).unwrap();
    r.checkout("lib", "main").unwrap();
    r.zone_remove("lib").unwrap();
    assert!(!r.worktree().join("lib").exists());
    let mem = Arc::new(MemFs::new());
    let r = r.with_worktree_fs(mem.clone());
    (d, r, mem)
}

#[test]
fn checkout_and_status_never_touch_the_disk() {
    let (_d, r, mem) = setup();
    r.zone_add("lib", None).unwrap();
    assert_eq!(mem.paths(), s(&["lib/a.txt", "lib/d/b.txt", "lib/d/e/c.txt"]));
    assert!(!r.worktree().join("lib").exists(), "nothing was written to the real worktree");
    let st = r.status().unwrap();
    assert!(st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()) && st.untracked.is_empty());

    // edits, deletions and untracked files are seen through the fake
    mem.put("lib/a.txt", b"edited\n");
    mem.remove_for_test("lib/d/b.txt");
    mem.put("lib/stray.txt", b"x");
    let st = r.status().unwrap();
    let lib = st.zones.iter().find(|z| z.zone == "lib").unwrap();
    let mut got: Vec<_> = lib.unstaged.iter().map(|c| (c.path.as_str(), c.kind)).collect();
    got.sort_by_key(|(p, _)| *p);
    assert_eq!(got, vec![("lib/a.txt", ChangeKind::Modified), ("lib/d/b.txt", ChangeKind::Deleted)]);
    assert_eq!(st.untracked, s(&["lib/stray.txt"]));
    // a dirty zone refuses to switch
    assert!(r.checkout("lib", "f").is_err());
}

#[test]
fn branch_switch_and_sparse_set_on_the_fake() {
    let (_d, r, mem) = setup();
    r.zone_add("lib", None).unwrap();
    r.checkout("lib", "f").unwrap();
    assert_eq!(mem.get("lib/a.txt").unwrap(), b"a-f\n");
    assert!(mem.get("lib/d/b.txt").is_none() && mem.get("lib/new.txt").is_some());
    let st = r.status().unwrap();
    assert!(st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()));
    r.checkout("lib", "main").unwrap();
    assert_eq!(mem.paths(), s(&["lib/a.txt", "lib/d/b.txt", "lib/d/e/c.txt"]));

    // sparse: leaving files removes them (and the emptied directories), joining writes them
    let rep = r.sparse_set(None, "lib", &s(&["lib/d/e"])).unwrap();
    assert_eq!((rep.materialized, rep.dematerialized), (0, 2));
    assert_eq!(mem.paths(), s(&["lib/d/e/c.txt"]));
    let st = r.status().unwrap();
    assert!(st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()));
    r.checkout("lib", "f").unwrap();
    assert_eq!(
        mem.paths(),
        s(&["lib/d/e/c.txt"]),
        "a.txt and new.txt are tracked but not materialized"
    );
    r.sparse_add(None, "lib", &s(&["lib/new.txt"])).unwrap();
    assert_eq!(mem.get("lib/new.txt").unwrap(), b"n\n");
    r.sparse_disable(None, "lib").unwrap();
    assert_eq!(mem.paths(), s(&["lib/a.txt", "lib/d/e/c.txt", "lib/new.txt"]));
    assert_eq!(mem.get("lib/a.txt").unwrap(), b"a-f\n");
}

#[test]
fn untracked_file_in_the_way_is_refused_on_the_fake() {
    let (_d, r, mem) = setup();
    mem.put("lib/a.txt", b"mine\n");
    let e = r.zone_add("lib", None).unwrap_err().to_string();
    assert!(e.contains("untracked") && e.contains("lib/a.txt"), "{e}");
    assert_eq!(mem.get("lib/a.txt").unwrap(), b"mine\n");
    mem.put("lib/d", b"a file where a directory must go");
    mem.remove_for_test("lib/a.txt");
    assert!(r.zone_add("lib", None).is_err());
}
