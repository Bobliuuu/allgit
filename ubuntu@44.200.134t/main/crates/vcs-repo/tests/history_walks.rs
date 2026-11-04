//! History walks (`log_walk`, `PathFilter`, `commit_pairs` via subtree-skipping diff) must give
//! exactly what the whole-tree versions give, on a history with nesting, deletes, renames of
//! directories into files and merges.
use std::collections::BTreeSet;
use std::fs;
use vcs_objects::{Commit, ObjectId, Signature};
use vcs_repo::treeops::{self, Flat};
use vcs_repo::{PathFilter, Repo, ROOT_ZONE};

fn sig(ts: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: ts,
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

/// A repo with `n` main commits over a few nested directories, with side branches merged back.
fn history(n: u64) -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let mut rng = Rng(7);
    let mut ts = 1000;
    let w = r.worktree().to_path_buf();
    let write = |p: &str, v: u64| {
        let f = w.join(p);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, format!("{p} {v}\n")).unwrap();
        r.add(&[p.to_string()]).unwrap();
    };
    let pool = [
        "a/x.txt",
        "a/b/y.txt",
        "a/b/c/z.txt",
        "d/e.txt",
        "d/f/g.txt",
        "top.txt",
        "h/i/j/k.txt",
        "h/l.txt",
    ];
    for p in pool {
        write(p, 0);
    }
    r.commit("init", sig(ts)).unwrap();
    for i in 0..n {
        ts += 1 + rng.next(50);
        if i % 9 == 4 {
            // side branch with its own files, merged back
            let name = format!("s{i}");
            r.create_branch(ROOT_ZONE, &name).unwrap();
            r.checkout(ROOT_ZONE, &name).unwrap();
            for k in 0..2 {
                ts += 1 + rng.next(50);
                write(&format!("side/{name}/f{k}.txt"), i);
                r.commit("side", sig(ts)).unwrap();
            }
            r.checkout(ROOT_ZONE, "main").unwrap();
            ts += 5;
            write(pool[(i % 8) as usize], i + 1000);
            r.commit("main edit", sig(ts)).unwrap();
            ts += 5;
            r.merge(ROOT_ZONE, &name, sig(ts)).unwrap();
            continue;
        }
        for _ in 0..1 + rng.next(2) {
            let p = pool[rng.next(8) as usize];
            if rng.next(10) == 0 && p != "top.txt" && w.join(p).exists() {
                let _ = r.rm(&[p.to_string()], false, true);
            } else {
                write(p, i);
            }
        }
        write("top.txt", i + 5000);
        r.commit(&format!("c{i}"), sig(ts)).unwrap();
    }
    (d, r)
}

fn flat(r: &Repo, c: &Commit) -> Flat {
    treeops::flatten(r.store(), Some(c.root), "").unwrap()
}

fn in_spec(key: &str, specs: &[String]) -> bool {
    specs
        .iter()
        .any(|s| s.is_empty() || key == s || key.strip_prefix(s.as_str()).is_some_and(|r| r.starts_with('/')))
}

/// The reference: filtered whole-tree flatten equal to a parent's means "does not touch".
fn touches_ref(r: &Repo, specs: &[String], c: &Commit) -> bool {
    let filt = |c: &Commit| {
        let mut f = flat(r, c);
        f.retain(|k, _| in_spec(k, specs));
        f
    };
    let mine = filt(c);
    if c.parents.is_empty() {
        return !mine.is_empty();
    }
    c.parents.iter().all(|p| filt(&r.commit_of(p).unwrap()) != mine)
}

#[test]
fn log_walk_is_log() {
    let (_d, r) = history(60);
    let all = r.log(ROOT_ZONE, None, usize::MAX).unwrap();
    let walked: Vec<_> = r.log_walk(ROOT_ZONE, None).unwrap().map(|x| x.unwrap()).collect();
    assert_eq!(
        all.iter().map(|x| x.0).collect::<Vec<_>>(),
        walked.iter().map(|x| x.0).collect::<Vec<_>>()
    );
    assert!(all.len() > 60);
    for lim in [0, 1, 7] {
        assert_eq!(r.log(ROOT_ZONE, None, lim).unwrap().len(), lim);
    }
}

