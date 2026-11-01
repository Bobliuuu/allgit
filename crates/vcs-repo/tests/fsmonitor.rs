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

fn apply(r: &Repo, o: &Op, step: u64) -> Vec<String> {
    let root = r.worktree();
    let mut touched: Vec<String> = Vec::new();
    match o {
        Op::Create(d, n, c) => {
            let p = path(*d, *n);
            if !root.join(&p).exists() {
                w(r, &p, &format!("c{c}"));
                touched.push(p);
            }
        }
        Op::Edit(d, n, c) => {
            let p = path(*d, *n);
            if root.join(&p).is_file() {
                fs::write(root.join(&p), format!("e{c}{step}")).unwrap();
                touched.push(p);
            }
        }
        Op::Remove(d, n) => {
            let p = root.join(path(*d, *n));
            if p.is_file() {
                fs::remove_file(p).unwrap();
                touched.push(path(*d, *n));
            } else if p.is_dir() {
                let _ = fs::remove_dir_all(p);
                touched.push(path(*d, *n));
            }
        }
        Op::Rename(d, n, d2, n2) => {
            let (a, b) = (root.join(path(*d, *n)), root.join(path(*d2, *n2)));
            if a.exists() && !b.exists() && !b.starts_with(&a) {
                let _ = fs::rename(a, b);
                touched.push(path(*d, *n));
                touched.push(path(*d2, *n2));
            }
        }
        Op::Mkdir(d, n) => {
            let p = root.join(path(*d, *n));
            if !p.exists() {
                fs::create_dir_all(&p).unwrap();
                fs::write(p.join("inner"), "i").unwrap();
                touched.push(path(*d, *n));
                touched.push(format!("{}/inner", path(*d, *n)));
            }
        }
        Op::Swap(d, n) => {
            let p = root.join(path(*d, *n));
            if p.is_file() {
                fs::remove_file(&p).unwrap();
                fs::create_dir(&p).unwrap();
                fs::write(p.join("sub"), "s").unwrap();
                touched.push(path(*d, *n));
            } else if p.is_dir() {
                fs::remove_dir_all(&p).unwrap();
                fs::write(&p, "back").unwrap();
                touched.push(path(*d, *n));
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
            touched.push(path(*d, *n));
        }
        Op::OldMtime(d, n, c) => {
            let p = root.join(path(*d, *n));
            if p.is_file() {
                let len = fs::metadata(&p).unwrap().len() as usize;
                fs::write(&p, vec![b'a' + (c % 26); len.max(1)]).unwrap();
                set_mtime(&p, SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000));
                touched.push(path(*d, *n));
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
    touched
}

/// How the monitor misbehaves for one operation.
#[derive(Clone, Copy, Debug)]
enum Fault {
    None,
    /// Every event twice, in reverse order.
    Duplicate,
    /// An unrelated (maybe nonexistent) path is reported too.
    Spurious(usize, usize),
    /// Events were lost, and the monitor says so.
    Overflow,
    /// Backend restarted: old tokens are invalid.
    Restart,
    /// Daemon unreachable for one query.
    Dead,
}

fn fault() -> impl Strategy<Value = Fault> {
    prop_oneof![
        4 => Just(Fault::None),
        2 => Just(Fault::Duplicate),
        2 => (0..DIRS.len(), 0..NAMES.len()).prop_map(|(a, b)| Fault::Spurious(a, b)),
        1 => Just(Fault::Overflow),
        1 => Just(Fault::Restart),
        1 => Just(Fault::Dead),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, .. ProptestConfig::default() })]
    #[test]
    fn monitored_status_equals_full_status(steps in proptest::collection::vec((op(), fault()), 1..16)) {
        let (_d, r) = base();
        let m = MockMonitor::new();
        assert_same(&r, &m, "setup"); // first query: full scan, state saved
        for (i, (o, f)) in steps.iter().enumerate() {
            let mut ev = apply(&r, o, i as u64);
            match f {
                Fault::None => {}
                Fault::Duplicate => {
                    let mut again = ev.clone();
                    again.reverse();
                    ev.extend(again);
                }
                Fault::Spurious(d, n) => ev.push(path(*d, *n)),
                Fault::Overflow => ev.clear(),
                Fault::Restart => ev.clear(),
                Fault::Dead => {}
            }
            for e in &ev {
                m.event(e);
            }
            match f {
                Fault::Overflow => m.overflow(),
                Fault::Restart => m.restart(),
                Fault::Dead => m.set_dead(true),
                _ => {}
            }
            if matches!(f, Fault::Dead) {
                assert_same(&r, &m, &format!("{o:?} {f:?} (dead)"));
                m.set_dead(false);
            }
            assert_same(&r, &m, &format!("{o:?} {f:?} (1)"));
            settle();
            assert_same(&r, &m, &format!("{o:?} {f:?} (2)"));
            assert_same(&r, &m, &format!("{o:?} {f:?} (3)"));
        }
    }
}

#[test]
fn clean_tree_trusts_the_monitor_and_events_reveal_changes() {
    let (_d, r) = base();
    let m = MockMonitor::new();
    assert_same(&r, &m, "cold"); // saves state
    assert_same(&r, &m, "warm"); // incremental, nothing changed
                                 // A change the monitor does not report is not looked for: that is the whole point (the
                                 // daemon, not status, vouches for the tree).
    w(&r, "a/x.txt", "silently edited, different size");
    let s = r.status_monitored(opts(), &m).unwrap();
    assert!(
        s.zones[0].unstaged.is_empty(),
        "monitor said nothing changed: {:?}",
        s.zones[0].unstaged
    );
    // Once it is reported, the file shows up, and the full scan agrees.
    m.event("a/x.txt");
    assert_same(&r, &m, "reported edit");
    assert_eq!(r.status_monitored(opts(), &m).unwrap().zones[0].unstaged.len(), 1);
    // A new untracked file needs the walk; reported -> seen.
    w(&r, "a/b/new.txt", "n");
    m.event("a/b/new.txt");
    let s = r.status_monitored(opts(), &m).unwrap();
    assert!(s.untracked.contains(&"a/b/new.txt".to_string()));
    assert_same(&r, &m, "after new file");
    // Overflow forces the full scan, which finds an unreported change.
    w(&r, "d/e/f.txt", "unreported");
    m.overflow();
    assert_same(&r, &m, "after overflow");
    // Reverting a file back to its committed content clears it once reported.
    w(&r, "a/x.txt", "a/x.txt");
    m.event("a/x.txt");
    assert_same(&r, &m, "revert");
}

/// Regression for the flake seen under load (shrunk case `Swap(2,3)`, `Remove(2,3)`): a tracked
/// name that was a directory holding untracked files is removed; the saved untracked list must not
/// keep the names that vanished with it. Deterministic: no timing involved.
#[test]
fn removed_directory_over_tracked_name_drops_its_untracked_names() {
    let (_d, r) = base();
    let m = MockMonitor::new();
    assert_same(&r, &m, "cold");
    let p = r.worktree().join("a/b/y.txt");
    fs::remove_file(&p).unwrap();
    fs::create_dir(&p).unwrap();
    fs::write(p.join("sub"), "s").unwrap();
    m.event("a/b/y.txt");
    assert_same(&r, &m, "file replaced by dir");
    assert!(r.status_monitored(opts(), &m).unwrap().untracked.contains(&"a/b/y.txt/sub".to_string()));
    fs::remove_dir_all(&p).unwrap();
    m.event("a/b/y.txt");
    assert_same(&r, &m, "dir removed");
    assert!(!r.status_monitored(opts(), &m).unwrap().untracked.iter().any(|u| u.starts_with("a/b/y.txt/")));
}

#[test]
fn state_for_another_index_is_not_trusted() {
    let (_d, r) = base();
    let m = MockMonitor::new();
    assert_same(&r, &m, "cold");
    w(&r, "a/x.txt", "staged edit");
    r.add(&["a/x.txt".to_string()]).unwrap(); // index changed, monitor saw nothing
    w(&r, "top.txt", "unreported edit");
    assert_same(&r, &m, "index changed");
}

/// The real thing: `vcs watch`'s notify daemon (inotify on Linux) feeding the journal monitor.
#[cfg(feature = "watcher")]
mod real {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use vcs_repo::fsmonitor::JournalMonitor;

    struct Daemon {
        stop: Arc<AtomicBool>,
        h: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Daemon {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(h) = self.h.take() {
                let _ = h.join();
            }
        }
    }

    fn start(r: &Repo) -> (Daemon, JournalMonitor) {
        let root = r.worktree().to_path_buf();
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let h = std::thread::spawn(move || {
            let r = Repo::discover(&root).unwrap();
            vcs_repo::watcher::run(&r, &s2).unwrap();
        });
        let vcs = r.worktree().join(".vcs");
        for _ in 0..500 {
            if let Some(m) = JournalMonitor::open(&vcs) {
                return (Daemon { stop, h: Some(h) }, m);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("watcher did not start");
    }

    fn same(r: &Repo, m: &JournalMonitor, ctx: &str) {
        let mon = summary(&r.status_monitored(opts(), m).unwrap());
        let full = summary(&r.status_full().unwrap());
        assert_eq!(mon, full, "watcher-assisted != full after {ctx}");
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 12, .. ProptestConfig::default() })]
        #[test]
        fn watcher_status_equals_full_status(ops in proptest::collection::vec(op(), 1..14)) {
            let (_d, r) = base();
            let (_dm, m) = start(&r);
            same(&r, &m, "setup");
            for (i, o) in ops.iter().enumerate() {
                apply(&r, o, i as u64);
                same(&r, &m, &format!("{o:?} (1)"));
                settle();
                same(&r, &m, &format!("{o:?} (2)"));
                same(&r, &m, &format!("{o:?} (3)"));
            }
        }
    }

    #[test]
    fn watcher_skips_unchanged_files_and_sees_edits_and_new_dirs() {
        let (_d, r) = base();
        let (_dm, m) = start(&r);
        same(&r, &m, "cold");
        same(&r, &m, "warm");
        // new directory with files created in it right away (the inotify new-directory race)
        for i in 0..20 {
            w(&r, &format!("fresh/sub{i}/f.txt"), "x");
        }
        let s = r.status_monitored(opts(), &m).unwrap();
        assert!(s.untracked.contains(&"fresh/".to_string()), "{:?}", s.untracked);
        // rename of a tracked directory
        fs::rename(r.worktree().join("a/b"), r.worktree().join("a/c")).unwrap();
        same(&r, &m, "dir rename");
        w(&r, "a/c/y.txt", "edited after the rename");
        same(&r, &m, "edit under renamed dir");
        // touch -r style restore of mtime after an edit
        let p = r.worktree().join("top.txt");
        let old = fs::metadata(&p).unwrap().modified().unwrap();
        fs::write(&p, "TOP.TXT").unwrap(); // same size as the original "top.txt"
        set_mtime(&p, old);
        same(&r, &m, "mtime restored");
        // an ignore file changes what is untracked
        w(&r, ".vcsignore", "fresh/\n");
        same(&r, &m, "ignore file");
    }

    #[test]
    fn dead_daemon_falls_back_to_full_scan() {
        let (_d, r) = base();
        let (dm, m) = start(&r);
        same(&r, &m, "cold");
        drop(dm); // removes pid and journal
        w(&r, "a/x.txt", "changed while nobody watched");
        // a leftover journal with no daemon must not be trusted either
        fs::create_dir_all(r.worktree().join(".vcs/fsmon")).unwrap();
        fs::write(r.worktree().join(".vcs/fsmon/journal"), "E stale\n").unwrap();
        fs::write(r.worktree().join(".vcs/fsmon/pid"), "999999999").unwrap();
        let m = JournalMonitor::open(&r.worktree().join(".vcs")).unwrap();
        same(&r, &m, "stale journal");
        assert_eq!(r.status_monitored(opts(), &m).unwrap().zones[0].unstaged.len(), 1);
    }

    /// FSEvents reports real paths (`/private/var/...` for a `/var/...` temp dir), coalesces
    /// events, and sees a case-only rename as a rename: none of that may hide a change.
    #[cfg(target_os = "macos")]
    #[test]
    fn fsevents_sees_case_renames_atomic_replaces_and_unicode_names() {
        let (d, r) = base();
        assert!(d.path().starts_with("/var") || d.path().starts_with("/private"), "{:?}", d.path());
        let (_dm, m) = start(&r);
        same(&r, &m, "cold");
        same(&r, &m, "warm");
        fs::rename(r.worktree().join("top.txt"), r.worktree().join("TOP.txt")).unwrap();
        same(&r, &m, "case-only rename");
        fs::rename(r.worktree().join("TOP.txt"), r.worktree().join("top.txt")).unwrap();
        same(&r, &m, "renamed back");
        // editor-style atomic save: write a temp file, rename it over the tracked file
        fs::write(r.worktree().join("a/x.tmp"), "saved atomically").unwrap();
        fs::rename(r.worktree().join("a/x.tmp"), r.worktree().join("a/x.txt")).unwrap();
        same(&r, &m, "atomic replace");
        // a decomposed (NFD) name; APFS keeps the spelling it was given
        w(&r, "cafe\u{301}/in.txt", "x");
        same(&r, &m, "nfd directory");
        // many files at once (event coalescing)
        for i in 0..200 {
            w(&r, &format!("burst/f{i}.txt"), "x");
        }
        fs::remove_file(r.worktree().join("burst/f7.txt")).unwrap();
        same(&r, &m, "burst");
        settle();
        same(&r, &m, "burst settled");
    }
}
