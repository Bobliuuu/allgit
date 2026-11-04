//! `rm`, `rm --cached` and `restore --staged` patch the index file instead of rewriting it
//! whole. On an index spanning several blocks the results must match a plain model of the
//! tracked set, and `fsck`/`status` must stay sound.

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
fn names(files: &[String]) -> BTreeSet<String> {
    files.iter().cloned().collect()
}

#[test]
fn rm_and_restore_staged_on_a_multi_block_index() {
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
    assert_eq!(tracked(&r), names(&all));

    // rm --cached of one file in the middle, one at each end
    for p in [&all[2500], &all[0], all.last().unwrap()] {
        r.rm(&s(p), true, false).unwrap();
    }
    let mut want = names(&all);
    for p in [&all[2500], &all[0], all.last().unwrap()] {
        want.remove(p.as_str());
    }
    assert_eq!(tracked(&r), want);
    assert!(r.worktree().join(&all[2500]).exists(), "--cached keeps the file");

    // restore --staged puts one back, the directory form puts back the rest
    r.restore(&s(&all[2500]), true).unwrap();
    assert!(tracked(&r).contains(&all[2500]));
    r.restore(&s(""), true).unwrap();
    assert_eq!(tracked(&r), names(&all));

    // rm of a whole directory removes its files from disk and the index
    let gone: Vec<&String> = all.iter().filter(|p| p.starts_with("d07/")).collect();
    let rep = r.rm(&s("d07"), false, false).unwrap();
    assert_eq!(rep.removed.len(), gone.len());
    assert!(!r.worktree().join("d07").exists());
    assert!(tracked(&r).iter().all(|p| !p.starts_with("d07/")));
    r.restore(&s("d07"), true).unwrap();
    assert_eq!(tracked(&r), names(&all));

    // the patched index is a normal one: fsck and status are clean apart from the deleted dir
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
    r.restore(&s("d07"), false).unwrap();
    let st = r.status().unwrap();
    assert!(st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()), "{st:?}");

    // a new file added after patches lands in the right place
    fs::write(r.worktree().join("d03/new.txt"), "n\n").unwrap();
    r.add(&s("d03/new.txt")).unwrap();
    assert!(tracked(&r).contains("d03/new.txt"));
    r.rm(&s("d03/new.txt"), true, true).unwrap();
    assert!(!tracked(&r).contains("d03/new.txt"));
}

#[test]
fn a_crash_while_writing_the_patched_index_keeps_the_old_one() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    for k in 0..300u32 {
        fs::write(r.worktree().join(format!("f{k:03}.txt")), format!("{k}\n")).unwrap();
    }
    r.add(&s("")).unwrap();
    r.commit("base", sig()).unwrap();
    let before = tracked(&r);
    vcs_repo::failpoint::reset();
    vcs_repo::failpoint::arm(Some(0)); // the index write is the first state write of rm --cached
    assert!(r.rm(&s("f007.txt"), true, false).is_err());
    vcs_repo::failpoint::arm(None);
    assert_eq!(tracked(&r), before, "the failed rm left the old index");
    r.rm(&s("f007.txt"), true, false).unwrap();
    assert_eq!(tracked(&r).len(), before.len() - 1);
}

#[test]
fn a_torn_index_is_detected_and_rebuilt_at_open_never_read_as_empty() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("r");
    let r = Repo::init(&root).unwrap();
    for k in 0..3000u32 {
        fs::write(r.worktree().join(format!("f{k:04}.txt")), format!("{k}\n")).unwrap();
    }
    r.add(&s("")).unwrap();
    r.commit("base", sig()).unwrap();
    r.rm(&s("f0007.txt"), true, false).unwrap(); // leaves a patched (AGI4) index
    let want: BTreeSet<String> = (0..3000u32).map(|k| format!("f{k:04}.txt")).collect();
    let index = root.join(".vcs/index");
    let whole = fs::read(&index).unwrap();
    assert!(whole.starts_with(b"AGI4"));
    drop(r);
    for cut in [whole.len() / 2, whole.len() - 1, 40, 4, 0] {
        fs::write(&index, &whole[..cut]).unwrap();
        let r = Repo::discover(&root).unwrap();
        let got = tracked(&r);
        assert!(!got.is_empty(), "cut at {cut}: the torn index was read as empty");
        assert_eq!(got, want, "cut at {cut}: rebuilt from the branch head");
        // the files are untouched, and the lost staged removal shows as an untracked-free, clean tree
        assert!(r.worktree().join("f0007.txt").exists());
        let st = r.status().unwrap();
        assert!(st.zones.iter().all(|z| z.unstaged.is_empty()), "cut at {cut}: {st:?}");
        // and the repaired index is whole again
        assert!(Repo::discover(&root).is_ok());
        assert!(vcs_repo::Repo::discover(&root).is_ok() && fs::read(&index).unwrap().starts_with(b"AGI"));
    }
}

