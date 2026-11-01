//! `merge-base`, `diff-tree`, and that log/describe/bisect/blame give the same answers with and
//! without the commit-graph and changed-path caches (deleting `.vcs/commit-graphs` and
//! `.vcs/path-filters` between runs, and with a long enough history for the filters to be used).
use std::path::Path;
use std::process::{Command, Output};

fn run(dir: &Path, ts: u64, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_TIMESTAMP", ts.to_string())
        .output()
        .unwrap()
}
fn ok(dir: &Path, args: &[&str]) -> String {
    let o = run(dir, 1_700_000_000, args);
    assert!(
        o.status.success(),
        "vcs {args:?} failed: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// `n` commits touching `d{i%3}/f{i%7}.txt`; every 10th commit also edits `hot.txt`.
fn history(n: u64) -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    for i in 0..3 {
        std::fs::create_dir_all(d.join(format!("d{i}"))).unwrap();
    }
    std::fs::write(d.join("hot.txt"), "hot 0\nline\n").unwrap();
    for i in 0..n {
        let p = format!("d{}/f{}.txt", i % 3, i % 7);
        std::fs::write(d.join(&p), format!("{p} v{i}\n")).unwrap();
        let mut add = vec!["add", "--", "d0", "d1", "d2"];
        if i % 10 == 0 {
            std::fs::write(d.join("hot.txt"), format!("hot {i}\nline\n")).unwrap();
            add.push("hot.txt");
        }
        if i == 0 {
            add.push("hot.txt");
        }
        let o = run(&d, 1_700_000_000 + i * 10, &add);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let o = run(&d, 1_700_000_000 + i * 10, &["commit", "-m", &format!("c{i}")]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        if i == 40 {
            ok(&d, &["tag", "v1", "-m", "one"]);
        }
    }
    (t, d)
}

fn drop_caches(d: &Path) {
    for c in ["commit-graphs", "path-filters"] {
        let _ = std::fs::remove_dir_all(d.join(".vcs").join(c));
    }
}

#[test]
fn merge_base_and_diff_tree() {
    let (_t, d) = history(12);
    let head = ok(&d, &["rev-parse", "HEAD"]).trim().to_owned();
    let older = ok(&d, &["rev-parse", "HEAD~5"]).trim().to_owned();
    assert_eq!(ok(&d, &["merge-base", "HEAD", "HEAD~5"]).trim(), older);
    assert_eq!(ok(&d, &["merge-base", "HEAD~5", "HEAD"]).trim(), older);
    assert_eq!(ok(&d, &["merge-base", "HEAD", "HEAD"]).trim(), head);
    // c7..c11 (HEAD~4..HEAD) created d1/f0 d2/f1 d0/f2 d1/f3 d2/f4, and c10 edited hot.txt
    let out = ok(&d, &["diff-tree", "HEAD~5", "HEAD"]);
    let want = "A\td0/f2.txt\nA\td1/f0.txt\nA\td1/f3.txt\nA\td2/f1.txt\nA\td2/f4.txt\nM\thot.txt\n";
    assert_eq!(out, want);
    assert_eq!(ok(&d, &["diff-tree", "HEAD", "HEAD"]), "");
    // a file only in the older commit is a deletion, only in the newer an addition
    let first = ok(&d, &["rev-parse", "HEAD~11"]).trim().to_owned();
    let back = ok(&d, &["diff-tree", "HEAD", &first]);
    assert!(back.contains("D\td1/f0.txt") && back.contains("M\thot.txt"), "{back}");
    let j = ok(&d, &["diff-tree", "HEAD~5", "HEAD", "--json"]);
    assert!(j.contains("\"diff-tree\"") && j.contains("d1/f0.txt"), "{j}");
}

#[test]
fn history_commands_agree_with_and_without_caches() {
    // long enough (> 256 commits examined) that the changed-path filters are built and then used
    let (_t, d) = history(300);
    let cmds: Vec<Vec<&str>> = vec![
        vec!["log", "-n", "5", "--oneline", "--", "d1/f3.txt"],
        vec!["log", "-n", "1000", "--oneline", "--", "d1/f3.txt"],
        vec!["log", "-n", "1000", "--oneline", "--", "hot.txt"],
        vec!["log", "-n", "1000", "--oneline", "--", "d2"],
        vec!["log", "-n", "1000", "--oneline", "--", "d0", "hot.txt"],
        vec!["log", "-n", "1000", "--oneline", "--", "nothing/here"],
        vec!["log", "-n", "1000", "--oneline", "--follow", "--", "hot.txt"],
        vec!["log", "-n", "1000", "--oneline", "-S", "hot 100"],
        vec!["log", "-n", "50", "--name-status", "--format=%s"],
        vec!["blame", "hot.txt"],
        vec!["describe", "--always"],
        vec!["describe", "HEAD~100", "--always"],
        vec!["merge-base", "HEAD~7", "HEAD~250"],
        vec!["diff-tree", "HEAD~250", "HEAD"],
        vec!["show", "HEAD~120", "--name-status"],
    ];
    let cold: Vec<String> = cmds
        .iter()
        .map(|c| {
            drop_caches(&d);
            ok(&d, c)
        })
        .collect();
    // run everything twice in a row so the second pass reads the caches the first one wrote
    for pass in 0..2 {
        for (c, want) in cmds.iter().zip(&cold) {
            assert_eq!(&ok(&d, c), want, "pass {pass}: vcs {c:?}");
        }
    }
    assert!(d.join(".vcs/commit-graphs").exists());
    assert!(
        d.join(".vcs/path-filters").exists(),
        "a 300-commit path walk must have stored filters"
    );
    ok(&d, &["fsck"]);
    // sanity: the answers are not all empty
    assert!(cold[0].lines().count() == 5 && cold[1].lines().count() > 10, "{}", cold[1]);
    assert!(cold[8].contains('M'));
    assert!(cold[10].contains("v1-"), "{}", cold[10]);
}

#[test]
fn bisect_finds_the_same_commit_with_the_graph() {
    let (_t, d) = history(60);
    let good = ok(&d, &["rev-parse", "HEAD~50"]).trim().to_owned();
    for _ in 0..2 {
        ok(&d, &["bisect", "start", "HEAD", &good]);
        let out = ok(&d, &["bisect", "run", "sh", "-c", "! grep -q 'hot 30' hot.txt"]);
        assert!(out.contains("first bad commit"), "{out}");
        ok(&d, &["bisect", "reset"]);
        drop_caches(&d);
    }
}
