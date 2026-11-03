//! The fast diff paths (stat-cache candidates, cache-tree walk, pruned tree diff) return exactly what the
//! read-everything reference returns, on random histories with a second zone, symlinks and path limits.
use std::collections::BTreeSet;
use std::fs;
use vcs_objects::{EntryMode, Signature};
use vcs_repo::{FilePair, Repo};

fn sig(t: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: t,
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self, m: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % m
    }
}

const DIRS: &[&str] = &["", "a/", "a/b/", "c/", "z/", "z/q/"];

/// Reference for `diff A B` / `diff REV`: flatten everything (what the CLI did before the fast path).
fn ref_pairs(r: &Repo, old: &vcs_repo::treeops::Flat, new: &vcs_repo::treeops::Flat) -> Vec<FilePair> {
    let mut keys: BTreeSet<&String> = old.keys().collect();
    keys.extend(new.keys());
    let mut out = vec![];
    for k in keys {
        let (o, n) = (old.get(k), new.get(k));
        if o == n {
            continue;
        }
        out.push(FilePair {
            path: k.clone(),
            old: o.map(|(_, id)| r.content(id).unwrap()),
            new: n.map(|(_, id)| r.content(id).unwrap()),
            old_mode: o.map(|(m, _)| *m),
            new_mode: n.map(|(m, _)| *m),
        });
    }
    out
}

fn ref_rev_worktree(r: &Repo, rev: &str) -> Vec<FilePair> {
    let old = r.rev_files(rev).unwrap();
    let mut keys: BTreeSet<String> = old.keys().cloned().collect();
    keys.extend(r.tracked_files().unwrap().into_iter().map(|(p, _, _)| p));
    let mut out = vec![];
    for k in keys {
        let new = fs::read(r.worktree().join(&k)).ok();
        let o = old.get(&k);
        let same = match (&o, &new) {
            (Some((_, id)), Some(n)) => &r.content(id).unwrap() == n,
            (None, None) => true,
            _ => false,
        };
        if same {
            continue;
        }
        out.push(FilePair {
            path: k.clone(),
            old: o.map(|(_, id)| r.content(id).unwrap()),
            old_mode: o.map(|(m, _)| *m),
            new_mode: new.as_ref().map(|_| o.map_or(EntryMode::File, |(m, _)| *m)),
            new,
        });
    }
    out
}

fn ref_rev_index(r: &Repo, rev: &str) -> Vec<FilePair> {
    let old = r.rev_files(rev).unwrap();
    let new: vcs_repo::treeops::Flat = r.tracked_files().unwrap().into_iter().map(|(p, m, id)| (p, (m, id))).collect();
    ref_pairs(r, &old, &new)
}

fn limit(v: Vec<FilePair>, only: &[String]) -> Vec<FilePair> {
    v.into_iter().filter(|p| vcs_repo::diff_path_kept(only, &p.path)).collect()
}

fn check(r: &Repo, commits: &[String], rng: &mut Rng, seen: &mut [usize; 4]) {
    let onlys: Vec<Vec<String>> = vec![
        vec![],
        vec!["a".into()],
        vec!["a/b".into(), "z".into()],
        vec!["c/f1".into(), "nothing".into()],
    ];
    for only in &onlys {
        for staged in [false, true] {
            let want = limit(r.diff_pairs_full(staged).unwrap(), only);
            assert_eq!(
                r.diff_pairs_limited(staged, only).unwrap(),
                want,
                "diff_pairs staged={staged} only={only:?}"
            );
            seen[staged as usize] += want.len();
        }
        if !commits.is_empty() {
            let rev = &commits[rng.next(commits.len() as u64) as usize];
            if !r.rev_resolves(rev) {
                continue;
            }
            let want = limit(ref_rev_worktree(r, rev), only);
            seen[2] += want.len();
            assert_eq!(r.rev_diff_pairs(rev, false, only).unwrap(), want, "rev vs worktree {rev}");
            assert_eq!(
                r.rev_diff_pairs(rev, true, only).unwrap(),
                limit(ref_rev_index(r, rev), only),
                "rev vs index {rev}"
            );
            let rev2 = &commits[rng.next(commits.len() as u64) as usize];
            if !r.rev_resolves(rev2) {
                continue;
            }
            let want = limit(ref_pairs(r, &r.rev_files(rev).unwrap(), &r.rev_files(rev2).unwrap()), only);
            seen[3] += want.len();
            assert_eq!(r.rev_rev_pairs(rev, rev2, only).unwrap(), want, "{rev}..{rev2}");
        }
    }
    for staged in [false, true] {
        assert_eq!(r.diff(staged).unwrap(), r.diff_full(staged).unwrap());
    }
}

