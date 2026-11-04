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

/// Property test for #119: for random zone histories, the tree a checkout writes through `MemFs` equals the tree it
/// writes to disk, for every branch (content and file set).
#[test]
fn memfs_checkout_equals_real_checkout_on_random_histories() {
    use std::collections::BTreeMap;
    for seed in 1..=8u64 {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = move |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n
        };
        let d = tempfile::tempdir().unwrap();
        let r = Repo::init(&d.path().join("r")).unwrap();
        r.create_zone("lib", "lib").unwrap();
        let names = ["a.txt", "d/b.txt", "d/e/c.txt", "d/e/f/g.txt", "h.txt", "d/i.txt"];
        let mut branches = vec!["main".to_string()];
        for step in 0..6 {
            if step > 0 {
                let b = format!("b{step}");
                r.create_branch("lib", &b).unwrap();
                r.checkout("lib", &b).unwrap();
                branches.push(b);
            }
            for n in names {
                let abs = r.worktree().join("lib").join(n);
                if next(3) == 0 {
                    let _ = fs::remove_file(&abs);
                } else {
                    fs::create_dir_all(abs.parent().unwrap()).unwrap();
                    fs::write(&abs, format!("{seed}-{step}-{}\n", next(5))).unwrap();
                }
            }
            r.add(&s(&[""])).unwrap();
            let _ = r.commit(&format!("c{step}"), sig());
        }
        for b in &branches {
            r.checkout("lib", b).unwrap();
            let mut real = BTreeMap::new();
            for n in names {
                if let Ok(c) = fs::read(r.worktree().join("lib").join(n)) {
                    real.insert(format!("lib/{n}"), c);
                }
            }
            r.zone_remove("lib").unwrap();
            let mem = Arc::new(MemFs::new());
            let rm = Repo::discover(r.worktree()).unwrap().with_worktree_fs(mem.clone());
            rm.zone_add("lib", Some(b)).unwrap();
            let got: BTreeMap<_, _> = mem.paths().into_iter().map(|p| (p.clone(), mem.get(&p).unwrap())).collect();
            assert_eq!(got, real, "seed {seed} branch {b}");
            rm.zone_remove("lib").unwrap();
            r.zone_add("lib", Some(b)).unwrap();
        }
    }
}
