//! Monitor-assisted `status` (#113, #154) must always equal the full scan, including under renames,
//! same-name replacement, timestamp tricks, duplicated and spurious events, and signalled loss
//! (overflow, backend restart, dead daemon). Events come from a mock monitor fed by the test.

use proptest::prelude::*;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};
use vcs_repo::fsmonitor::MockMonitor;
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

fn assert_same(r: &Repo, m: &MockMonitor, ctx: &str) {
    let mon = summary(&r.status_monitored(opts(), m).unwrap());
    let full = summary(&r.status_full().unwrap());
    assert_eq!(mon, full, "monitored != full after {ctx}");
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


fn sg(t: u64) -> vcs_objects::Signature { vcs_objects::Signature { name: "T".into(), email: "t@x".into(), timestamp: t } }
fn commit_all(r: &Repo, m: &str, t: u64) { r.add(&[String::new()]).unwrap(); r.commit(m, sg(t)).unwrap(); }

/// Switch between branches with file<->dir swaps, deleted/added dirs and new files; with NO events from
/// the monitor the monitored status must already equal the full scan (the switch logged what it wrote).
#[test]
fn vfy2_switch_with_shape_changes_monitored_equals_full() {
    let (_d, r) = base();
    r.create_branch("root", "side").unwrap();
    r.checkout("root", "side").unwrap();
    // side: a/b becomes a file, d/ removed, new tree n/p/q, top.txt changed, new d2 file where dir a/b/c was
    fs::remove_dir_all(r.worktree().join("a/b")).unwrap();
    w(&r, "a/b", "now a file");
    fs::remove_dir_all(r.worktree().join("d")).unwrap();
    w(&r, "n/p/q.txt", "q");
    w(&r, "top.txt", "side top");
    commit_all(&r, "side", 2);
    r.checkout("root", "main").unwrap();
    settle();
    let m = MockMonitor::new();
    assert_same(&r, &m, "cold");
    for i in 0..6 {
        let to = if i % 2 == 0 { "side" } else { "main" };
        r.checkout("root", to).unwrap();
        // an untracked file in a directory the switch rewrote must still be found
        let s = r.status_monitored(opts(), &m).unwrap();
        let full = r.status_full().unwrap();
        assert_eq!(summary(&s), summary(&full), "after switch {i} to {to} (no events)");
        // now with all events reported
        for p in ["a/b", "a/b/y.txt", "a/b/c/z.txt", "a/x.txt", "d", "d/e/f.txt", "n", "n/p/q.txt", "top.txt"] { m.event(p); }
        assert_same(&r, &m, &format!("switch {i} with events"));
        // dirty edit blocks the next switch
        w(&r, "a/x.txt", &format!("edit {i}"));
        m.event("a/x.txt");
        assert!(r.checkout("root", if to == "side" { "main" } else { "side" }).is_err());
        w(&r, "a/x.txt", "a/x.txt");
        m.event("a/x.txt");
        assert_same(&r, &m, &format!("restored {i}"));
    }
}

/// Edits made while the switch runs (silent, unreported) to files OUTSIDE the diff are by design not found;
/// but an untracked file placed where the switch will write must make the switch fail, not be overwritten.
#[test]
fn vfy2_switch_does_not_clobber_untracked() {
    let (_d, r) = base();
    r.create_branch("root", "side").unwrap();
    r.checkout("root", "side").unwrap();
    w(&r, "newfile.txt", "tracked on side");
    commit_all(&r, "side", 2);
    r.checkout("root", "main").unwrap();
    settle();
    let m = MockMonitor::new();
    assert_same(&r, &m, "cold");
    w(&r, "newfile.txt", "my untracked work");
    m.event("newfile.txt");
    let res = r.checkout("root", "side");
    assert_eq!(fs::read_to_string(r.worktree().join("newfile.txt")).unwrap(), "my untracked work", "clobbered; switch result: {:?}", res.is_ok());
}
