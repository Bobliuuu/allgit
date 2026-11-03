//! Correctness of the `status` directory cache (#102): cached status must always equal the full
//! (uncached) status, including under renames, same-name replacement, and timestamp tricks.

use proptest::prelude::*;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};
use vcs_repo::{Repo, StatusOpts};

/// Short racy window so a test can settle quickly; production uses 2 s.
const RACY_NS: u64 = 40_000_000;

fn opts() -> StatusOpts {
    StatusOpts {
        dir_cache: true,
        racy_ns: RACY_NS,
    }
}

fn settle() {
    std::thread::sleep(Duration::from_millis(60));
}

type Summary = (Vec<String>, Vec<String>, Vec<String>, Vec<String>);

fn summary(s: &vcs_repo::Status) -> Summary {
    let z = &s.zones[0];
    (
        z.staged.iter().map(|c| format!("{} {:?}", c.path, c.kind)).collect(),
        z.unstaged.iter().map(|c| format!("{} {:?}", c.path, c.kind)).collect(),
        s.untracked.clone(),
        s.unsupported.clone(),
    )
}

fn assert_same(r: &Repo, ctx: &str) {
    let cached = summary(&r.status_with(opts()).unwrap());
    let full = summary(&r.status_full().unwrap());
    assert_eq!(cached, full, "cached != full after {ctx}");
}

fn w(r: &Repo, p: &str, c: &str) {
    let a = r.worktree().join(p);
    fs::create_dir_all(a.parent().unwrap()).unwrap();
    fs::write(a, c).unwrap();
}

fn set_mtime(p: &Path, t: SystemTime) {
    let f = fs::File::open(p).unwrap();
    f.set_modified(t).unwrap();
}

fn base() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    for p in ["a/x.txt", "a/b/y.txt", "a/b/c/z.txt", "d/e/f.txt", "top.txt"] {
        w(&r, p, p);
    }
    r.add(&[String::new()]).unwrap();
    r.commit(
        "base",
        vcs_objects::Signature {
            name: "T".into(),
            email: "t@x".into(),
            timestamp: 1,
        },
    )
    .unwrap();
    settle();
    (d, r)
}

const DIRS: [&str; 6] = ["", "a", "a/b", "a/b/c", "d", "d/e"];
const NAMES: [&str; 5] = ["n1", "n2", "x.txt", "y.txt", "f.txt"];

#[derive(Clone, Debug)]
enum Op {
    Create(usize, usize, u8),
    Edit(usize, usize, u8),
    Remove(usize, usize),
    Rename(usize, usize, usize, usize),
    Mkdir(usize, usize),
    /// Replace a file by a directory of the same name (and back).
    Swap(usize, usize),
    /// Add or remove an entry, then reset the directory's mtime to its old value.
    RestoreDirMtime(usize, usize),
    /// Set a file's mtime to a fixed old time (content may differ, size equal).
    OldMtime(usize, usize, u8),
    Add,
    Commit,
    Settle,
}

fn op() -> impl Strategy<Value = Op> {
    let d = 0..DIRS.len();
    let n = 0..NAMES.len();
    prop_oneof![
        (d.clone(), n.clone(), any::<u8>()).prop_map(|(a, b, c)| Op::Create(a, b, c)),
        (d.clone(), n.clone(), any::<u8>()).prop_map(|(a, b, c)| Op::Edit(a, b, c)),
        (d.clone(), n.clone()).prop_map(|(a, b)| Op::Remove(a, b)),
        (d.clone(), n.clone(), d.clone(), n.clone()).prop_map(|(a, b, c, e)| Op::Rename(a, b, c, e)),
        (d.clone(), n.clone()).prop_map(|(a, b)| Op::Mkdir(a, b)),
        (d.clone(), n.clone()).prop_map(|(a, b)| Op::Swap(a, b)),
        (d.clone(), n.clone()).prop_map(|(a, b)| Op::RestoreDirMtime(a, b)),
        (d.clone(), n, any::<u8>()).prop_map(|(a, b, c)| Op::OldMtime(a, b, c)),
        Just(Op::Add),
        Just(Op::Commit),
        Just(Op::Settle),
    ]
}

fn path(d: usize, n: usize) -> String {
    if DIRS[d].is_empty() {
        NAMES[n].to_owned()
    } else {
        format!("{}/{}", DIRS[d], NAMES[n])
    }
}

