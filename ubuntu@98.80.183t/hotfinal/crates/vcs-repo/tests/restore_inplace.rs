//! `restore` replaces edited files by unlinking and re-creating them in place (no temp file and
//! rename). The visible result must be the same as before: contents, modes, a clean status.

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
fn s(p: &str) -> Vec<String> {
    vec![p.to_string()]
}

fn setup() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    for k in 0..400u32 {
        let abs = r.worktree().join(format!("d{}/f{k:03}.txt", k % 8));
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(abs, format!("content {k}\n")).unwrap();
    }
    r.add(&s("")).unwrap();
    r.commit("base", sig()).unwrap();
    (d, r)
}

fn clean(r: &Repo) -> bool {
    let st = r.status().unwrap();
    st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty())
}

#[test]
fn restore_dot_discards_edits_and_brings_back_deleted_files() {
    let (_d, r) = setup();
    for k in (0..400u32).step_by(7) {
        fs::write(
            r.worktree().join(format!("d{}/f{k:03}.txt", k % 8)),
            "edited, and longer than the original\n",
        )
        .unwrap();
    }
    for k in [1u32, 2, 3, 4, 5] {
        fs::remove_file(r.worktree().join(format!("d{}/f{k:03}.txt", k % 8))).unwrap();
    }
    assert!(!clean(&r));
    let rep = r.restore(&s(""), false).unwrap();
    assert!(rep.restored > 50);
    assert!(clean(&r));
    for k in 0..400u32 {
        let got = fs::read_to_string(r.worktree().join(format!("d{}/f{k:03}.txt", k % 8))).unwrap();
        assert_eq!(got, format!("content {k}\n"));
    }
}

#[test]
fn restore_of_a_directory_and_a_file_touches_only_that() {
    let (_d, r) = setup();
    fs::write(r.worktree().join("d0/f000.txt"), "x\n").unwrap();
    fs::write(r.worktree().join("d1/f001.txt"), "y\n").unwrap();
    fs::write(r.worktree().join("d2/f002.txt"), "z\n").unwrap();
    r.restore(&s("d0"), false).unwrap();
    assert_eq!(fs::read_to_string(r.worktree().join("d0/f000.txt")).unwrap(), "content 0\n");
    assert_eq!(fs::read_to_string(r.worktree().join("d1/f001.txt")).unwrap(), "y\n");
    r.restore(&["d1/f001.txt".to_string(), "d2/f002.txt".to_string(), "d2".to_string()], false)
        .unwrap();
    assert!(clean(&r));
    assert!(r.restore(&s("nope"), false).is_err());
}

#[cfg(unix)]
#[test]
fn restore_keeps_the_executable_bit_and_replaces_a_symlink() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    fs::write(r.worktree().join("run.sh"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(r.worktree().join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(r.worktree().join("plain.txt"), "plain\n").unwrap();
    r.add(&s("")).unwrap();
    r.commit("base", sig()).unwrap();
    fs::write(r.worktree().join("run.sh"), "changed\n").unwrap();
    fs::remove_file(r.worktree().join("plain.txt")).unwrap();
    symlink("run.sh", r.worktree().join("plain.txt")).unwrap(); // a link where a file is tracked
    r.restore(&s(""), false).unwrap();
    assert_eq!(fs::read_to_string(r.worktree().join("run.sh")).unwrap(), "#!/bin/sh\n");
    assert_eq!(
        fs::metadata(r.worktree().join("run.sh")).unwrap().permissions().mode() & 0o111,
        0o111
    );
    assert!(!fs::symlink_metadata(r.worktree().join("plain.txt"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read_to_string(r.worktree().join("plain.txt")).unwrap(), "plain\n");
    assert!(clean(&r));
}
