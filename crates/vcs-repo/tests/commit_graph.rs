//! The commit-graph file is a pure cache: results must be identical with and without it.
use proptest::prelude::*;
use std::fs;
use vcs_objects::{ObjectId, Signature};
use vcs_repo::{Repo, ROOT_ZONE};

fn sig(ts: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: ts,
    }
}

fn commit_file(r: &Repo, name: &str, ts: u64) {
    fs::write(r.worktree().join(name), name).unwrap();
    r.add(&[name.to_string()]).unwrap();
    r.commit(name, sig(ts)).unwrap();
}

fn linear(n: usize) -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    for i in 0..n {
        commit_file(&r, &format!("f{i}"), 100 + i as u64);
    }
    (d, r)
}

fn heads(r: &Repo) -> Vec<ObjectId> {
    r.branches(ROOT_ZONE).unwrap().into_iter().filter_map(|(_, h, _)| h).collect()
}

/// Any merge-base query builds or refreshes the graph file.
fn refresh(r: &Repo) {
    let h = heads(r)[0];
    r.merge_bases(&h, &h).unwrap();
}

fn cg_dir(r: &Repo) -> std::path::PathBuf {
    r.worktree().join(".vcs/commit-graphs")
}

/// Graph files present (temp files of crashed writes excluded), sorted.
fn cg_files(r: &Repo) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = fs::read_dir(cg_dir(r))
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| !p.file_name().unwrap().to_string_lossy().contains('.')).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// The graph file of a repository that has a single zone.
fn cg_path(r: &Repo) -> std::path::PathBuf {
    let f = cg_files(r);
    assert!(f.len() <= 1, "{f:?}");
    f.into_iter().next().unwrap_or_else(|| cg_dir(r).join("none"))
}

/// Everything the graph can influence, computed through `r`.
fn observe(r: &Repo) -> String {
    let hs = heads(r);
    let mut out = String::new();
    for (b, _, _) in r.branches(ROOT_ZONE).unwrap() {
        let l: Vec<String> = r
            .log(ROOT_ZONE, Some(&b), usize::MAX)
            .unwrap()
            .iter()
            .map(|(i, _)| i.to_hex())
            .collect();
        out += &format!("log {b}: {l:?}\n");
        let l3: Vec<String> = r.log(ROOT_ZONE, Some(&b), 3).unwrap().iter().map(|(i, _)| i.to_hex()).collect();
        out += &format!("log3 {b}: {l3:?}\n");
    }
    for a in &hs {
        for b in &hs {
            out += &format!("mb {:?} ", r.merge_bases(a, b).unwrap());
            out += &format!("anc {}\n", r.is_ancestor(*a, *b).unwrap());
        }
    }
    out
}

fn without_graph(r: &Repo) -> Repo {
    let mut o = Repo::discover(r.worktree()).unwrap();
    o.set_commit_graph(false);
    o
}

#[test]
fn graph_is_written_and_fsck_clean() {
    let (_d, r) = linear(20);
    assert!(cg_files(&r).is_empty());
    refresh(&r);
    assert_eq!(cg_files(&r).len(), 1);
    assert!(r.fsck().unwrap().problems.is_empty());
}

#[test]
fn disabled_graph_writes_nothing() {
    let (_d, r) = linear(5);
    let o = without_graph(&r);
    let h = heads(&o)[0];
    o.merge_bases(&h, &h).unwrap();
    o.is_ancestor(h, h).unwrap();
    assert!(cg_files(&r).is_empty());
}

#[test]
fn stale_graph_is_extended_not_trusted() {
    let (_d, r) = linear(10);
    refresh(&r);
    let before = fs::read(cg_path(&r)).unwrap();
    commit_file(&r, "new", 500);
    refresh(&r);
    assert_ne!(fs::read(cg_path(&r)).unwrap(), before);
    assert_eq!(observe(&r), observe(&without_graph(&r)));
}

