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
