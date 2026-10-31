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

fn cg_path(r: &Repo) -> std::path::PathBuf {
    r.worktree().join(".vcs/commit-graph")
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
    assert!(!cg_path(&r).exists());
    refresh(&r);
    assert!(cg_path(&r).exists());
    assert!(r.fsck().unwrap().problems.is_empty());
}

#[test]
fn disabled_graph_writes_nothing() {
    let (_d, r) = linear(5);
    let o = without_graph(&r);
    let h = heads(&o)[0];
    o.merge_bases(&h, &h).unwrap();
    o.is_ancestor(h, h).unwrap();
    assert!(!cg_path(&r).exists());
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
