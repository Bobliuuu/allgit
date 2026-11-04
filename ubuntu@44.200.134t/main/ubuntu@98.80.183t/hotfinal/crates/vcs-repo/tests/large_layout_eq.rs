//! The same bytes can have two ids (v3 manifest, kind 9 manifest). Merge, status and diff must see equal content as
//! equal. One test only: it flips a process-wide environment variable.
use std::fs;
use vcs_objects::Signature;
use vcs_repo::{MergeOutcome, Repo, ResetMode};

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}
fn rnd(n: usize, mut x: u64) -> Vec<u8> {
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}
fn commit_all(r: &Repo, m: &str) {
    r.add(&["".into()]).unwrap();
    r.commit(m, sig()).unwrap();
}

#[test]
fn byte_identical_content_in_two_layouts_is_equal_for_merge_status_diff_restore_and_reset() {
    let d = tempfile::tempdir().unwrap();
    let data = rnd(20 << 20, 21);
    let mut v1 = data.clone();
    v1[7 << 20] ^= 1;
    let r = Repo::init(&d.path().join("a")).unwrap();
    let big = r.worktree().join("big.bin");
    fs::write(&big, &v1).unwrap();
    commit_all(&r, "v1");
    let base = r.head_commit("root").unwrap().unwrap();
    r.create_branch("root", "other").unwrap();
    // main: the new bytes stored as a v3 manifest
    std::env::set_var("VCS_CHUNK_LAYOUT", "v3");
    fs::write(&big, &data).unwrap();
    commit_all(&r, "main v3");
    std::env::remove_var("VCS_CHUNK_LAYOUT");
    let main_head = r.head_commit("root").unwrap().unwrap();
    // other: the same new bytes stored as a kind 9 manifest
    r.checkout("root", "other").unwrap();
    fs::write(&big, &data).unwrap();
    commit_all(&r, "other kind9");
    let other_head = r.head_commit("root").unwrap().unwrap();
    assert_ne!(main_head, other_head);

    // status: HEAD (kind 9) vs an index holding the v3 id of the same bytes is not a staged change
    r.checkout("root", "main").unwrap();
    r.reset("root", &other_head, ResetMode::Soft).unwrap();
    let st = r.status().unwrap();
    assert!(st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()), "{st:?}");
    assert!(r.diff(true).unwrap().is_empty());
    assert!(r.diff_pairs(true).unwrap().is_empty());
    assert!(r.diff(false).unwrap().is_empty());
    r.reset("root", &main_head, ResetMode::Soft).unwrap();

    // merge: both sides changed the file to the same bytes, only the layout differs: no conflict
    let out = r.merge("root", "other", sig()).unwrap();
    assert!(
        matches!(
            out,
            MergeOutcome::Merged { .. } | MergeOutcome::FastForward { .. } | MergeOutcome::UpToDate
        ),
        "{out:?}"
    );
    assert_eq!(fs::read(&big).unwrap(), data);

    // restore and hard reset write the file back through the streaming path
    fs::write(&big, b"scribble").unwrap();
    assert_eq!(r.restore(&["big.bin".into()], false).unwrap().restored, 1);
    assert_eq!(fs::read(&big).unwrap(), data);
    fs::write(&big, b"scribble").unwrap();
    let head = r.head_commit("root").unwrap().unwrap();
    r.reset("root", &head, ResetMode::Hard).unwrap();
    assert_eq!(fs::read(&big).unwrap(), data);
    r.reset("root", &base, ResetMode::Hard).unwrap();
    assert_eq!(fs::read(&big).unwrap(), v1);
    let st = r.status().unwrap();
    assert!(st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()), "{st:?}");
    assert!(r.fsck().unwrap().problems.is_empty());
}
