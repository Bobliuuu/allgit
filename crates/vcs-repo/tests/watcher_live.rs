//! The real watcher daemon (inotify) with the stored-answer fast path: edits made while it runs are
//! seen, a dead or stopped daemon means a full scan, and nothing stale is served.
#![cfg(all(feature = "watcher", target_os = "linux"))]

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use vcs_repo::{Repo, StatusOpts};

fn sig(t: u64) -> vcs_objects::Signature {
    vcs_objects::Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: t,
    }
}

fn unstaged(r: &Repo) -> Vec<String> {
    let mut v: Vec<String> = r.status_auto(StatusOpts::default()).unwrap().zones[0]
        .unstaged
        .iter()
        .map(|c| c.path.clone())
        .collect();
    v.sort();
    v
}

fn full(r: &Repo) -> Vec<String> {
    let mut v: Vec<String> = r.status_full().unwrap().zones[0].unstaged.iter().map(|c| c.path.clone()).collect();
    v.sort();
    v
}

#[test]
fn edits_during_daemon_dead_daemon_and_stop() {
    let d = tempfile::tempdir().unwrap();
    let mut r = Repo::init(&d.path().join("r")).unwrap();
    r.set_index_racy_ns(30_000_000);
    for i in 0..20 {
        fs::create_dir_all(r.worktree().join(format!("d{}", i % 4))).unwrap();
        fs::write(r.worktree().join(format!("d{}/f{i}.txt", i % 4)), format!("file {i}")).unwrap();
    }
    r.add(&[String::new()]).unwrap();
    r.commit("base", sig(1)).unwrap();
    std::thread::sleep(Duration::from_millis(100));

    let stop = Arc::new(AtomicBool::new(false));
    let (s2, root) = (stop.clone(), r.worktree().to_path_buf());
    let th = std::thread::spawn(move || {
        let r = Repo::discover(&root).unwrap();
        vcs_repo::watcher::run(&r, &s2)
    });
    let jr = r.worktree().join(".vcs/fsmon/journal");
    for _ in 0..200 {
        if jr.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(jr.exists(), "daemon did not start");
    std::thread::sleep(Duration::from_millis(100));

    assert!(unstaged(&r).is_empty()); // cold: full scan, state saved
    std::thread::sleep(Duration::from_millis(100));
    assert!(unstaged(&r).is_empty()); // stores the answer
    assert!(unstaged(&r).is_empty()); // served from it
                                      // Edit while the daemon runs (different size, then same size within the same mtime tick).
    fs::write(r.worktree().join("d1/f1.txt"), "edited to another length").unwrap();
    assert_eq!(unstaged(&r), vec!["d1/f1.txt".to_string()]);
    assert_eq!(unstaged(&r), full(&r));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(unstaged(&r), vec!["d1/f1.txt".to_string()]);
    assert_eq!(unstaged(&r), vec!["d1/f1.txt".to_string()]); // from the stored answer
    fs::write(r.worktree().join("d1/f1.txt"), "file 1").unwrap(); // back to the committed bytes
    assert_eq!(unstaged(&r), full(&r));
    assert!(unstaged(&r).is_empty());
    // New edit, then the daemon is made to look dead: the edit must still be found.
    std::thread::sleep(Duration::from_millis(100));
    assert!(unstaged(&r).is_empty());
    fs::write(r.worktree().join("d2/f2.txt"), "dead daemon edit!").unwrap();
    fs::write(r.worktree().join(".vcs/fsmon/pid"), "999999999").unwrap();
    assert_eq!(unstaged(&r), vec!["d2/f2.txt".to_string()]);
    assert_eq!(unstaged(&r), full(&r));
    // Stop the daemon (journal removed): edits are still found.
    stop.store(true, Ordering::SeqCst);
    th.join().unwrap().unwrap();
    fs::write(r.worktree().join("d3/f3.txt"), "after stop, longer").unwrap();
    assert_eq!(unstaged(&r), full(&r));
    assert!(unstaged(&r).contains(&"d3/f3.txt".to_string()));
}