/// The "index names no attribute file" marker must follow every kind of index write: a patch
/// (`rm`, `restore --staged`) and a full rewrite (`add`) never leave a stale "none".
#[test]
fn attrs_marker_follows_index_writes() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    for k in 0..600u32 {
        let p = format!("d{:02}/f{k:05}.txt", k % 7);
        let abs = r.worktree().join(&p);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(abs, format!("file {k}\n")).unwrap();
    }
    r.add(&s("")).unwrap();
    r.commit("base", sig()).unwrap();
    let lockable = |r: &Repo, p: &str| r.attrs().unwrap().lookup(p).lockable;
    assert!(!lockable(&r, "d01/x.psd"));
    assert!(!lockable(&r, "d01/x.psd")); // second call runs on the marker

    // a patch write keeps the marker valid; the next index write that adds the file must drop it
    r.rm(&s("d00/f00000.txt"), true, false).unwrap();
    assert!(!lockable(&r, "d01/x.psd"));
    fs::write(r.worktree().join("d01/.vcsattributes"), "*.psd lockable\n").unwrap();
    r.add(&s("d01/.vcsattributes")).unwrap();
    assert!(lockable(&r, "d01/x.psd"), "attribute file added to the index is seen");
    assert!(!lockable(&r, "d02/x.psd"));

    // removing it again (patch) makes the directory plain; the working file is gone too
    r.rm(&s("d01/.vcsattributes"), false, true).unwrap();
    assert!(!lockable(&r, "d01/x.psd"));
    // and the marker never hides one that comes back
    fs::write(r.worktree().join("d01/.vcsattributes"), "*.psd lockable\n").unwrap();
    r.add(&s("d01/.vcsattributes")).unwrap();
    assert!(lockable(&r, "d01/x.psd"));
}

#[test]
fn add_and_restore_of_named_files_patch_the_index_and_match_the_full_path() {
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
    let clean = |r: &Repo| {
        let st = r.status().unwrap();
        st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty())
    };
    assert!(clean(&r));

    // restore of one edited file and of one deleted file (patched), others untouched
    let (a, b, c) = (&all[10], &all[2500], &all[4990]);
    fs::write(r.worktree().join(a), "edited a\n").unwrap();
    fs::write(r.worktree().join(c), "edited c\n").unwrap();
    fs::remove_file(r.worktree().join(b)).unwrap();
    assert_eq!(r.restore(&s(a), false).unwrap().restored, 1);
    let k: u32 = a[a.find('f').unwrap() + 1..a.len() - 4].parse().unwrap();
    assert_eq!(fs::read_to_string(r.worktree().join(a)).unwrap(), format!("file {k}\n"));
    assert_eq!(
        fs::read_to_string(r.worktree().join(c)).unwrap(),
        "edited c\n",
        "restore of a touches only a"
    );
    assert_eq!(r.restore(&s(b), false).unwrap().restored, 1);
    assert!(r.worktree().join(b).exists());
    // a clean file restores nothing
    assert_eq!(r.restore(&s(&all[7]), false).unwrap().restored, 0);

    // add of tracked, edited files stages exactly them
    fs::write(r.worktree().join(a), "second a\n").unwrap();
    let rep = r.add(&[a.clone(), c.clone()]).unwrap();
    assert_eq!(rep.staged, 2);
    let st = r.status().unwrap();
    let staged: BTreeSet<String> = st.zones.iter().flat_map(|z| z.staged.iter().map(|x| x.path.clone())).collect();
    assert_eq!(staged, names(&[a.clone(), c.clone()]));
    assert!(st.zones.iter().all(|z| z.unstaged.is_empty()), "{st:?}");
    // add of a file that was deleted records the removal (full path), still correct
    fs::remove_file(r.worktree().join(b)).unwrap();
    r.add(&s(b)).unwrap();
    assert!(!tracked(&r).contains(b.as_str()));
    r.commit("edits", sig()).unwrap();
    assert!(clean(&r));
    assert_eq!(tracked(&r).len(), 4999);
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}