#[test]
fn path_filter_matches_whole_tree_filter() {
    let (_d, r) = history(70);
    let all = r.log(ROOT_ZONE, None, usize::MAX).unwrap();
    let cases: Vec<Vec<&str>> = vec![
        vec![],
        vec![""],
        vec!["a"],
        vec!["a/b"],
        vec!["a/b/y.txt"],
        vec!["a/b/c/z.txt", "d"],
        vec!["d/f/g.txt"],
        vec!["top.txt"],
        vec!["h/i"],
        vec!["h/i/j/k.txt", "h/l.txt"],
        vec!["side"],
        vec!["nothing/here"],
        vec!["top.txt/inside"],
        vec!["a", "a/b/y.txt"],
    ];
    let mut some_true = 0;
    for specs in cases {
        let specs: Vec<String> = specs.into_iter().map(String::from).collect();
        // plain, with the changed-path filters built on the fly, and again with them read back
        for pass in 0..3 {
            let mut pf = PathFilter::new(&r, ROOT_ZONE, &specs)
                .unwrap()
                .use_filters_after(if pass == 0 { usize::MAX } else { 0 });
            for (id, c) in &all {
                let want = !specs.is_empty() && touches_ref(&r, &specs, c);
                let got = specs.is_empty() || pf.touches(id, c).unwrap();
                if !specs.is_empty() {
                    assert_eq!(got, want, "pass {pass} specs {specs:?} commit {}", id.short());
                    some_true += usize::from(got);
                }
            }
        }
    }
    assert!(some_true > 150, "the history must exercise both outcomes");
}

#[test]
fn commit_pairs_match_full_diff() {
    let (_d, r) = history(50);
    for (id, c) in r.log(ROOT_ZONE, None, usize::MAX).unwrap() {
        let got = r.commit_pairs(ROOT_ZONE, &id).unwrap();
        let new = flat(&r, &c);
        let old = c.parents.first().map(|p| flat(&r, &r.commit_of(p).unwrap())).unwrap_or_default();
        let paths: BTreeSet<&String> = old.keys().chain(new.keys()).filter(|p| old.get(*p) != new.get(*p)).collect();
        let names: Vec<&String> = got.iter().map(|p| &p.path).collect();
        assert_eq!(names, paths.into_iter().collect::<Vec<_>>(), "commit {}", id.short());
        for p in &got {
            assert_eq!(p.new_mode, new.get(&p.path).map(|x| x.0));
            assert_eq!(p.old_mode, old.get(&p.path).map(|x| x.0));
        }
    }
}

#[test]
fn entry_at_agrees_with_flatten() {
    let (_d, r) = history(20);
    let (_, c) = r.log(ROOT_ZONE, None, 1).unwrap().remove(0);
    let f = flat(&r, &c);
    for (p, (m, id)) in &f {
        assert_eq!(treeops::entry_at(r.store(), c.root, p).unwrap(), Some((*m, *id)));
    }
    assert_eq!(treeops::entry_at(r.store(), c.root, "").unwrap().map(|e| e.1), Some(c.root));
    assert_eq!(treeops::entry_at(r.store(), c.root, "top.txt/x").unwrap(), None);
    assert_eq!(treeops::entry_at(r.store(), c.root, "zzz").unwrap(), None);
    let _: Option<ObjectId> = None;
}

fn pf_file(r: &Repo) -> std::path::PathBuf {
    let d = r.worktree().join(".vcs/path-filters");
    fs::read_dir(d)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| !p.file_name().unwrap().to_string_lossy().contains('.'))
        .unwrap()
}

