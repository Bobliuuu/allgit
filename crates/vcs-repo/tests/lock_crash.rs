//! kill -9 while holding `.vcs/lock`: the next process proceeds, nothing is lost, fsck is clean.
#![cfg(unix)]
use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use vcs_repo::Repo;

const ENV: &str = "LOCK_CHILD_REPO";

/// Child: take the repo lock via real mutating commands in a loop; killed at a random point.
#[test]
fn child_committer() {
    let Ok(p) = std::env::var(ENV) else { return };
    let r = Repo::discover(std::path::Path::new(&p)).unwrap();
    println!("READY");
    let mut i = 0u64;
    loop {
        i += 1;
        let f = format!("c{i}");
        fs::write(r.worktree().join(&f), format!("v{i}")).unwrap();
        let _ = r.add(&[f]);
    }
}

#[test]
fn sigkill_during_mutations_leaves_repo_usable_and_fsck_clean() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(d.path()).unwrap();
    fs::write(d.path().join("base"), "b").unwrap();
    r.add(&["base".into()]).unwrap();
    for round in 0..6 {
        let mut c = Command::new(std::env::current_exe().unwrap())
            .args(["child_committer", "--exact", "--nocapture", "--test-threads=1"])
            .env(ENV, d.path())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut rd = BufReader::new(c.stdout.take().unwrap());
        let mut l = String::new();
        while rd.read_line(&mut l).unwrap() > 0 && !l.contains("READY") {
            l.clear();
        }
        std::thread::sleep(Duration::from_millis(40 + round * 13));
        c.kill().unwrap();
        c.wait().unwrap();
        let t = Instant::now();
        let f = format!("after{round}");
        fs::write(d.path().join(&f), "x").unwrap();
        r.add(&[f]).expect("next process proceeds after kill -9");
        assert!(t.elapsed() < Duration::from_secs(5));
        let rep = r.fsck().unwrap();
        assert!(rep.problems.is_empty(), "{:?}", rep.problems);
    }
}
