//! `add` of a few tracked files (the lazy path) and `add .` after a few changes (the patched
//! write) must leave the same index a full rewrite would: same tracked set, same staged
//! changes, a clean fsck, and a commit that leaves the worktree clean.

use std::collections::BTreeSet;
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
fn tracked(r: &Repo) -> BTreeSet<String> {
    r.tracked_files().unwrap().into_iter().map(|t| t.0).collect()
}
fn staged(r: &Repo) -> BTreeSet<String> {
    r.status().unwrap().zones.iter().flat_map(|z| z.staged.iter().map(|c| c.path.clone())).collect()
}
fn unstaged(r: &Repo) -> BTreeSet<String> {
    r.status().unwrap().zones.iter().flat_map(|z| z.unstaged.iter().map(|c| c.path.clone())).collect()
}

fn big() -> (tempfile::TempDir, Repo, Vec<String>) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let mut all = vec![];
    for k in 0..5000u32 {
        let p = format!("d{:02}/f{k:05}.txt", k % 25);
        let abs = r.worktree().join(&p);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(abs, format!("file {k}\n")).unwrap();
        all.push(p);
    }
    all.sort();
    r.add(&s("")).unwrap();
    r.commit("base", sig()).unwrap();
    (d, r, all)
}

fn assert_sound(r: &Repo, want: &BTreeSet<String>) {
    assert_eq!(&tracked(r), want);
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}

#[test]
fn explicit_paths_stage_through_the_lazy_path() {
    let (_d, r, all) = big();
    let want: BTreeSet<String> = all.iter().cloned().collect();
    let hit = [all[0].clone(), all[2047].clone(), all[2048].clone(), all[4000].clone(), all.last().unwrap().clone()];
    for (i, p) in hit.iter().enumerate() {
        fs::write(r.worktree().join(p), format!("changed {i} with a different length\n")).unwrap();
    }
    let rep = r.add(&hit).unwrap();
    assert_eq!(rep.staged, hit.len());
    assert_eq!(staged(&r), hit.iter().cloned().collect());
    assert!(unstaged(&r).is_empty());
    assert_sound(&r, &want);
    // adding them again changes nothing
    r.add(&hit).unwrap();
    assert_eq!(staged(&r), hit.iter().cloned().collect());
    r.commit("c1", sig()).unwrap();
    assert!(staged(&r).is_empty() && unstaged(&r).is_empty());
    assert_sound(&r, &want);
    // a second round on the committed (tail-patched) index
    fs::write(r.worktree().join(&hit[1]), "again\n").unwrap();
    r.add(&s(&hit[1])).unwrap();
    assert_eq!(staged(&r), s(&hit[1]).into_iter().collect());
    r.commit("c2", sig()).unwrap();
    assert!(staged(&r).is_empty() && unstaged(&r).is_empty());
    assert_sound(&r, &want);
}

#[test]
fn lazy_path_hands_over_to_the_full_one_for_new_and_vanished_paths() {
    let (_d, r, all) = big();
    let mut want: BTreeSet<String> = all.iter().cloned().collect();
    // an untracked file next to a tracked one
    fs::write(r.worktree().join(&all[10]), "mod\n").unwrap();
    fs::write(r.worktree().join("d01/new.txt"), "n\n").unwrap();
    r.add(&[all[10].clone(), "d01/new.txt".into()]).unwrap();
    want.insert("d01/new.txt".into());
    assert_eq!(staged(&r), [all[10].clone(), "d01/new.txt".to_string()].into_iter().collect());
    assert_sound(&r, &want);
    // a vanished tracked file is staged as a removal
    fs::remove_file(r.worktree().join(&all[20])).unwrap();
    let rep = r.add(&s(&all[20])).unwrap();
    assert_eq!(rep.removed, 1);
    want.remove(&all[20]);
    assert_sound(&r, &want);
    // an unknown path is still an error and changes nothing
    assert!(r.add(&s("nope.txt")).is_err());
    assert_sound(&r, &want);
}

#[test]
fn add_dot_after_a_few_changes_patches_the_index() {
    let (_d, r, all) = big();
    let mut want: BTreeSet<String> = all.iter().cloned().collect();
    fs::write(r.worktree().join(&all[5]), "x\n").unwrap();
    fs::write(r.worktree().join(&all[3000]), "y\n").unwrap();
    fs::remove_file(r.worktree().join(&all[4999])).unwrap();
    fs::write(r.worktree().join("d02/added.txt"), "z\n").unwrap();
    want.remove(&all[4999]);
    want.insert("d02/added.txt".into());
    r.add(&s("")).unwrap();
    assert_eq!(
        staged(&r),
        [all[5].clone(), all[3000].clone(), all[4999].clone(), "d02/added.txt".into()].into_iter().collect()
    );
    assert!(unstaged(&r).is_empty());
    assert_sound(&r, &want);
    r.commit("c", sig()).unwrap();
    assert!(staged(&r).is_empty() && unstaged(&r).is_empty());
    // re-adding with nothing changed leaves a clean tree
    r.add(&s("")).unwrap();
    assert!(staged(&r).is_empty() && unstaged(&r).is_empty());
    assert_sound(&r, &want);
}

#[test]
fn symlinks_and_modes_through_the_lazy_path() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let (_d, r, all) = big();
        let want: BTreeSet<String> = all.iter().cloned().collect();
        let p = &all[7];
        fs::set_permissions(r.worktree().join(p), fs::Permissions::from_mode(0o755)).unwrap();
        r.add(&s(p)).unwrap();
        assert_eq!(staged(&r), s(p).into_iter().collect());
        assert_sound(&r, &want);
    }
}