#[test]
fn filters_are_a_cache_that_is_persisted_checked_and_rebuilt() {
    let (_d, r) = history(70);
    let all = r.log(ROOT_ZONE, None, usize::MAX).unwrap();
    let specs = vec!["a/b".to_string()];
    let run = |r: &Repo, after: usize| -> Vec<ObjectId> {
        let mut pf = PathFilter::new(r, ROOT_ZONE, &specs).unwrap().use_filters_after(after);
        all.iter().filter(|(id, c)| pf.touches(id, c).unwrap()).map(|x| x.0).collect()
    };
    let plain = run(&r, usize::MAX);
    assert!(!plain.is_empty() && plain.len() < all.len());
    // nothing is written when filters are not used
    assert!(!r.worktree().join(".vcs/path-filters").exists());
    assert_eq!(run(&r, 0), plain);
    let good = fs::read(pf_file(&r)).unwrap();
    assert!(r.fsck().unwrap().problems.is_empty(), "{:?}", r.fsck().unwrap().problems);
    assert_eq!(run(&r, 0), plain, "read back from disk");
    // damaged files are ignored (and fsck says so), the answer does not change
    for bad in [b"junk".to_vec(), Vec::new(), good[..good.len() / 2].to_vec(), {
        let mut b = good.clone();
        let n = b.len() / 2;
        b[n] ^= 0xff;
        b
    }] {
        fs::write(pf_file(&r), &bad).unwrap();
        assert!(!r.fsck().unwrap().problems.is_empty());
        assert_eq!(run(&r, 0), plain);
        assert_eq!(fs::read(pf_file(&r)).unwrap(), good, "rebuilt identically");
    }
    // a file that lies (valid checksum, wrong content) is caught by fsck
    let mut lie = good.clone();
    let n = lie.len() - 33;
    lie[n] ^= 0xff;
    let sum = blake3::hash(&lie[..lie.len() - 32]);
    let l = lie.len();
    lie[l - 32..].copy_from_slice(sum.as_bytes());
    fs::write(pf_file(&r), &lie).unwrap();
    assert!(!r.fsck().unwrap().problems.is_empty());
    // disabling the cache stops use of it
    fs::write(pf_file(&r), &good).unwrap();
    let mut off = Repo::discover(r.worktree()).unwrap();
    off.set_commit_graph(false);
    assert_eq!(run(&off, 0), plain);
}

#[test]
fn log_paths_equals_filtering_the_walk() {
    let (_d, r) = history(80);
    let all = r.log(ROOT_ZONE, None, usize::MAX).unwrap();
    let who = |c: &Commit| c.message.contains('1');
    let mut off = Repo::discover(r.worktree()).unwrap();
    off.set_commit_graph(false);
    for round in 0..3 {
        for specs in [
            vec![],
            vec!["a/b"],
            vec!["d"],
            vec!["h/i/j/k.txt", "top.txt"],
            vec!["side"],
            vec!["nope"],
        ] {
            let specs: Vec<String> = specs.into_iter().map(String::from).collect();
            for limit in [0, 1, 5, 1000] {
                for author in [false, true] {
                    let want: Vec<ObjectId> = all
                        .iter()
                        .filter(|(_, c)| (!author || who(c)) && (specs.is_empty() || touches_ref(&r, &specs, c)))
                        .map(|x| x.0)
                        .take(limit)
                        .collect();
                    let f: Option<&dyn Fn(&Commit) -> bool> = if author { Some(&who) } else { None };
                    for repo in [&r, &off] {
                        let (got, empty) = repo.log_paths(ROOT_ZONE, None, &specs, f, limit).unwrap();
                        assert!(!empty);
                        assert_eq!(
                            got.iter().map(|x| x.0).collect::<Vec<_>>(),
                            want,
                            "round {round} {specs:?} limit {limit} author {author}"
                        );
                    }
                }
            }
        }
        if round == 0 {
            // build and store the changed-path filters, so the next rounds skip commits through them
            for specs in [vec!["a/b"], vec!["d"], vec!["h/i/j/k.txt", "top.txt"], vec!["side"], vec!["nope"]] {
                let specs: Vec<String> = specs.into_iter().map(String::from).collect();
                let mut pf = PathFilter::new(&r, ROOT_ZONE, &specs).unwrap().use_filters_after(0);
                for (id, c) in &all {
                    pf.touches(id, c).unwrap();
                }
            }
            assert!(r.worktree().join(".vcs/path-filters").exists());
        }
    }
    // an empty branch
    let d = tempfile::tempdir().unwrap();
    let e = Repo::init(&d.path().join("e")).unwrap();
    assert!(e.log_paths(ROOT_ZONE, Some("main"), &[], None, 5).is_err() || e.log_paths(ROOT_ZONE, None, &[], None, 5).unwrap().1);
}
