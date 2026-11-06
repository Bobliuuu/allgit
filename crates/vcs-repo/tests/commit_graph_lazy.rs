//! A commit-graph file that is opened lazily (not verified at open, as every big file is) and is wrong inside must
//! never change an answer: queries fall back to the objects, the bad file is dropped, fsck reports it.
use std::fs;
use vcs_objects::Signature;
use vcs_repo::{Repo, ROOT_ZONE};

fn sig(ts: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: ts,
    }
}

#[test]
fn lazily_opened_corrupt_graph_degrades_to_the_object_walk() {
    // Lazy even for this tiny file; the variable is read at open and this binary has only this test.
    std::env::set_var("VCS_CACHE_EAGER_VERIFY_MAX", "0");
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    for i in 0..12u64 {
        let name = format!("f{i}");
        fs::write(r.worktree().join(&name), &name).unwrap();
        r.add(&[name]).unwrap();
        r.commit("c", sig(100 + i)).unwrap();
    }
    let head = r.branches(ROOT_ZONE).unwrap().into_iter().find_map(|(_, h, _)| h).unwrap();
    let root = r.log(ROOT_ZONE, None, usize::MAX).unwrap().last().unwrap().0;
    let mut off = Repo::discover(r.worktree()).unwrap();
    off.set_commit_graph(false);
    let want = off.merge_bases(&head, &root).unwrap();
    assert_eq!(want, vec![root]);
    assert_eq!(r.merge_bases(&head, &root).unwrap(), want); // builds the graph
    let dir = r.worktree().join(".vcs/commit-graphs");
    let file = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| !p.file_name().unwrap().to_string_lossy().contains('.'))
        .unwrap();
    let good = fs::read(&file).unwrap();
    // set every generation number to 1: valid length and header, wrong inside (the layout is in vcs-merge's cgraph.rs)
    let n = u32::from_le_bytes(good[8..12].try_into().unwrap()) as usize;
    let o_gen = 32 + 40 * n;
    let mut bad = good.clone();
    for i in 0..n {
        bad[o_gen + 4 * i..o_gen + 4 * i + 4].copy_from_slice(&1u32.to_le_bytes());
    }
    fs::write(&file, &bad).unwrap();
    assert!(r
        .fsck()
        .unwrap()
        .problems
        .iter()
        .any(|p| p.contains("commit-graph") || p.contains("generation")));
    assert_eq!(r.merge_bases(&head, &root).unwrap(), want, "answer unchanged");
    assert!(!file.exists(), "the self-contradicting file is dropped");
    assert_eq!(r.merge_bases(&head, &root).unwrap(), want);
    assert!(file.exists(), "and rebuilt on the next use");
    assert_eq!(fs::read(&file).unwrap(), good);
    assert!(r.fsck().unwrap().problems.is_empty());
}
