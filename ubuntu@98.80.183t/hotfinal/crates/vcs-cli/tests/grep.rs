//! `vcs grep`: worktree and revision modes, flags, zones, binary and chunked files.
use std::path::Path;
use std::process::{Command, Output};

fn vcs(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_TIMESTAMP", "1700000000")
        .output()
        .unwrap()
}
fn ok(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(
        o.status.success(),
        "vcs {args:?} failed: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::write(d.join("a.txt"), "Hello world\nsecond line\nHELLO again\n").unwrap();
    std::fs::write(d.join("src/b.txt"), "say hello\nhelloworld\n").unwrap();
    std::fs::write(d.join("bin.dat"), b"hello\0binary").unwrap();
    std::fs::write(d.join("ignored.log"), "hello ignored\n").unwrap();
    std::fs::write(d.join(".vcsignore"), "*.log\n").unwrap();
    ok(&d, &["add", "."]);
    ok(&d, &["commit", "-m", "init"]);
    (t, d)
}

#[test]
fn flags_and_modes() {
    let (_t, d) = repo();
    assert_eq!(ok(&d, &["grep", "hello"]), "src/b.txt:say hello\nsrc/b.txt:helloworld\n");
    assert_eq!(
        ok(&d, &["grep", "-n", "-i", "hello", "--", "a.txt"]),
        "a.txt:1:Hello world\na.txt:3:HELLO again\n"
    );
    assert_eq!(
        ok(&d, &["grep", "-w", "-i", "hello"]),
        "a.txt:Hello world\na.txt:HELLO again\nsrc/b.txt:say hello\n"
    );
    assert_eq!(ok(&d, &["grep", "-l", "-i", "hello"]), "a.txt\nsrc/b.txt\n");
    assert_eq!(ok(&d, &["grep", "-c", "-i", "hello"]), "a.txt:2\nsrc/b.txt:2\n");
    let o = vcs(&d, &["grep", "-F", "l.o"]);
    assert_eq!(o.status.code(), Some(1), "-F is literal: no `l.o` in the files");
    assert_eq!(ok(&d, &["grep", "-l", "l.o"]), "a.txt\nsrc/b.txt\n", "regex: l.o matches `llo`");
}

#[test]
fn no_match_exits_one_and_bad_pattern_errors() {
    let (_t, d) = repo();
    let o = vcs(&d, &["grep", "zzzz"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(o.stdout.is_empty());
    let o = vcs(&d, &["grep", "("]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("bad pattern"));
}

#[test]
fn worktree_sees_edits_but_revision_does_not() {
    let (_t, d) = repo();
    std::fs::write(d.join("a.txt"), "edited needle\n").unwrap();
    assert_eq!(ok(&d, &["grep", "needle"]), "a.txt:edited needle\n");
    assert_eq!(vcs(&d, &["grep", "needle", "HEAD"]).status.code(), Some(1));
    assert!(ok(&d, &["grep", "-i", "second", "HEAD"]).contains("a.txt:second line"));
    // untracked files are not searched
    std::fs::write(d.join("new.txt"), "needle\n").unwrap();
    assert_eq!(ok(&d, &["grep", "-l", "needle"]), "a.txt\n");
}

#[test]
fn zones_and_chunked_files() {
    let (_t, d) = repo();
    ok(&d, &["zone", "create", "lib", "--path", "lib"]);
    std::fs::create_dir_all(d.join("lib")).unwrap();
    std::fs::write(d.join("lib/z.txt"), "zone needle\n").unwrap();
    // a large file (several MiB of text) with the match near the end
    let mut big = String::new();
    for i in 0..400_000 {
        big.push_str(&format!("line number {i} of the big file\n"));
    }
    big.push_str("the needle is here\n");
    std::fs::write(d.join("big.txt"), &big).unwrap();
    ok(&d, &["add", "."]);
    ok(&d, &["commit", "-m", "more"]);
    assert_eq!(ok(&d, &["grep", "-l", "needle", "HEAD"]), "big.txt\nlib/z.txt\n");
    assert_eq!(ok(&d, &["grep", "-l", "needle"]), "big.txt\nlib/z.txt\n");
    assert_eq!(ok(&d, &["grep", "-l", "needle", "--", "lib"]), "lib/z.txt\n");
}
