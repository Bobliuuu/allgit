//! `vcs describe`.
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

fn commit(d: &Path, n: usize) {
    std::fs::write(d.join("f.txt"), format!("{n}\n")).unwrap();
    ok(d, &["add", "f.txt"]);
    ok(d, &["commit", "-m", &format!("c{n}")]);
}

#[test]
fn describe_distance_tags_and_always() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    commit(&d, 1);
    let o = vcs(&d, &["describe"]);
    assert!(!o.status.success(), "no tags yet");
    let short = ok(&d, &["describe", "--always"]).trim().to_owned();
    assert!(!short.is_empty() && !short.contains('-'));
    assert!(ok(&d, &["log", "-n", "1"]).contains(&short));
    ok(&d, &["tag", "light"]);
    assert!(!vcs(&d, &["describe"]).status.success(), "lightweight tag needs --tags");
    assert_eq!(ok(&d, &["describe", "--tags"]), "light\n");
    commit(&d, 2);
    ok(&d, &["tag", "-m", "release one", "v1"]);
    assert_eq!(ok(&d, &["describe"]), "v1\n");
    commit(&d, 3);
    commit(&d, 4);
    let out = ok(&d, &["describe"]);
    let id = out.trim().trim_start_matches("v1-2-g");
    assert!(id.len() >= 6 && id.bytes().all(|b| b.is_ascii_hexdigit()), "{out}");
    assert!(ok(&d, &["log", "-n", "1"]).contains(id), "short id is a prefix of the commit id");
    assert_eq!(ok(&d, &["describe", "HEAD~2"]), "v1\n");
}