#[test]
fn fast_diffs_equal_the_reference_on_random_histories() {
    for seed in 1..=4u64 {
        let d = tempfile::tempdir().unwrap();
        let r = Repo::init(&d.path().join("r")).unwrap();
        r.create_zone("z", "z").unwrap();
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ seed.wrapping_mul(0x1234_5678_9abc));
        let mut commits: Vec<String> = vec![];
        let mut seen = [0usize; 4];
        let mut t = 1000;
        for step in 0..40 {
            for _ in 0..1 + rng.next(6) {
                let dir = DIRS[rng.next(DIRS.len() as u64) as usize];
                let name = format!("{dir}f{}", rng.next(5));
                let path = r.worktree().join(&name);
                match rng.next(10) {
                    0 | 1 => {
                        let _ = fs::remove_file(&path);
                    }
                    2 => {
                        // a binary file
                        fs::create_dir_all(path.parent().unwrap()).unwrap();
                        let _ = fs::remove_file(&path);
                        fs::write(&path, [0u8, 1, 2, rng.next(256) as u8]).unwrap();
                    }
                    3 if cfg!(unix) => {
                        fs::create_dir_all(path.parent().unwrap()).unwrap();
                        let _ = fs::remove_file(&path);
                        #[cfg(unix)]
                        std::os::unix::fs::symlink(format!("t{}", rng.next(3)), &path).unwrap();
                    }
                    _ => {
                        fs::create_dir_all(path.parent().unwrap()).unwrap();
                        let _ = fs::remove_file(&path);
                        let body: String = (0..rng.next(8)).map(|_| format!("line{}\n", rng.next(4))).collect();
                        fs::write(&path, body).unwrap();
                    }
                }
            }
            check(&r, &commits, &mut rng, &mut seen);
            match rng.next(4) {
                0 => {
                    // stage everything touched, keep going without committing
                    r.add(&["".into()]).unwrap();
                }
                1 | 2 => {
                    r.add(&["".into()]).unwrap();
                    t += 1;
                    if r.commit(&format!("c{step}"), sig(t)).is_ok() {
                        commits.push(r.head_commit("root").unwrap().unwrap().to_hex());
                        commits.push("HEAD".into());
                        commits.push("HEAD~1".into());
                        if let Some(h) = r.head_commit("z").unwrap() {
                            commits.push(h.to_hex());
                        }
                    }
                }
                _ => {}
            }
            check(&r, &commits, &mut rng, &mut seen);
        }
        assert!(seen.iter().all(|n| *n > 50), "the comparisons must be non-trivial: {seen:?}");
    }
}

/// More than one `AGI3` block (2048 entries each): block selection for path limits, the borrowed
/// scan, and the cache-tree shortcut must agree with the reference.
#[test]
fn fast_diffs_equal_the_reference_across_index_blocks() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let w = r.worktree().to_path_buf();
    for dir in 0..50 {
        fs::create_dir_all(w.join(format!("d{dir:02}"))).unwrap();
        for f in 0..100 {
            fs::write(w.join(format!("d{dir:02}/f{f:03}")), format!("file {dir} {f}\nline\n")).unwrap();
        }
    }
    r.add(&["".into()]).unwrap();
    r.commit("c0", sig(1)).unwrap();
    let mut rng = Rng(0xdead_beef_1234_5677);
    let mut commits = vec!["HEAD".to_owned()];
    let onlies: Vec<Vec<String>> = vec![
        vec![],
        vec!["d10".into()],
        vec!["d1".into()],
        vec!["d49".into(), "d00/f005".into()],
        vec!["d20/f050".into()],
        vec!["d07".into(), "d33".into(), "zzz".into()],
    ];
    let mut seen = [0usize; 4];
    for round in 0..4 {
        for _ in 0..40 {
            let p = w.join(format!("d{:02}/f{:03}", rng.next(50), rng.next(100)));
            match rng.next(6) {
                0 => {
                    let _ = fs::remove_file(&p);
                }
                1 => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if p.exists() {
                            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
                        }
                    }
                }
                _ => fs::write(&p, format!("round {round} {}\n", rng.next(1000))).unwrap(),
            }
        }
        fs::write(w.join("d10/new.txt"), "new\n").unwrap();
        for _ in 0..2 {
            for only in &onlies {
                for staged in [false, true] {
                    let want = limit(r.diff_pairs_full(staged).unwrap(), only);
                    seen[staged as usize] += want.len();
                    assert_eq!(r.diff_pairs_limited(staged, only).unwrap(), want, "staged={staged} only={only:?}");
                }
                for rev in &commits {
                    let want = limit(ref_rev_worktree(&r, rev), only);
                    seen[2] += want.len();
                    assert_eq!(r.rev_diff_pairs(rev, false, only).unwrap(), want, "{rev} only={only:?}");
                    assert_eq!(r.rev_diff_pairs(rev, true, only).unwrap(), limit(ref_rev_index(&r, rev), only));
                    let want = limit(ref_pairs(&r, &r.rev_files(rev).unwrap(), &r.rev_files("HEAD").unwrap()), only);
                    seen[3] += want.len();
                    assert_eq!(r.rev_rev_pairs(rev, "HEAD", only).unwrap(), want);
                }
            }
            assert_eq!(r.diff(false).unwrap(), r.diff_full(false).unwrap());
            // second pass with everything staged, then committed on the last pass
            r.add(&["".into()]).unwrap();
        }
        if r.commit(&format!("c{}", round + 1), sig(10 + round)).is_ok() {
            commits.push(r.head_commit("root").unwrap().unwrap().to_hex());
        }
        // clean again: the cache-tree shortcut says "nothing staged"
        assert!(r.diff_pairs_limited(true, &[]).unwrap().is_empty());
    }
    assert!(seen.iter().all(|n| *n > 100), "{seen:?}");
}

