//! `vcs log` options on a hand-built history, plus `vcs gen`.
use std::path::Path;
use std::process::{Command, Output};

fn run(dir: &Path, who: &str, envs: &[(&str, &str)], args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
    c.args(args)
        .current_dir(dir)
        .env_remove("NO_COLOR")
        .env("VCS_AUTHOR_NAME", who)
        .env("VCS_AUTHOR_EMAIL", format!("{}@example.com", who.to_lowercase()));
    for (k, v) in envs {
        c.env(k, v);
    }
    c.output().unwrap()
}
fn as_(dir: &Path, who: &str, args: &[&str]) -> String {
    let o = run(dir, who, &[], args);
    assert!(o.status.success(), "vcs {args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn commit(dir: &Path, who: &str, file: &str, content: &str, msg: &str) {
    let p = dir.join(file);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, content).unwrap();
    as_(dir, who, &["add", file]);
    as_(dir, who, &["commit", "-m", msg]);
}

/// c1 (Ann, src/a.txt) -> c2 (Bob, docs/b.txt) -> c3 (Ann, src/a.txt) -> c4 (Bob, docs/b.txt)
fn history() -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    as_(t.path(), "Ann", &["init", "r"]);
    let d = t.path().join("r");
    commit(&d, "Ann", "src/a.txt", "1\n", "first src");
    commit(&d, "Bob", "docs/b.txt", "1\n", "first docs");
    commit(&d, "Ann", "src/a.txt", "2\n", "second src");
    commit(&d, "Bob", "docs/b.txt", "2\n", "second docs");
    (t, d)
}

#[test]
fn oneline_prints_short_id_and_subject() {
    let (_t, d) = history();
    let out = as_(&d, "Ann", &["log", "--oneline"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4, "{out}");
    assert!(lines[0].ends_with(" second docs") && lines[3].ends_with(" first src"), "{out}");
    assert!(!out.contains("Author:"), "{out}");
    assert!(lines[0].split(' ').next().unwrap().len() < 20);
}

#[test]
fn count_limits_after_filters() {
    let (_t, d) = history();
    assert_eq!(as_(&d, "Ann", &["log", "--oneline", "-n", "2"]).lines().count(), 2);
    let out = as_(&d, "Ann", &["log", "--oneline", "-n", "1", "--author", "ann"]);
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(out.contains("second src"), "{out}");
}

#[test]
fn path_filter_keeps_only_touching_commits() {
    let (_t, d) = history();
    let out = as_(&d, "Ann", &["log", "--oneline", "--", "src"]);
    assert_eq!(out.lines().count(), 2, "{out}");
    assert!(
        out.contains("second src") && out.contains("first src") && !out.contains("docs"),
        "{out}"
    );
    let out = as_(&d, "Ann", &["log", "--oneline", "--", "docs/b.txt"]);
    assert_eq!(out.lines().count(), 2, "{out}");
    // relative to the current directory
    let out = as_(&d.join("docs"), "Ann", &["log", "--oneline", "--", "b.txt"]);
    assert_eq!(out.lines().count(), 2, "{out}");
    let out = as_(&d, "Ann", &["log", "--oneline", "--", "nope"]);
    assert!(out.contains("no matching commits"), "{out}");
}

#[test]
fn author_filter_matches_name_or_email_case_insensitively() {
    let (_t, d) = history();
    let out = as_(&d, "Ann", &["log", "--oneline", "--author", "BOB"]);
    assert_eq!(out.lines().count(), 2, "{out}");
    assert!(!out.contains("src"), "{out}");
    let out = as_(&d, "Ann", &["log", "--oneline", "--author", "ann@example"]);
    assert_eq!(out.lines().count(), 2, "{out}");
}

#[test]
fn graph_linear_and_merge() {
    let (_t, d) = history();
    let out = as_(&d, "Ann", &["log", "--oneline", "--graph"]);
    assert_eq!(out.lines().count(), 4);
    assert!(out.lines().all(|l| l.starts_with("* ")), "{out}");

    // branch off, diverge, merge
    as_(&d, "Ann", &["branch", "feat"]);
    as_(&d, "Ann", &["zone", "checkout", "root", "feat"]);
    commit(&d, "Ann", "feat.txt", "f\n", "on feat");
    as_(&d, "Ann", &["zone", "checkout", "root", "main"]);
    commit(&d, "Bob", "main.txt", "m\n", "on main");
    let out = as_(&d, "Ann", &["merge", "feat"]);
    assert!(out.contains("merged"), "{out}");
    let out = as_(&d, "Ann", &["log", "--oneline", "--graph", "-n", "50"]);
    let stars = out.lines().filter(|l| l.contains('*')).count();
    assert_eq!(stars, 7, "{out}");
    assert!(out.contains('\\') && out.contains('/'), "{out}");
    // every commit subject appears exactly once
    for s in ["on feat", "on main", "first src"] {
        assert_eq!(out.matches(s).count(), 1, "{s}: {out}");
    }
}

#[test]
fn full_format_graph_prefixes_every_line() {
    let (_t, d) = history();
    let out = as_(&d, "Ann", &["log", "--graph", "-n", "2"]);
    assert!(out.lines().next().unwrap().starts_with("* commit "), "{out}");
    assert!(
        out.lines().any(|l| l.trim() == "second src" || l.contains("    second src")),
        "{out}"
    );
}

#[test]
fn colour_follows_flag_and_no_color() {
    let (_t, d) = history();
    let plain = run(&d, "Ann", &[], &["log", "--oneline"]);
    assert!(
        !String::from_utf8_lossy(&plain.stdout).contains('\x1b'),
        "piped stdout is not a terminal"
    );
    let on = run(&d, "Ann", &[], &["log", "--oneline", "--color", "always"]);
    assert!(String::from_utf8_lossy(&on.stdout).contains("\x1b[33m"));
    let off = run(&d, "Ann", &[], &["log", "--oneline", "--color", "never"]);
    assert!(!String::from_utf8_lossy(&off.stdout).contains('\x1b'));
}

#[test]
fn gen_completions_and_man() {
    let t = tempfile::tempdir().unwrap();
    for (shell, needle) in [("bash", "_vcs"), ("zsh", "#compdef vcs"), ("fish", "complete -c vcs")] {
        let o = run(t.path(), "Ann", &[], &["gen", "completions", shell]);
        assert!(o.status.success());
        let s = String::from_utf8_lossy(&o.stdout);
        assert!(
            s.contains(needle) && s.contains("oneline") || s.contains(needle) && s.contains("log"),
            "{shell}"
        );
    }
    let o = run(t.path(), "Ann", &[], &["gen", "man"]);
    assert!(o.status.success());
    let man = String::from_utf8_lossy(&o.stdout);
    assert!(man.contains(".TH vcs"), "{}", &man[..man.len().min(300)]);
    let dir = t.path().join("man");
    as_(t.path(), "Ann", &["gen", "man", "--dir", dir.to_str().unwrap()]);
    assert!(dir.join("vcs.1").exists() && dir.join("vcs-log.1").exists());
    let log = std::fs::read_to_string(dir.join("vcs-log.1")).unwrap();
    assert!(log.contains("oneline") && log.contains("graph"));
}