#[test]
fn corrupt_graph_is_ignored_rebuilt_and_flagged_by_fsck() {
    let (_d, r) = linear(15);
    let expect = observe(&without_graph(&r));
    refresh(&r);
    let good = fs::read(cg_path(&r)).unwrap();
    for bad in [b"junk".to_vec(), Vec::new(), good[..good.len() / 2].to_vec(), {
        let mut b = good.clone();
        b[40] ^= 0xff;
        b
    }] {
        fs::write(cg_path(&r), &bad).unwrap();
        assert!(!r.fsck().unwrap().problems.is_empty(), "fsck must flag a corrupt graph");
        assert_eq!(observe(&r), expect);
        assert_eq!(fs::read(cg_path(&r)).unwrap(), good, "rebuilt identically");
        assert!(r.fsck().unwrap().problems.is_empty());
    }
}

#[test]
fn fsck_catches_a_well_formed_graph_that_lies() {
    use vcs_merge::CommitGraphFile;
    let (_d, r) = linear(6);
    refresh(&r);
    // Valid checksum and generations, but another repo's shape: drop a parent link by rebuilding
    // from a graph whose commit 3 claims to be a root.
    let ids: Vec<ObjectId> = r.log(ROOT_ZONE, None, usize::MAX).unwrap().into_iter().map(|(i, _)| i).collect();
    let lie = CommitGraphFile::default()
        .extend(&ids, |id| {
            let (_, c) = r
                .log(ROOT_ZONE, None, usize::MAX)
                .unwrap()
                .into_iter()
                .find(|(i, _)| i == id)
                .unwrap();
            let parents = if c.message == "f3" { vec![] } else { c.parents.clone() };
            Ok((parents, c.author.timestamp))
        })
        .unwrap();
    fs::write(cg_path(&r), lie.encode()).unwrap();
    let p = r.fsck().unwrap().problems;
    assert!(p.iter().any(|m| m.contains("parents differ")), "{p:?}");
}

fn zone_commit(r: &Repo, zone: &str, name: &str, ts: u64) {
    fs::write(r.worktree().join(zone).join(name), name).unwrap();
    r.add(&[format!("{zone}/{name}")]).unwrap();
    r.commit(name, sig(ts)).unwrap();
}

fn zone_head(r: &Repo, zone: &str) -> ObjectId {
    r.branches(zone).unwrap().into_iter().find_map(|(_, h, _)| h).unwrap()
}

fn multi_zone(zones: usize, commits: usize) -> (tempfile::TempDir, Repo, Vec<String>) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let names: Vec<String> = (0..zones).map(|i| format!("z{i}")).collect();
    for z in &names {
        fs::create_dir_all(r.worktree().join(z)).unwrap();
        r.create_zone(z, z).unwrap();
    }
    for i in 0..commits {
        for z in &names {
            zone_commit(&r, z, &format!("f{i}"), 100 + i as u64);
        }
    }
    (d, r, names)
}

fn touch_all(r: &Repo, names: &[String]) {
    for z in names {
        let h = zone_head(r, z);
        r.merge_bases(&h, &h).unwrap();
    }
}

#[test]
fn one_file_per_zone_and_a_commit_rewrites_only_its_zone() {
    let (_d, r, names) = multi_zone(4, 5);
    touch_all(&r, &names);
    let files = cg_files(&r);
    assert_eq!(files.len(), 4);
    let before: Vec<Vec<u8>> = files.iter().map(|f| fs::read(f).unwrap()).collect();
    zone_commit(&r, "z2", "extra", 900);
    touch_all(&r, &names);
    let after: Vec<Vec<u8>> = files.iter().map(|f| fs::read(f).unwrap()).collect();
    let changed = before.iter().zip(&after).filter(|(a, b)| a != b).count();
    assert_eq!(changed, 1, "only the touched zone's file changes");
    assert!(r.fsck().unwrap().problems.is_empty());
    // a query across two zones falls back to walking and still answers (same as no graph)
    let (a, b) = (zone_head(&r, "z0"), zone_head(&r, "z1"));
    assert_eq!(r.merge_bases(&a, &b).unwrap(), without_graph(&r).merge_bases(&a, &b).unwrap());
}