/// `commit_changes` lists what `commit_pairs_names` lists, and `show` prints what a full flatten of both trees
/// printed, including a file that becomes a directory and back.
#[test]
fn commit_listings_and_show_equal_the_reference() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let w = r.worktree().to_path_buf();
    let step = |t: u64, f: &dyn Fn()| {
        f();
        r.add(&["".into()]).unwrap();
        r.commit(&format!("c{t}"), sig(t)).unwrap();
        r.head_commit("root").unwrap().unwrap()
    };
    let mut ids = vec![];
    ids.push(step(1, &|| {
        fs::write(w.join("x"), "file\n").unwrap();
        fs::create_dir_all(w.join("m/n")).unwrap();
        fs::write(w.join("m/n/a"), "a\n").unwrap();
        fs::write(w.join("m.txt"), "t\n").unwrap();
    }));
    ids.push(step(2, &|| {
        fs::remove_file(w.join("x")).unwrap();
        fs::create_dir_all(w.join("x")).unwrap();
        fs::write(w.join("x/y"), "y\n").unwrap();
        fs::write(w.join("m/n/a"), "a2\n").unwrap();
    }));
    ids.push(step(3, &|| {
        fs::remove_dir_all(w.join("x")).unwrap();
        fs::write(w.join("x"), "file again\n").unwrap();
        fs::remove_dir_all(w.join("m")).unwrap();
        fs::write(w.join("m"), "m is a file\n").unwrap();
    }));
    for id in &ids {
        let got: Vec<(String, bool, bool)> = r
            .commit_changes("root", id)
            .unwrap()
            .into_iter()
            .map(|(p, l)| (p, l != b'A', l != b'D'))
            .collect();
        let want: Vec<(String, bool, bool)> = r
            .commit_pairs_names("root", id)
            .unwrap()
            .into_iter()
            .map(|p| (p.path, p.old.is_some(), p.new.is_some()))
            .collect();
        assert_eq!(got, want, "{id}");
        // show: the flatten-based text
        let c = r.commit_of(id).unwrap();
        let flat = |root: Option<vcs_objects::ObjectId>| vcs_repo::treeops::flatten(r.store(), root, "").unwrap();
        let old = flat(c.parents.first().map(|p| r.commit_of(p).unwrap().root));
        let new = flat(Some(c.root));
        let mut paths: BTreeSet<&String> = old.keys().collect();
        paths.extend(new.keys());
        let mut text = String::new();
        for p in paths {
            let (o, n) = (old.get(p), new.get(p));
            if o == n {
                continue;
            }
            let ob = o.map(|(_, i)| r.content(i).unwrap());
            let nb = n.map(|(_, i)| r.content(i).unwrap());
            let dd = vcs_repo::diff::unified(p, ob.as_deref(), nb.as_deref());
            if dd.is_empty() {
                text.push_str(&format!("diff --vcs a/{p} b/{p}\nmode changed\n"));
            } else {
                text.push_str(&dd);
            }
        }
        assert_eq!(r.show("root", &id.to_hex()).unwrap().diff, text);
    }
}