fn apply(r: &Repo, o: &Op, step: u64) {
    let root = r.worktree();
    match o {
        Op::Create(d, n, c) => {
            let p = path(*d, *n);
            if !root.join(&p).exists() {
                w(r, &p, &format!("c{c}"));
            }
        }
        Op::Edit(d, n, c) => {
            let p = path(*d, *n);
            if root.join(&p).is_file() {
                fs::write(root.join(&p), format!("e{c}{step}")).unwrap();
            }
        }
        Op::Remove(d, n) => {
            let p = root.join(path(*d, *n));
            if p.is_file() {
                fs::remove_file(p).unwrap();
            } else if p.is_dir() {
                let _ = fs::remove_dir_all(p);
            }
        }
        Op::Rename(d, n, d2, n2) => {
            let (a, b) = (root.join(path(*d, *n)), root.join(path(*d2, *n2)));
            if a.exists() && !b.exists() && !b.starts_with(&a) {
                let _ = fs::rename(a, b);
            }
        }
        Op::Mkdir(d, n) => {
            let p = root.join(path(*d, *n));
            if !p.exists() {
                fs::create_dir_all(&p).unwrap();
                fs::write(p.join("inner"), "i").unwrap();
            }
        }
        Op::Swap(d, n) => {
            let p = root.join(path(*d, *n));
            if p.is_file() {
                fs::remove_file(&p).unwrap();
                fs::create_dir(&p).unwrap();
                fs::write(p.join("sub"), "s").unwrap();
            } else if p.is_dir() {
                fs::remove_dir_all(&p).unwrap();
                fs::write(&p, "back").unwrap();
            }
        }
        Op::RestoreDirMtime(d, n) => {
            let dir = root.join(DIRS[*d]);
            let old = fs::metadata(&dir).unwrap().modified().unwrap();
            let p = dir.join(NAMES[*n]);
            if p.is_file() {
                fs::remove_file(&p).unwrap();
            } else if !p.exists() {
                fs::write(&p, "sneaky").unwrap();
            }
            set_mtime(&dir, old);
        }
        Op::OldMtime(d, n, c) => {
            let p = root.join(path(*d, *n));
            if p.is_file() {
                let len = fs::metadata(&p).unwrap().len() as usize;
                fs::write(&p, vec![b'a' + (c % 26); len.max(1)]).unwrap();
                set_mtime(&p, SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000));
            }
        }
        Op::Add => {
            let _ = r.add(&[String::new()]);
        }
        Op::Commit => {
            let _ = r.commit(
                "m",
                vcs_objects::Signature {
                    name: "T".into(),
                    email: "t@x".into(),
                    timestamp: 2 + step,
                },
            );
        }
        Op::Settle => settle(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, .. ProptestConfig::default() })]
    #[test]
    fn cached_status_equals_full_status(ops in proptest::collection::vec(op(), 1..14)) {
        let (_d, r) = base();
        assert_same(&r, "setup"); // warms the cache
        for (i, o) in ops.iter().enumerate() {
            apply(&r, o, i as u64);
            // check twice: the first may refill the cache, the second must use it
            assert_same(&r, &format!("{o:?} (1)"));
            settle();
            assert_same(&r, &format!("{o:?} (2)"));
            assert_same(&r, &format!("{o:?} (3)"));
        }
    }
}

#[test]
fn warm_cache_is_used_and_new_file_is_seen() {
    let (_d, r) = base();
    assert_same(&r, "cold");
    assert!(r.worktree().join(".vcs/dircache").exists(), "cache file written");
    assert_same(&r, "warm");
    w(&r, "a/b/new.txt", "n");
    let s = r.status_with(opts()).unwrap();
    assert!(s.untracked.contains(&"a/b/new.txt".to_string()), "{:?}", s.untracked);
}

#[test]
fn restored_directory_mtime_does_not_hide_new_file() {
    let (_d, r) = base();
    assert_same(&r, "cold");
    assert_same(&r, "warm");
    let dir = r.worktree().join("a/b");
    let old = fs::metadata(&dir).unwrap().modified().unwrap();
    fs::write(dir.join("hidden.txt"), "h").unwrap();
    set_mtime(&dir, old);
    let s = r.status_with(opts()).unwrap();
    assert!(s.untracked.contains(&"a/b/hidden.txt".to_string()), "{:?}", s.untracked);
}

#[test]
fn corrupt_cache_is_ignored() {
    let (_d, r) = base();
    assert_same(&r, "cold");
    fs::write(r.worktree().join(".vcs/dircache"), b"garbage").unwrap();
    assert_same(&r, "after corruption");
}

#[test]
fn racy_directory_is_never_cached() {
    let (_d, r) = base();
    // default 2 s window: a directory touched just now must always be rescanned
    w(&r, "a/fresh1.txt", "1");
    let _ = r.status().unwrap();
    w(&r, "a/fresh2.txt", "2");
    let s = r.status().unwrap();
    assert!(s.untracked.contains(&"a/fresh2.txt".to_string()), "{:?}", s.untracked);
}

#[test]
fn tracked_file_under_a_path_that_became_a_file_is_deleted_not_an_error() {
    let (_d, r) = base();
    // `a/b/y.txt` is tracked; replace directory `a/b` by a plain file.
    fs::remove_dir_all(r.worktree().join("a/b")).unwrap();
    fs::write(r.worktree().join("a/b"), "now a file").unwrap();
    let s = r.status_full().unwrap();
    let un: Vec<_> = s.zones[0].unstaged.iter().map(|c| c.path.as_str()).collect();
    assert!(un.contains(&"a/b/y.txt") && un.contains(&"a/b/c/z.txt"), "{un:?}");
    assert_same(&r, "dir replaced by file");
}
