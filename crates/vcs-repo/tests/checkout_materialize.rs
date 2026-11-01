//! Materialization of big checkouts: many new files in many new directories (the threaded writer,
//! the per-directory preflight memo, in-place creation of new files) must give exactly the tree
//! the commit names, with exact modes, no temporary leftovers, and the same refusals as before.
use std::fs;
use std::path::Path;
use vcs_objects::Signature;
use vcs_repo::Repo;

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}

fn body(i: usize) -> String {
    format!("content of file {i}\n").repeat(1 + i % 5)
}

fn path_of(i: usize) -> String {
    format!("top{}/mid{}/leaf{}/f{i}.txt", i % 3, i % 7, i % 11)
}

fn leftovers(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn go(dir: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let n = e.file_name().to_string_lossy().into_owned();
            if n == ".vcs" {
                continue;
            }
            if n.starts_with(".vcs-tmp") {
                out.push(n);
            }
            if e.file_type().unwrap().is_dir() {
                go(&e.path(), out);
            }
        }
    }
    go(root, &mut out);
    out
}

/// main holds 40 files; `wide` adds 400 more (all in directories that do not exist on main) and
/// flips the exec bit of a few. Switching to `wide` writes them on the thread pool.
fn setup() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("w")).unwrap();
    let root = r.worktree().to_path_buf();
    for i in 0..40 {
        let p = root.join(format!("base/d{}/b{i}.txt", i % 4));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body(i)).unwrap();
    }
    r.add(&["".into()]).unwrap();
    r.commit("base", sig()).unwrap();
    r.create_branch("root", "wide").unwrap();
    r.checkout("root", "wide").unwrap();
    for i in 0..400 {
        let p = root.join(path_of(i));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, body(i)).unwrap();
        #[cfg(unix)]
        if i % 50 == 0 {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    r.add(&["".into()]).unwrap();
    r.commit("wide", sig()).unwrap();
    r.checkout("root", "main").unwrap();
    (d, r)
}

#[test]
fn big_switch_materializes_exact_tree() {
    let (_d, r) = setup();
    let root = r.worktree().to_path_buf();
    assert!(!root.join("top0").exists());
    r.checkout("root", "wide").unwrap();
    for i in 0..400 {
        let p = root.join(path_of(i));
        assert_eq!(fs::read_to_string(&p).unwrap(), body(i), "{p:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let want = if i % 50 == 0 { 0o755 } else { 0o644 };
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, want, "{p:?}");
        }
    }
    assert!(leftovers(&root).is_empty(), "{:?}", leftovers(&root));
    let st = r.status().unwrap();
    assert!(
        st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()),
        "status not clean"
    );
    assert!(st.untracked.is_empty(), "{:?}", st.untracked);
    // and back: the new directories are pruned again
    r.checkout("root", "main").unwrap();
    assert!(!root.join("top0").exists());
    assert!(r.status().unwrap().untracked.is_empty());
}

#[test]
fn untracked_file_in_a_new_directory_blocks_the_big_switch() {
    let (_d, r) = setup();
    let root = r.worktree().to_path_buf();
    // `top1/mid1/leaf1/` exists on disk (untracked) and holds a file the switch would create.
    let victim = root.join(path_of(1));
    fs::create_dir_all(victim.parent().unwrap()).unwrap();
    fs::write(&victim, "mine, not yours").unwrap();
    let err = r.checkout("root", "wide").unwrap_err().to_string();
    assert!(err.contains("would be overwritten"), "{err}");
    assert_eq!(fs::read_to_string(&victim).unwrap(), "mine, not yours");
    assert!(!root.join(path_of(2)).exists(), "nothing else was written");
    assert!(leftovers(&root).is_empty());
}

#[test]
fn file_where_a_directory_is_needed_blocks_the_big_switch() {
    let (_d, r) = setup();
    let root = r.worktree().to_path_buf();
    fs::write(root.join("top2"), "a file, not a directory").unwrap();
    assert!(r.checkout("root", "wide").is_err());
    assert_eq!(fs::read_to_string(root.join("top2")).unwrap(), "a file, not a directory");
    assert!(!root.join("top0").exists(), "nothing was created");
}

#[cfg(unix)]
#[test]
fn symlinked_directory_in_the_way_is_not_written_through() {
    let (_d, r) = setup();
    let root = r.worktree().to_path_buf();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("top0")).unwrap();
    assert!(r.checkout("root", "wide").is_err());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0, "nothing escaped through the link");
}

#[cfg(unix)]
#[test]
fn restore_recreates_deleted_files_with_exact_modes() {
    let (_d, r) = setup();
    let root = r.worktree().to_path_buf();
    r.checkout("root", "wide").unwrap();
    fs::remove_dir_all(root.join("top0")).unwrap();
    r.restore(&["top0".into()], false).unwrap();
    use std::os::unix::fs::PermissionsExt;
    for i in (0..400).filter(|i| i % 3 == 0) {
        let p = root.join(path_of(i));
        assert_eq!(fs::read_to_string(&p).unwrap(), body(i));
        let want = if i % 50 == 0 { 0o755 } else { 0o644 };
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, want);
    }
    assert!(r.status().unwrap().zones.iter().all(|z| z.unstaged.is_empty()));
}