#[test]
fn fsck_flags_a_graph_file_holding_another_zones_commits() {
    let (_d, r, names) = multi_zone(2, 3);
    touch_all(&r, &names);
    let files = cg_files(&r);
    fs::copy(&files[0], &files[1]).unwrap();
    let p = r.fsck().unwrap().problems;
    assert!(p.iter().any(|m| m.contains("another zone")), "{p:?}");
    touch_all(&r, &names); // rebuilt/ignored without error
    let off = without_graph(&r);
    for z in &names {
        let h = zone_head(&r, z);
        assert_eq!(r.merge_bases(&h, &h).unwrap(), off.merge_bases(&h, &h).unwrap());
    }
}

#[test]
fn per_zone_files_are_cache_only() {
    let (_d, r, names) = multi_zone(3, 4);
    let expect: Vec<_> = names.iter().map(|z| (z.clone(), zone_head(&r, z))).collect();
    touch_all(&r, &names);
    for f in cg_files(&r) {
        fs::write(f, b"junk").unwrap();
    }
    assert!(!r.fsck().unwrap().problems.is_empty());
    touch_all(&r, &names);
    assert!(r.fsck().unwrap().problems.is_empty());
    for (z, h) in expect {
        assert_eq!(zone_head(&r, &z), h);
    }
}

#[derive(Clone, Debug)]
enum Op {
    Commit,
    Branch,
    Checkout(usize),
    Merge(usize),
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    proptest::collection::vec(
        prop_oneof![
            4 => Just(Op::Commit),
            2 => Just(Op::Branch),
            2 => (0usize..6).prop_map(Op::Checkout),
            2 => (0usize..6).prop_map(Op::Merge),
        ],
        1..25,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    #[test]
    fn identical_with_and_without_graph(ops in ops(), warm in any::<bool>()) {
        let d = tempfile::tempdir().unwrap();
        let r = Repo::init(&d.path().join("r")).unwrap();
        commit_file(&r, "root", 1);
        let mut n = 0u64;
        for op in ops {
            n += 1;
            let names: Vec<String> = r.branches(ROOT_ZONE).unwrap().into_iter().map(|(b, _, _)| b).collect();
            match op {
                Op::Commit => commit_file(&r, &format!("c{n}"), 10 + (n * 7) % 13),
                Op::Branch => { let _ = r.create_branch(ROOT_ZONE, &format!("b{n}")); }
                Op::Checkout(i) => { let _ = r.checkout(ROOT_ZONE, &names[i % names.len()]); }
                Op::Merge(i) => { let _ = r.merge(ROOT_ZONE, &names[i % names.len()], sig(10 + n)); }
            }
            if warm {
                // exercise the incremental path: graph is stale after nearly every step
                refresh(&r);
            }
        }
        let off = without_graph(&r);
        prop_assert_eq!(observe(&r), observe(&off));
        prop_assert!(r.fsck().unwrap().problems.is_empty());
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    #[test]
    fn identical_with_and_without_per_zone_graphs(picks in proptest::collection::vec((0usize..3, any::<bool>()), 1..30)) {
        let (_d, r, names) = multi_zone(3, 1);
        for (n, (z, warm)) in picks.into_iter().enumerate() {
            zone_commit(&r, &names[z], &format!("p{n}"), 200 + (n as u64 * 5) % 17);
            if warm {
                let h = zone_head(&r, &names[z]);
                r.merge_bases(&h, &h).unwrap();
            }
        }
        let off = without_graph(&r);
        for z in &names {
            let h = zone_head(&r, z);
            let root = r.log(z, None, usize::MAX).unwrap().last().unwrap().0;
            prop_assert_eq!(r.merge_bases(&h, &root).unwrap(), off.merge_bases(&h, &root).unwrap());
            prop_assert_eq!(r.is_ancestor(root, h).unwrap(), off.is_ancestor(root, h).unwrap());
        }
        prop_assert!(r.fsck().unwrap().problems.is_empty());
    }
}