#[test]
fn adding_new_files_patches_the_index_and_still_refuses_case_collisions() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    for k in 0..4000u32 {
        let p = format!("Dir{:02}/f{k:05}.txt", k % 20);
        let abs = r.worktree().join(&p);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(abs, format!("file {k}\n")).unwrap();
    }
    r.add(&s("")).unwrap();
    r.commit("base", sig()).unwrap();
    let write = |p: &str| {
        let abs = r.worktree().join(p);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(abs, "n\n").unwrap();
    };
    // plain new files: top level, an existing directory, a new directory
    for p in ["new1.txt", "Dir03/new2.txt", "fresh/deep/new3.txt"] {
        write(p);
        let rep = r.add(&s(p)).unwrap();
        assert_eq!(rep.staged, 1, "{p}");
        assert!(tracked(&r).contains(p), "{p}");
    }
    // case-variant of a tracked directory, of a tracked file, of each other
    write("dir03/x.txt");
    let e = r.add(&s("dir03/x.txt")).unwrap_err().to_string();
    assert!(e.contains("differ only by case"), "{e}");
    write("NEW1.txt");
    let e = r.add(&s("NEW1.txt")).unwrap_err().to_string();
    assert!(e.contains("differ only by case"), "{e}");
    write("Aa.txt");
    write("aA.txt");
    let e = r.add(&["Aa.txt".to_string(), "aA.txt".to_string()]).unwrap_err().to_string();
    assert!(e.contains("differ only by case"), "{e}");
    assert!(!tracked(&r).contains("Aa.txt") && !tracked(&r).contains("dir03/x.txt"));
    // staged state is sound
    let st = r.status().unwrap();
    let staged: BTreeSet<String> = st.zones.iter().flat_map(|z| z.staged.iter().map(|x| x.path.clone())).collect();
    assert_eq!(
        staged,
        names(&["new1.txt".into(), "Dir03/new2.txt".into(), "fresh/deep/new3.txt".into()])
    );
    r.commit("more", sig()).unwrap();
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}

/// The case-collision check steps over whole directories instead of reading every entry; it must still
/// find a clash with a directory, a root file, or a file inside a directory, wherever it sorts among
/// blocks, and must not report one where none exists.
#[test]
fn collision_check_that_skips_directories_agrees_with_the_obvious_rule() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    for k in 0..9000u32 {
        let p = format!("Dir{:02}/f{k:05}.txt", k % 30);
        let abs = r.worktree().join(&p);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(abs, format!("file {k}\n")).unwrap();
    }
    for p in ["Mid.txt", "zeta.txt", "Alpha.txt"] {
        fs::write(r.worktree().join(p), "r\n").unwrap();
    }
    r.add(&s("")).unwrap();
    r.commit("base", sig()).unwrap();
    let write = |p: &str| {
        let abs = r.worktree().join(p);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(abs, "n\n").unwrap();
    };
    for bad in ["dir00/x", "DIR29/x", "dir15/x", "mid.TXT", "ZETA.txt", "alpha.txt", "Dir07/F00007.txt", "Dir29/F08999.TXT"] {
        write(bad);
        let e = r.add(&s(bad)).unwrap_err().to_string();
        assert!(e.contains("differ only by case"), "{bad}: {e}");
        assert!(!tracked(&r).contains(bad), "{bad}");
    }
    for good in ["Dir00/new.txt", "Dir29/zz.txt", "Dir30/x.txt", "Between.txt", "Dir15/f99999.txt", "zz.txt", "AAA.txt"] {
        write(good);
        assert_eq!(r.add(&s(good)).unwrap().staged, 1, "{good}");
        assert!(tracked(&r).contains(good), "{good}");
    }
    r.commit("more", sig()).unwrap();
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}
