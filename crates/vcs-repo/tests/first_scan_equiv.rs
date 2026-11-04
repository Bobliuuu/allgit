//! The first status after a big checkout fans the per-file `lstat` and content check out over a
//! worker pool. Its result must equal the single-threaded scan (`VCS_STATUS_THREADS=1`) exactly:
//! same changes in the same order, whatever the scheduling. One test in this file because it sets
//! a process-wide variable.
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

#[test]
fn parallel_first_scan_equals_the_single_threaded_scan() {
    let d = tempfile::tempdir().unwrap();
    let src = Repo::init(&d.path().join("src")).unwrap();
    let root = src.worktree().to_path_buf();
    let n = 6000;
    for i in 0..n {
        let p = root.join(format!("d{}/s{}/f{i}.txt", i % 40, i % 7));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, format!("content {i}\n").repeat(1 + i % 5)).unwrap();
    }
    src.add(&["".into()]).unwrap();
    src.commit("all", sig()).unwrap();
    // a clone-like first state: every entry freshly written (racy), then a mix of edits
    for i in (0..n).step_by(97) {
        fs::write(root.join(format!("d{}/s{}/f{i}.txt", i % 40, i % 7)), "changed\n").unwrap();
        // size change
    }
    for i in (5..n).step_by(131) {
        let p = root.join(format!("d{}/s{}/f{i}.txt", i % 40, i % 7));
        let old = fs::read(&p).unwrap();
        let mut new = old.clone();
        new[0] ^= 1; // same size, same second: only the content check can see it
        fs::write(p, new).unwrap();
    }
    for i in (7..n).step_by(211) {
        fs::remove_file(root.join(format!("d{}/s{}/f{i}.txt", i % 40, i % 7))).unwrap();
    }
    fs::write(root.join("d1/untracked.txt"), "u").unwrap();

    std::env::set_var("VCS_STATUS_THREADS", "1");
    let one = format!("{:?}", Repo::discover(&root).unwrap().status().unwrap());
    std::env::set_var("VCS_STATUS_THREADS", "8");
    let eight = format!("{:?}", Repo::discover(&root).unwrap().status().unwrap());
    std::env::remove_var("VCS_STATUS_THREADS");
    let dflt = format!("{:?}", Repo::discover(&root).unwrap().status().unwrap());
    assert_eq!(one, eight);
    assert_eq!(one, dflt);
    let st = Repo::discover(&root).unwrap().status().unwrap();
    let ch: usize = st.zones.iter().map(|z| z.unstaged.len()).sum();
    let want = (0..n).step_by(97).count() + (5..n).step_by(131).count() + (7..n).step_by(211).count();
    assert!(ch >= want - 40 && ch <= want, "{ch} vs {want}"); // overlaps between the three sets are possible
    assert_eq!(st.untracked, vec!["d1/untracked.txt".to_string()]);
}
