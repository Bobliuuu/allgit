//! `reset --mixed|--hard` compares the index with the target tree by subtree (cached tree ids)
//! instead of walking everything. It must give exactly the results of the flat walk it replaces:
//! same reports, same index, same working tree.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use vcs_objects::Signature;
use vcs_repo::reset::set_reset_diff_limit;
use vcs_repo::{Repo, ResetMode, ROOT_ZONE};

fn sig(ts: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: ts,
    }
}
fn write(r: &Repo, p: &str, c: &str) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
}
fn commit(r: &Repo, msg: &str, ts: u64) {
    r.add(&[String::new()]).unwrap();
    r.commit(msg, sig(ts)).unwrap();
}

/// Three commits over a small tree with directories, an added directory and a deletion.
fn build(root: &Path) -> Repo {
    let r = Repo::init(root).unwrap();
    for d in 0..6 {
        for f in 0..40 {
            write(&r, &format!("d{d}/f{f:02}.txt"), &format!("base {d} {f}\n"));
        }
    }
    write(&r, "gone.txt", "will be deleted\n");
    commit(&r, "c1", 1);
    write(&r, "d2/f07.txt", "changed in c2\n");
    write(&r, "d4/new.txt", "added in c2\n");
    fs::remove_file(r.worktree().join("gone.txt")).unwrap();
    write(&r, "d3/extra/inner.txt", "a new directory\n");
    commit(&r, "c2", 2);
    write(&r, "d0/f00.txt", "changed in c3\n");
    write(&r, "d2/f07.txt", "changed again in c3\n");
    fs::remove_file(r.worktree().join("d5/f39.txt")).unwrap();
    commit(&r, "c3", 3);
    r
}

fn tree_of(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if e.file_name() == ".vcs" {
                continue;
            }
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                out.insert(p.strip_prefix(base).unwrap().to_string_lossy().into_owned(), fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Local state to discard or keep: edits, deletions, a staged addition, a staged edit, an untracked file.
fn dirty(r: &Repo) {
    write(r, "d1/f03.txt", "local edit\n");
    fs::remove_file(r.worktree().join("d3/f10.txt")).unwrap();
    write(r, "d4/f20.txt", "staged edit\n");
    r.add(&["d4/f20.txt".to_string()]).unwrap();
    write(r, "staged-new.txt", "staged new\n");
    r.add(&["staged-new.txt".to_string()]).unwrap();
    write(r, "untracked.txt", "u\n");
    write(r, "d2/f07.txt", "local edit on a changed file\n");
}

fn run(
    mode: ResetMode,
    back: usize,
    make_dirty: bool,
    limit: Option<usize>,
) -> (usize, usize, Vec<(String, String)>, BTreeMap<String, Vec<u8>>, String) {
    let d = tempfile::tempdir().unwrap();
    let r = build(&d.path().join("r"));
    if make_dirty {
        dirty(&r);
    }
    let mut target = r.head_commit(ROOT_ZONE).unwrap().unwrap();
    for _ in 0..back {
        target = r.commit_of(&target).unwrap().parents[0];
    }
    set_reset_diff_limit(limit);
    let rep = r.reset(ROOT_ZONE, &target, mode).unwrap();
    set_reset_diff_limit(None);
    let idx = r
        .tracked_files()
        .unwrap()
        .into_iter()
        .map(|t| (t.0, format!("{:?}{}", t.1, t.2)))
        .collect();
    let st = r.status().unwrap();
    let status = format!(
        "{:?}|{:?}",
        st.zones.iter().map(|z| (&z.staged, &z.unstaged)).collect::<Vec<_>>(),
        st.untracked
    );
    (rep.index_changed, rep.files_changed, idx, tree_of(r.worktree()), status)
}

#[test]
fn diff_path_equals_flat_walk() {
    for mode in [ResetMode::Mixed, ResetMode::Hard] {
        for back in [0, 1, 2] {
            for make_dirty in [false, true] {
                let flat = run(mode, back, make_dirty, Some(0));
                let diff = run(mode, back, make_dirty, None);
                assert_eq!(flat.0, diff.0, "index_changed {mode:?} back={back} dirty={make_dirty}");
                assert_eq!(flat.1, diff.1, "files_changed {mode:?} back={back} dirty={make_dirty}");
                assert_eq!(flat.2, diff.2, "index {mode:?} back={back} dirty={make_dirty}");
                assert_eq!(flat.3, diff.3, "worktree {mode:?} back={back} dirty={make_dirty}");
                assert_eq!(flat.4, diff.4, "status {mode:?} back={back} dirty={make_dirty}");
            }
        }
    }
}

#[test]
fn hard_reset_leaves_a_clean_tree_at_the_target() {
    let d = tempfile::tempdir().unwrap();
    let r = build(&d.path().join("r"));
    dirty(&r);
    let c1 = {
        let h = r.head_commit(ROOT_ZONE).unwrap().unwrap();
        let c2 = r.commit_of(&h).unwrap().parents[0];
        r.commit_of(&c2).unwrap().parents[0]
    };
    r.reset(ROOT_ZONE, &c1, ResetMode::Hard).unwrap();
    let st = r.status().unwrap();
    assert!(st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()), "{st:?}");
    assert_eq!(st.untracked, vec!["untracked.txt".to_string()]);
    assert!(r.worktree().join("gone.txt").exists());
    assert!(!r.worktree().join("d3/extra").exists());
    assert_eq!(fs::read_to_string(r.worktree().join("d2/f07.txt")).unwrap(), "base 2 7\n");
}
