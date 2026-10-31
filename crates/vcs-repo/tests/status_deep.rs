//! Regression tests for #22: `status` output on deep directory layouts must be exactly what the
//! straightforward algorithm (flatten HEAD, compare with index) produces. The fast path that
//! skips flattening HEAD when the index hashes to HEAD's root must never hide a staged change.

use std::fs;
use vcs_objects::Signature;
use vcs_repo::{ChangeKind, Repo};

fn sig(ts: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: ts,
    }
}

/// depth-6, fan-out 3 tree with one file per leaf plus a few shallow files.
fn deep_paths() -> Vec<String> {
    let mut v = Vec::new();
    for i in 0..243usize {
        let mut p = String::new();
        let mut x = i;
        for _ in 0..5 {
            p.push_str(&format!("l{}/", x % 3));
            x /= 3;
        }
        v.push(format!("{p}f{i}.txt"));
    }
    v.push("top.txt".into());
    v.push("l0/mid.txt".into());
    v
}

fn write(r: &Repo, p: &str, c: &str) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
}

fn setup() -> (tempfile::TempDir, Repo, Vec<String>) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let paths = deep_paths();
    for (i, p) in paths.iter().enumerate() {
        write(&r, p, &format!("content {i}\n"));
    }
    r.add(&[String::new()]).unwrap();
    r.commit("base", sig(1)).unwrap();
    (d, r, paths)
}

type Changes = Vec<(String, ChangeKind)>;

fn summary(r: &Repo) -> (Changes, Changes, Vec<String>) {
    let s = r.status().unwrap();
    let a = &s.zones[0];
    (
        a.staged.iter().map(|c| (c.path.clone(), c.kind)).collect(),
        a.unstaged.iter().map(|c| (c.path.clone(), c.kind)).collect(),
        s.untracked.clone(),
    )
}

#[test]
fn clean_deep_tree_reports_nothing() {
    let (_d, r, _) = setup();
    let (staged, unstaged, untracked) = summary(&r);
    assert!(staged.is_empty() && unstaged.is_empty() && untracked.is_empty());
}

#[test]
fn staged_changes_in_deep_tree_are_never_hidden() {
    let (_d, r, paths) = setup();
    // staged modification deep down
    write(&r, &paths[10], "changed\n");
    r.add(std::slice::from_ref(&paths[10])).unwrap();
    // staged deletion
    fs::remove_file(r.worktree().join(&paths[20])).unwrap();
    r.add(std::slice::from_ref(&paths[20])).unwrap();
    // staged addition in a brand-new deep directory
    write(&r, "l1/l1/l1/new/dir/added.txt", "new\n");
    r.add(&["l1/l1/l1/new/dir/added.txt".to_string()]).unwrap();

    let (staged, unstaged, untracked) = summary(&r);
    let mut expected = vec![
        (paths[10].clone(), ChangeKind::Modified),
        (paths[20].clone(), ChangeKind::Deleted),
        ("l1/l1/l1/new/dir/added.txt".to_string(), ChangeKind::Added),
    ];
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(staged, expected);
    assert!(unstaged.is_empty());
    assert!(untracked.is_empty());

    // after committing, everything is clean again (fast path back on) and still correct
    r.commit("second", sig(2)).unwrap();
    let (staged, unstaged, untracked) = summary(&r);
    assert!(staged.is_empty() && unstaged.is_empty() && untracked.is_empty());
}

#[test]
fn unstaged_and_untracked_in_deep_tree() {
    let (_d, r, paths) = setup();
    write(&r, &paths[5], "edited, not staged\n");
    fs::remove_file(r.worktree().join(&paths[7])).unwrap();
    // untracked file next to tracked ones, and a wholly untracked deep directory
    let sibling = format!("{}/untracked.txt", paths[30].rsplit_once('/').unwrap().0);
    write(&r, &sibling, "u\n");
    write(&r, "zz/a/b/c/d/e/f.txt", "u\n");

    let (staged, unstaged, untracked) = summary(&r);
    assert!(staged.is_empty());
    let mut want = vec![(paths[5].clone(), ChangeKind::Modified), (paths[7].clone(), ChangeKind::Deleted)];
    want.sort_by(|a, b| a.0.cmp(&b.0));
    let mut got = unstaged.clone();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(got, want);
    assert!(untracked.contains(&sibling), "{untracked:?}");
    assert!(untracked.contains(&"zz/".to_string()), "{untracked:?}");
    assert_eq!(untracked.len(), 2, "{untracked:?}");
}
