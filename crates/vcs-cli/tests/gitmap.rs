//! End to end: every git command in the mapping table (src/gitmap_table.rs) that has a scenario is
//! run with `vcs git ...` in a temp repository AND, when a `git` binary is on the PATH, the same
//! command line with real git in a twin repository. After each step the two repositories must agree
//! in meaning: tracked files and their contents, working-tree files, log subjects, branch name,
//! tags and the `status --porcelain` codes (without rename detection). Diff commands are compared
//! by their changed lines. The git side is only a yardstick; vcs never calls it.
//!
//! Rows whose `test` column names a scenario must have been executed by that scenario (checked at
//! the end); rows marked `-` are covered by the translation unit test only.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TABLE: &str = include_str!("../src/gitmap_table.rs");

/// Commit clock shared by both sides (0 = real time). `Op::T` sets it so that vcs and git give
/// every commit the same time and histories can be compared in order.
static CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn words(line: &str) -> Vec<String> {
    let mut out = vec![];
    let (mut cur, mut quote, mut any) = (String::new(), None::<char>, false);
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() || any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() || any {
        out.push(cur);
    }
    out
}

fn have_git() -> bool {
    Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Vcs,
    Git,
}

struct Side {
    kind: Kind,
    dir: PathBuf,
    /// Where `/srv/app.git` and friends point for this side.
    srv: PathBuf,
}

impl Side {
    fn cmd(&self, cwd: &Path, line: &str) -> Output {
        let w = words(line);
        assert_eq!(w[0], "git", "{line}");
        let args: Vec<String> = w[1..]
            .iter()
            .map(|a| {
                a.replace("ssh://host/app.git", self.srv.join("app.git").to_str().unwrap())
                    .replace("/srv/app.git", self.srv.join("app.git").to_str().unwrap())
                    .replace("/srv/app", self.srv.join("app.git").to_str().unwrap())
                    .replace("/srv/b", self.srv.join("b.git").to_str().unwrap())
            })
            .collect();
        let mut c = match self.kind {
            Kind::Vcs => {
                let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
                c.arg("git");
                c
            }
            Kind::Git => Command::new("git"),
        };
        let clock = CLOCK.load(std::sync::atomic::Ordering::SeqCst);
        if clock != 0 {
            let clock = 1_700_000_000 + clock;
            let d = format!("@{clock} +0000");
            c.env("VCS_TIMESTAMP", clock.to_string())
                .env("GIT_AUTHOR_DATE", &d)
                .env("GIT_COMMITTER_DATE", &d);
        }
        c.args(&args)
            .current_dir(cwd)
            .env("VCS_AUTHOR_NAME", "Dev")
            .env("VCS_AUTHOR_EMAIL", "dev@example.com")
            .env("VCS_GIT_QUIET", "1")
            .env("GIT_AUTHOR_NAME", "Dev")
            .env("GIT_AUTHOR_EMAIL", "dev@example.com")
            .env("GIT_COMMITTER_NAME", "Dev")
            .env("GIT_COMMITTER_EMAIL", "dev@example.com")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true")
            .env("EDITOR", "true")
            .output()
            .unwrap()
    }
    fn raw(&self, args: &[&str]) -> String {
        let o = match self.kind {
            Kind::Vcs => Command::new(env!("CARGO_BIN_EXE_vcs"))
                .args(args)
                .current_dir(&self.dir)
                .env("VCS_AUTHOR_NAME", "Dev")
                .env("VCS_AUTHOR_EMAIL", "dev@example.com")
                .output()
                .unwrap(),
            Kind::Git => Command::new("git")
                .args(args)
                .current_dir(&self.dir)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap(),
        };
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    /// What the repository looks like, in terms both tools can answer.
    fn observe(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        let (files, log, status, branch, tags) = match self.kind {
            Kind::Vcs => (
                self.raw(&["ls-files"]),
                self.raw(&["log", "-n", "100000", "--format=%s"])
                    .lines()
                    .filter(|l| !l.contains("no commits yet"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                self.git_line_vcs("status --porcelain"),
                self.git_line_vcs("rev-parse --abbrev-ref HEAD"),
                self.raw(&["tag"])
                    .lines()
                    .filter(|l| !l.starts_with("no tags"))
                    .map(|l| l.split_whitespace().next().unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Kind::Git => (
                self.raw(&["ls-files"]),
                self.raw(&["log", "--format=%s"]),
                self.raw(&["status", "--porcelain", "--no-renames"]),
                {
                    let b = self.raw(&["symbolic-ref", "--short", "HEAD"]);
                    if b.trim().is_empty() {
                        // during a rebase git's HEAD is detached; vcs keeps naming the branch being rebased
                        std::fs::read_to_string(self.dir.join(".git/rebase-merge/head-name"))
                            .ok()
                            .and_then(|h| h.trim().strip_prefix("refs/heads/").map(str::to_owned))
                            .unwrap_or_else(|| "HEAD".to_owned())
                    } else {
                        b
                    }
                },
                self.raw(&["tag"]),
            ),
        };
        let sorted = |s: &str| {
            let mut v: Vec<&str> = s.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
            v.sort();
            v.join("\n")
        };
        let mut tracked = String::new();
        for f in files.lines() {
            tracked.push_str(&format!("{f}={:?};", std::fs::read_to_string(self.dir.join(f)).ok()));
        }
        m.insert("tracked".into(), tracked);
        let mut subj: Vec<String> = log
            .lines()
            .map(|l| {
                let l = l.trim();
                match l.strip_prefix("Merge branch '").and_then(|r| r.split_once('\'')) {
                    Some((b, _)) => format!("Merge {b}"),
                    None => l.to_owned(),
                }
            })
            .collect();
        if subj.iter().any(|s| s.starts_with("Merge ")) {
            subj.sort(); // parent order of merges differs; compare as a multiset
        }
        m.insert("log".into(), subj.join("|"));
        m.insert("status".into(), sorted(&status));
        m.insert("branch".into(), branch.trim().to_owned());
        m.insert("tags".into(), sorted(&tags));
        m.insert("tree".into(), tree(&self.dir));
        m
    }
    fn git_line_vcs(&self, line: &str) -> String {
        let args: Vec<&str> = std::iter::once("git").chain(line.split(' ')).collect();
        self.raw(&args)
    }
}

/// All working-tree files except metadata directories, name=content.
fn tree(dir: &Path) -> String {
    fn walk(base: &Path, d: &Path, out: &mut Vec<String>) {
        let mut es: Vec<_> = std::fs::read_dir(d).unwrap().map(|e| e.unwrap()).collect();
        es.sort_by_key(|e| e.file_name());
        for e in es {
            let n = e.file_name().to_string_lossy().into_owned();
            if n == ".git" || n == ".vcs" {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                out.push(format!(
                    "{}={:?}",
                    p.strip_prefix(base).unwrap().display(),
                    std::fs::read_to_string(&p).ok()
                ));
            }
        }
    }
    let mut v = vec![];
    walk(dir, dir, &mut v);
    v.join(";")
}

/// The changed lines of a patch (no headers), sorted: equal for any two diffs that mean the same.
fn diff_lines(s: &str) -> Vec<String> {
    let mut v: Vec<String> = s
        .lines()
        .filter(|l| (l.starts_with('+') || l.starts_with('-')) && !l.starts_with("+++") && !l.starts_with("---"))
        .map(str::to_owned)
        .collect();
    v.sort();
    v
}

#[allow(dead_code)]
enum Op {
    /// Write a file in both repositories.
    W(&'static str, &'static str),
    /// Delete a file in both.
    #[allow(dead_code)]
    Rm(&'static str),
    /// Run in both; both must succeed and agree afterwards.
    G(&'static str),
    /// Like `G` but a staged change and an unstaged one count as the same (stash restores staged in vcs).
    GS(&'static str),
    /// Run in both; both must succeed and their outputs contain the needle.
    GC(&'static str, &'static str),
    /// Run in both; the changed lines of the two diffs are equal.
    D(&'static str),
    /// Run in both; the sorted non-empty output lines are equal.
    L(&'static str),
    /// Run in vcs only; must succeed and print the needle.
    V(&'static str, &'static str),
    /// Run in vcs only and require the failure text to contain the needle.
    X(&'static str, &'static str),
    /// Run the line in both; BOTH must fail.
    BF(&'static str),
    /// Set the commit time of the following commands (both sides).
    T(u64),
    /// Run in both; the output lines must be equal in the same order (merge subjects normalised).
    O(&'static str),
    /// Run the line (a `format-patch --stdout`) on the given tool's side only and save its output as
    /// `x.patch` in both working directories, so the next lines apply a patch made by that tool.
    Mk(Kind, &'static str),
}

struct Run {
    done: Vec<&'static str>,
}

fn run_scenario(name: &str, ops: &[Op], git_on: bool, fails: &mut Vec<String>) -> Vec<&'static str> {
    CLOCK.store(0, std::sync::atomic::Ordering::SeqCst);
    let t = tempfile::tempdir().unwrap();
    let mk = |kind: Kind, n: &str| {
        let dir = t.path().join(n);
        std::fs::create_dir_all(&dir).unwrap();
        let srv = t.path().join(format!("srv-{n}"));
        std::fs::create_dir_all(&srv).unwrap();
        Side { kind, dir, srv }
    };
    let v = mk(Kind::Vcs, "v");
    let g = mk(Kind::Git, "g");
    let o = v.cmd(&v.dir, "git init");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    if git_on {
        for a in [
            &["init", "-q"][..],
            &["symbolic-ref", "HEAD", "refs/heads/main"][..],
            &["config", "commit.gpgsign", "false"][..],
        ] {
            let o = Command::new("git")
                .args(a)
                .current_dir(&g.dir)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(o.status.success());
        }
        // a bare remote for the git side
        let o = Command::new("git")
            .args(["init", "-q", "--bare", g.srv.join("app.git").to_str().unwrap()])
            .output()
            .unwrap();
        assert!(o.status.success());
        let _ = Command::new("git")
            .args(["symbolic-ref", "HEAD", "refs/heads/main"])
            .current_dir(g.srv.join("app.git"))
            .output();
    }
    let mut run = Run { done: vec![] };
    let mut fail = |msg: String| fails.push(format!("[{name}] {msg}"));
    for op in ops {
        match op {
            Op::W(p, c) => {
                for s in [&v, &g] {
                    let f = s.dir.join(p);
                    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
                    std::fs::write(f, c).unwrap();
                }
            }
            Op::Rm(p) => {
                for s in [&v, &g] {
                    let _ = std::fs::remove_file(s.dir.join(p));
                }
            }
            Op::V(line, needle) | Op::X(line, needle) => {
                run.done.push(line);
                let o = v.cmd(&v.dir, line);
                let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                let want_ok = matches!(op, Op::V(..));
                if o.status.success() != want_ok || !text.contains(needle) {
                    fail(format!(
                        "`vcs {line}` (success={}, want {want_ok}) lacks {needle:?}:\n{text}",
                        o.status.success()
                    ));
                }
            }
            Op::T(t) => CLOCK.store(*t, std::sync::atomic::Ordering::SeqCst),
            Op::Mk(kind, line) => {
                run.done.push(line);
                // without real git the "git-made" patch comes from vcs, so the rest still runs
                let side = if *kind == Kind::Git && git_on { &g } else { &v };
                let o = side.cmd(&side.dir, line);
                if !o.status.success() {
                    fail(format!("`{line}` failed:\n{}", String::from_utf8_lossy(&o.stderr)));
                    continue;
                }
                for s in [&v, &g] {
                    std::fs::write(s.dir.join("x.patch"), &o.stdout).unwrap();
                }
            }
            Op::BF(line) => {
                run.done.push(line);
                for s in [&v, &g].into_iter().filter(|s| s.kind == Kind::Vcs || git_on) {
                    if s.cmd(&s.dir, line).status.success() {
                        fail(format!(
                            "`{line}` should fail on {}",
                            if s.kind == Kind::Vcs { "vcs" } else { "git" }
                        ));
                    }
                }
            }
            Op::G(line) | Op::GS(line) | Op::GC(line, _) | Op::D(line) | Op::L(line) | Op::O(line) => {
                run.done.push(line);
                let ov = v.cmd(&v.dir, line);
                let og = if git_on { Some(g.cmd(&g.dir, line)) } else { None };
                let tv = format!("{}{}", String::from_utf8_lossy(&ov.stdout), String::from_utf8_lossy(&ov.stderr));
                if !ov.status.success() {
                    fail(format!("`vcs {line}` failed:\n{tv}"));
                    continue;
                }
                if let Some(og) = &og {
                    if !og.status.success() {
                        fail(format!(
                            "`{line}` failed in real git (scenario bug):\n{}",
                            String::from_utf8_lossy(&og.stderr)
                        ));
                        continue;
                    }
                    let to = String::from_utf8_lossy(&og.stdout).into_owned();
                    match op {
                        Op::D(_) => {
                            let (a, b) = (diff_lines(&String::from_utf8_lossy(&ov.stdout)), diff_lines(&to));
                            if a != b {
                                fail(format!("`{line}`: changed lines differ\n vcs: {a:?}\n git: {b:?}"));
                            }
                        }
                        Op::O(_) => {
                            let norm = |s: &str| -> Vec<String> {
                                s.lines()
                                    .map(|l| {
                                        let l = l.trim();
                                        if l.starts_with("Merge ") {
                                            "Merge".to_owned()
                                        } else {
                                            l.to_owned()
                                        }
                                    })
                                    .collect()
                            };
                            let (a, b) = (norm(&String::from_utf8_lossy(&ov.stdout)), norm(&to));
                            if a != b {
                                fail(format!("`{line}`: output or order differs\n vcs: {a:?}\n git: {b:?}"));
                            }
                        }
                        Op::L(_) => {
                            let norm = |s: &str| {
                                let mut x: Vec<String> = s.lines().map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()).collect();
                                x.sort();
                                x
                            };
                            let (a, b) = (norm(&String::from_utf8_lossy(&ov.stdout)), norm(&to));
                            if a != b {
                                fail(format!("`{line}`: output differs\n vcs: {a:?}\n git: {b:?}"));
                            }
                        }
                        Op::GC(_, needle) => {
                            let tg = format!("{to}{}", String::from_utf8_lossy(&og.stderr));
                            if !tv.contains(needle) || !tg.contains(needle) {
                                fail(format!("`{line}`: needle {needle:?} missing\n vcs: {tv}\n git: {tg}"));
                            }
                        }
                        _ => {}
                    }
                    let (mut a, mut b) = (v.observe(), g.observe());
                    if matches!(op, Op::GS(_)) {
                        for m in [&mut a, &mut b] {
                            let s = m["status"].replace(" M ", "M  ").replace("\nM ", "\n M");
                            let s: Vec<String> = s.lines().map(|l| format!("M {}", &l[3..])).collect();
                            m.insert("status".into(), s.join("\n"));
                        }
                    }
                    if a != b {
                        let diffs: Vec<String> = a
                            .iter()
                            .filter(|(k, x)| b.get(*k) != Some(x))
                            .map(|(k, x)| format!("  {k}:\n    vcs {x:?}\n    git {:?}", b.get(k)))
                            .collect();
                        fail(format!("after `{line}` the repositories differ:\n{}", diffs.join("\n")));
                    }
                } else if let Op::GC(_, needle) = op {
                    if !tv.contains(needle) {
                        fail(format!("`vcs {line}` lacks {needle:?}:\n{tv}"));
                    }
                }
            }
        }
    }
    run.done
}

use Op::*;

#[allow(dead_code)]
fn _unused() {}

fn commit() -> Op {
    G(r#"git commit -m "msg""#)
}

fn scenarios() -> Vec<(&'static str, Vec<Op>)> {
    vec![
        (
            "basics",
            vec![
                W("f.txt", "1\n"),
                W("a.txt", "a\n"),
                W("dir/x.txt", "x\n"),
                GC("git status", "f.txt"),
                G("git status -s"),
                G("git status --porcelain"),
                G("git status -sb"),
                G("git add f.txt"),
                G("git add ."),
                commit(),
                W("f.txt", "1\n2\n"),
                D("git diff"),
                GC("git diff --stat", "f.txt"),
                L("git diff --name-only"),
                GC("git diff --name-status", "M"),
                D("git diff -U5 -w"),
                D("git diff --patience"),
                D("git diff --no-color"),
                D("git diff -M"),
                G("git add -u"),
                D("git diff --staged"),
                D("git diff --cached"),
                commit(),
                G(r#"git commit --allow-empty -m "msg""#),
                W("f.txt", "1\n2\nauth\n"),
                G("git add f.txt"),
                G(r#"git commit --author="A <a@x>" -m "msg""#),
                O("git log -n 1 --format=%an"),
                O("git log -n 1 --format=%ae"),
                W("f.txt", "1\n2\n3\n"),
                G(r#"git commit -a -m "msg""#),
                G("git add -A"),
                G("git mv a.txt b.txt"),
                commit(),
                G("git rm f.txt"),
                G("git restore --staged f.txt"),
                G("git restore f.txt"),
                G("git rm --cached f.txt"),
                G("git add f.txt"),
                G("git rm -rf dir"),
                commit(),
                // branches
                G("git branch feature"),
                GC("git branch", "feature"),
                G("git switch feature"),
                G("git switch main"),
                G("git switch -c other"),
                G("git checkout main"),
                G("git checkout -b feature2"),
                G("git checkout main"),
                G("git branch -d feature"),
                G("git branch -D feature2"),
                GC("git branch --show-current", "main"),
                W("f.txt", "scratch\n"),
                G("git checkout -- f.txt"),
                W("f.txt", "scratch\n"),
                G("git restore f.txt"),
                W("f.txt", "scratch\n"),
                G("git add f.txt"),
                G("git reset HEAD -- f.txt"),
                G("git restore f.txt"),
                G("git diff --exit-code"),
                W("f.txt", "dirty\n1\n"),
                BF("git diff --exit-code"),
                L("git diff --numstat"),
                G("git restore f.txt"),
            ],
        ),
        (
            "ranges",
            vec![
                T(100),
                W("a.txt", "m1\n"),
                G("git add ."),
                G(r#"git commit -m "m1""#),
                G("git branch topic"),
                T(200),
                W("b.txt", "m2\n"),
                G("git add ."),
                G(r#"git commit -m "m2""#),
                G("git branch feature"),
                G("git switch feature"),
                T(300),
                W("f.txt", "f1\n"),
                G("git add ."),
                G(r#"git commit -m "f1""#),
                T(500),
                W("g.txt", "f2\n"),
                G("git add ."),
                G(r#"git commit -m "f2""#),
                G("git switch main"),
                T(400),
                W("c.txt", "m3\n"),
                G("git add ."),
                G(r#"git commit -m "m3""#),
                T(600),
                G("git merge feature"),
                T(700),
                W("d.txt", "m4\n"),
                G("git add ."),
                G(r#"git commit -m "m4""#),
                G("git switch topic"),
                T(800),
                W("t.txt", "p1\n"),
                G("git add ."),
                G(r#"git commit -m "p1""#),
                G("git switch main"),
                // ranges: same commits, same order as git
                O("git log --format=%s main..feature"),
                O("git log --format=%s main..topic"),
                O("git log --format=%s main..feature"),
                O("git log --format=%s main...feature"),
                O("git log ^main feature --format=%s"),
                O("git log --format=%s feature..main"),
                O("git log --format=%s main...feature"),
                O("git log --format=%s topic...main"),
                O("git log --format=%s main...topic"),
                O("git log --format=%s ^feature main"),
                O("git log --format=%s feature --not main"),
                O("git log --format=%s main --not feature topic"),
                O("git log --format=%s --reverse main..topic"),
                O("git log --format=%s -n 2 topic...main"),
                O("git log --format=%s main~1..main"),
                O("git log --format=%s --first-parent topic...main"),
                O("git log --format=%s --all"),
                O("git log --format=%s --author=Dev topic..main"),
                O("git log --format=%s topic..main -- c.txt"),
                O("git log --format=%s main..main"),
                O("git rev-list --count topic...main"),
                O("git rev-list --count main..feature"),
                O("git rev-list --count --all"),
                O("git rev-list --count -n 3 --all"),
                O("git rev-list --count feature..main"),
                L("git shortlog -sn main..feature"),
                L("git shortlog -sn topic..main"),
                D("git diff topic...main"),
                D("git diff main...topic"),
                D("git diff feature...main"),
                GC("git log --all", "p1"),
                // the exact table rows
                G("git log --all"),
                G("git log main..feature"),
                G("git log main...feature"),
                G("git log ^main feature"),
                G("git log feature --not main"),
                G("git log --reverse main..feature"),
                G("git log -n 2 main...feature"),
                G("git rev-list --count main..feature"),
                G("git rev-list --all"),
                G("git shortlog -sn main..feature"),
                G("git diff main...feature"),
            ],
        ),
        (
            "detach",
            vec![
                T(100),
                W("a.txt", "c1\n"),
                G("git add ."),
                G(r#"git commit -m "c1""#),
                G("git tag v1.0"),
                T(200),
                W("b.txt", "c2\n"),
                G("git add ."),
                G(r#"git commit -m "c2""#),
                T(300),
                W("c.txt", "c3\n"),
                G("git add ."),
                G(r#"git commit -m "c3""#),
                G("git branch side main~1"),
                G("git checkout --detach v1.0"),
                GC("git status", "detached"),
                O("git log --format=%s"),
                // commits on a detached HEAD
                T(400),
                W("x.txt", "d1\n"),
                G("git add ."),
                G(r#"git commit -m "d1""#),
                O("git log --format=%s"),
                O("git log --format=%s --all"),
                O("git log --format=%s main..HEAD"),
                O("git log --format=%s HEAD..main"),
                G("git switch -c rescue"),
                O("git log --format=%s"),
                G("git switch main"),
                G("git switch --detach HEAD~1"),
                O("git log --format=%s"),
                G("git checkout main"),
                G("git checkout v1.0"),
                O("git log --format=%s"),
                G("git checkout main"),
                G("git checkout --detach"),
                G("git switch --detach side"),
                G("git switch main"),
                // rename and branch-at
                G("git branch -m main trunk"),
                GC("git branch --show-current", "trunk"),
                G("git branch -m trunk main"),
                G("git branch -M rescue side"),
                G("git branch topic3 main~1"),
                G("git switch -c topic2 main"),
                G("git switch main"),
                G("git checkout -b feature2 main"),
                G("git switch -c b3 v1.0"),
                G("git switch main"),
                G("git branch -D feature2"),
                G("git branch -D b3"),
                BF("git switch v1.0"),
                BF("git branch -m nope other"),
                BF("git branch -m topic3 topic2"),
            ],
        ),
        (
            "history",
            vec![
                W("f.txt", "v1\n"),
                G("git add f.txt"),
                commit(),
                G("git branch feature"),
                W("f.txt", "v2\n"),
                G("git add f.txt"),
                commit(),
                W("f.txt", "v3\n"),
                G("git add f.txt"),
                commit(),
                D("git diff HEAD~1 HEAD"),
                D("git diff main..feature"),
                GC("git blame f.txt", "v3"),
                G("git blame -M f.txt"),
                G("git blame HEAD~1 -- f.txt"),
                W("m.txt", "a\nb\nc\nd\ne\nf\n"),
                G("git add m.txt"),
                commit(),
                G("git blame -L 1,5 m.txt"),
                W("f.txt", "v4\n"),
                D("git diff HEAD"),
                D("git diff -- f.txt"),
                G("git log"),
                G("git log -n 5 --oneline"),
                G("git log -5"),
                G("git log --graph --oneline"),
                G("git log --author=Dev --since=2024-01-01"),
                G("git log --grep=fix"),
                G("git log -S needle"),
                G("git log -G v3"),
                O("git log -G v3 --format=%s"),
                O("git log -G ^v[12]$ --format=%s"),
                G("git log --follow -- f.txt"),
                G("git log --name-status"),
                G("git log --format=%h"),
                G("git log main -- f.txt"),
                GC("git log -p", "v3"),
                GC("git log --stat", "f.txt"),
                G("git show"),
                G("git show HEAD~1"),
                G("git show --stat HEAD"),
                GC("git show HEAD:f.txt", "v3"),
                G("git checkout HEAD~1 -- f.txt"),
                W("f.txt", "v4\n"),
                G("git restore --source=HEAD~1 f.txt"),
            ],
        ),
        (
            "amend",
            vec![
                W("f.txt", "1\n"),
                G("git add f.txt"),
                commit(),
                W("f.txt", "2\n"),
                G("git add f.txt"),
                G(r#"git commit -m "second""#),
                W("f.txt", "3\n"),
                G("git add f.txt"),
                G(r#"git commit --amend -m "msg""#),
                W("f.txt", "4\n"),
                G("git add f.txt"),
                G("git commit --amend --no-edit"),
            ],
        ),
        (
            "reset",
            vec![
                W("f.txt", "1\n"),
                G("git add f.txt"),
                commit(),
                W("f.txt", "2\n"),
                G("git add f.txt"),
                commit(),
                W("f.txt", "3\n"),
                G("git add f.txt"),
                commit(),
                G("git reset --soft HEAD~1"),
                commit(),
                G("git reset --mixed HEAD~1"),
                G("git add f.txt"),
                G("git reset"),
                W("f.txt", "uncommitted\n"),
                W("new.txt", "untracked\n"),
                G("git reset --hard HEAD~1"),
            ],
        ),
        (
            "merge",
            vec![
                W("f.txt", "base\n"),
                W("g.txt", "g\n"),
                G("git add ."),
                commit(),
                G("git switch -c feature"),
                W("g.txt", "g feature\n"),
                G("git add g.txt"),
                G(r#"git commit -m "feat""#),
                G("git switch main"),
                W("f.txt", "base main\n"),
                G("git add f.txt"),
                commit(),
                G("git merge feature"),
                // a conflicting merge, then give up
                G("git switch -c left"),
                W("f.txt", "left\n"),
                G("git add f.txt"),
                G(r#"git commit -m "left""#),
                G("git switch main"),
                W("f.txt", "right\n"),
                G("git add f.txt"),
                G(r#"git commit -m "right""#),
                Op::BF("git merge left"),
                G("git merge --abort"),
                // a conflicting rebase, then give up
                G("git switch left"),
                Op::BF("git rebase main"),
                G("git rebase --abort"),
                // and again, this time dropping the conflicting commit
                Op::BF("git rebase main"),
                G("git rebase --skip"),
            ],
        ),
        (
            "rebase_i",
            vec![
                W("f.txt", "base\n"),
                G("git add f.txt"),
                commit(),
                G("git switch -c feature"),
                W("a.txt", "a\n"),
                G("git add a.txt"),
                G(r#"git commit -m "add a""#),
                W("b.txt", "b\n"),
                G("git add b.txt"),
                G(r#"git commit -m "add b""#),
                W("a.txt", "a fixed\n"),
                G("git add a.txt"),
                G(r#"git commit -m "fixup! add a""#),
                G("git rebase -i HEAD~3"),
                G("git rebase -i --autosquash HEAD~3"),
                G("git switch main"),
                W("h.txt", "h\n"),
                G("git add h.txt"),
                commit(),
                G("git switch feature"),
                G(r#"git rebase -x "test -f f.txt" main"#),
            ],
        ),
        (
            "patch",
            vec![
                W("f.txt", "1\n2\n3\n4\n5\n6\n7\n8\n"),
                G("git add ."),
                commit(),
                W("f.txt", "1\n2\n3\n4\nfive\n6\n7\n8\n"),
                W("new.txt", "n\n"),
                G("git add ."),
                G(r#"git commit -m "second""#),
                // a patch made by git, applied by vcs and by git
                Mk(Kind::Git, "git format-patch -1 --stdout"),
                G("git reset --hard HEAD~1"),
                G("git apply --check x.patch"),
                GC("git apply --stat x.patch", "f.txt"),
                GC("git apply --stat x.patch", "new.txt"),
                // index only: the working tree keeps the old file, the index gets the patched one
                G("git apply --cached x.patch"),
                G("git reset --hard HEAD"),
                G("git apply x.patch"),
                G("git apply -R x.patch"),
                G("git apply --index x.patch"),
                G("git reset --hard HEAD"),
                G("git am x.patch"),
                // a patch made by vcs, applied by vcs and by git
                Mk(Kind::Vcs, "git format-patch -1 --stdout"),
                G("git reset --hard HEAD~1"),
                G("git apply --check x.patch"),
                G("git apply --index x.patch"),
                G("git reset --hard HEAD"),
                G("git am x.patch"),
                V("git format-patch --cover-letter -1", "0000-cover-letter.patch"),
                Rm("0000-cover-letter.patch"),
                Rm("0001-second.patch"),
            ],
        ),
        (
            "merge-shapes",
            vec![
                W("f.txt", "base\n"),
                G("git add f.txt"),
                commit(),
                G("git switch -c feature"),
                W("a.txt", "a\n"),
                G("git add a.txt"),
                G(r#"git commit -m "feat1""#),
                G("git switch main"),
                G("git merge --ff-only feature"),
                // diverge: --ff-only now refuses in both, nothing changes
                G("git switch feature"),
                W("b.txt", "b\n"),
                G("git add b.txt"),
                G(r#"git commit -m "feat2""#),
                G("git switch main"),
                W("f.txt", "main moved\n"),
                G("git add f.txt"),
                commit(),
                Op::BF("git merge --ff-only feature"),
                G("git merge --no-ff feature"),
                // a fast-forward is possible, --no-ff and -m still make a merge commit
                G("git switch feature"),
                W("c.txt", "c\n"),
                G("git add c.txt"),
                G(r#"git commit -m "feat3""#),
                G("git switch main"),
                G("git merge --no-ff -m release feature"),
                // squash: staged, one ordinary commit
                G("git switch feature"),
                W("d.txt", "d\n"),
                G("git add d.txt"),
                G(r#"git commit -m "feat4""#),
                G("git switch main"),
                G("git merge --squash feature"),
                G(r#"git commit -m "squashed""#),
            ],
        ),
        (
            "push-force",
            vec![
                W("f.txt", "one\n"),
                G("git add f.txt"),
                commit(),
                G("git remote add origin ssh://host/app.git"),
                G("git push -u origin main"),
                L("git branch -r"),
                W("f.txt", "one\ntwo\n"),
                G("git add f.txt"),
                G(r#"git commit -m "two""#),
                G("git push"),
                // rewrite the last commit: a plain push is refused by both
                G("git reset --hard HEAD~1"),
                W("f.txt", "one\nTWO\n"),
                G("git add f.txt"),
                G(r#"git commit -m "two again""#),
                Op::BF("git push"),
                G("git push --force"),
                W("f.txt", "one\nTHREE\n"),
                G("git add f.txt"),
                G("git commit --amend -m three"),
                G("git push -f origin main"),
                // a second branch, then delete it on the remote
                G("git switch -c feature"),
                W("g.txt", "g\n"),
                G("git add g.txt"),
                G(r#"git commit -m "feat""#),
                G("git push -u origin feature"),
                G("git switch main"),
                G("git push origin --delete feature"),
                Op::BF("git push origin --delete feature"),
                G("git remote remove origin"),
                L("git branch -r"),
                Op::BF("git push"),
                Op::BF("git remote remove origin"),
            ],
        ),
        (
            "rebase",
            vec![
                W("f.txt", "base\n"),
                G("git add f.txt"),
                commit(),
                G("git switch -c feature"),
                W("g.txt", "g\n"),
                G("git add g.txt"),
                G(r#"git commit -m "feat""#),
                G("git switch main"),
                W("h.txt", "h\n"),
                G("git add h.txt"),
                commit(),
                G("git switch feature"),
                G("git rebase main"),
            ],
        ),
        (
            // a range with a merge commit, recreated; the side branch is not touched
            "rebase_merges",
            vec![
                W("f.txt", "base\n"),
                G("git add f.txt"),
                commit(),
                G("git switch -c feature"),
                W("a.txt", "a\n"),
                G("git add a.txt"),
                G(r#"git commit -m "add a""#),
                G("git switch -c side"),
                W("s.txt", "s\n"),
                G("git add s.txt"),
                G(r#"git commit -m "add s""#),
                G("git switch feature"),
                W("b.txt", "b\n"),
                G("git add b.txt"),
                G(r#"git commit -m "add b""#),
                G("git merge --no-ff side"),
                W("c.txt", "c\n"),
                G("git add c.txt"),
                G(r#"git commit -m "add c""#),
                G("git switch main"),
                W("h.txt", "h\n"),
                G("git add h.txt"),
                commit(),
                G("git switch feature"),
                G("git rebase --rebase-merges main"),
                L("git log --format=%s"),
                G("git switch side"),
                L("git log --format=%s"),
            ],
        ),
        (
            // commits that do not descend from the merge base keep their parents by default ...
            "rebase_nocousins",
            vec![
                W("g.txt", "1\n2\n3\n"),
                G("git add g.txt"),
                commit(),
                G("git switch -c topic"),
                W("t1.txt", "t1\n"),
                G("git add t1.txt"),
                G(r#"git commit -m "t1""#),
                G("git switch main"),
                W("m1.txt", "m1\n"),
                G("git add m1.txt"),
                commit(),
                G("git switch topic"),
                G(r#"git merge --no-ff -m "Merge main into topic" main"#),
                W("t2.txt", "t2\n"),
                G("git add t2.txt"),
                G(r#"git commit -m "t2""#),
                G("git switch main"),
                W("m2.txt", "m2\n"),
                G("git add m2.txt"),
                commit(),
                G("git switch topic"),
                G("git rebase --rebase-merges=no-rebase-cousins main"),
            ],
        ),
        (
            // ... and are replayed with rebase-cousins
            "rebase_cousins",
            vec![
                W("g.txt", "1\n2\n3\n"),
                G("git add g.txt"),
                commit(),
                G("git switch -c topic"),
                W("t1.txt", "t1\n"),
                G("git add t1.txt"),
                G(r#"git commit -m "t1""#),
                G("git switch main"),
                W("m1.txt", "m1\n"),
                G("git add m1.txt"),
                commit(),
                G("git switch topic"),
                G(r#"git merge --no-ff -m "Merge main into topic" main"#),
                W("t2.txt", "t2\n"),
                G("git add t2.txt"),
                G(r#"git commit -m "t2""#),
                G("git switch main"),
                W("m2.txt", "m2\n"),
                G("git add m2.txt"),
                commit(),
                G("git switch topic"),
                G("git rebase --rebase-merges=rebase-cousins main"),
            ],
        ),
        (
            // the recreated merge itself conflicts: abort, then resolve and continue
            "rebase_merges_conflict",
            vec![
                W("g.txt", "1\n2\n3\n4\n5\n6\n7\n8\n"),
                G("git add g.txt"),
                commit(),
                G("git switch -c topic"),
                W("g.txt", "1\nTOPIC\n3\n4\n5\n6\n7\n8\n"),
                G("git add g.txt"),
                G(r#"git commit -m "t1""#),
                G("git switch main"),
                W("m1.txt", "m1\n"),
                G("git add m1.txt"),
                commit(),
                G("git switch topic"),
                G(r#"git merge --no-ff -m "Merge main into topic" main"#),
                W("t2.txt", "t2\n"),
                G("git add t2.txt"),
                G(r#"git commit -m "t2""#),
                G("git switch main"),
                W("g.txt", "1\nMAIN\n3\n4\n5\n6\n7\n8\n"),
                G("git add g.txt"),
                commit(),
                G("git switch topic"),
                Op::BF("git rebase --rebase-merges main"),
                G("git rebase --abort"),
                Op::BF("git rebase --rebase-merges main"),
                W("g.txt", "1\nBOTH\n3\n4\n5\n6\n7\n8\n"),
                G("git add g.txt"),
                G("git rebase --continue"),
            ],
        ),
        (
            // skipping a conflicting merge drops it and carries on with the commits after it
            "rebase_merges_skip",
            vec![
                W("g.txt", "1\n2\n3\n4\n5\n6\n7\n8\n"),
                G("git add g.txt"),
                commit(),
                G("git switch -c topic"),
                W("g.txt", "1\nTOPIC\n3\n4\n5\n6\n7\n8\n"),
                G("git add g.txt"),
                G(r#"git commit -m "t1""#),
                G("git switch main"),
                W("m1.txt", "m1\n"),
                G("git add m1.txt"),
                commit(),
                G("git switch topic"),
                G(r#"git merge --no-ff -m "Merge main into topic" main"#),
                W("t2.txt", "t2\n"),
                G("git add t2.txt"),
                G(r#"git commit -m "t2""#),
                G("git switch main"),
                W("g.txt", "1\nMAIN\n3\n4\n5\n6\n7\n8\n"),
                G("git add g.txt"),
                commit(),
                G("git switch topic"),
                Op::BF("git rebase --rebase-merges main"),
                G("git rebase --skip"),
            ],
        ),
        (
            // two merge bases (a criss-cross history): merged into a virtual base, as git's ort does
            "criss_cross",
            vec![
                W("d.txt", "d\n"),
                W("e.txt", "e\n"),
                W("f.txt", "1\n2\n3\n4\n5\n6\n7\n8\n"),
                G("git add ."),
                commit(),
                G("git switch -c other"),
                G("git rm e.txt"),
                G(r#"git commit -m "x deletes e""#),
                G("git branch xs"),
                G("git switch main"),
                G("git rm d.txt"),
                G(r#"git commit -m "y deletes d""#),
                G("git branch ys"),
                G("git switch other"),
                G("git merge ys"),
                G("git switch main"),
                G("git merge xs"),
                W("d.txt", "d again\n"),
                W("e.txt", "e again\n"),
                W("f.txt", "1\nMAIN\n3\n4\n5\n6\n7\n8\n"),
                G("git add ."),
                G(r#"git commit -m "main re-adds""#),
                G("git switch other"),
                W("f.txt", "1\n2\n3\n4\n5\n6\n7\nOTHER\n"),
                G("git add f.txt"),
                G(r#"git commit -m "other edit""#),
                G("git switch main"),
                G("git merge other"),
            ],
        ),
        (
            "pick",
            vec![
                W("a.txt", "a\n"),
                W("b.txt", "b\n"),
                W("c.txt", "c\n"),
                G("git add ."),
                commit(),
                G("git switch -c feature"),
                W("a.txt", "a2\n"),
                G("git add a.txt"),
                G(r#"git commit -m "feat""#),
                G("git switch main"),
                G("git cherry-pick feature"),
                W("b.txt", "b2\n"),
                G("git add b.txt"),
                commit(),
                W("c.txt", "c2\n"),
                G("git add c.txt"),
                commit(),
                G("git revert HEAD"),
                G("git revert --no-edit HEAD~2"),
                W("d.txt", "d\n"),
                G("git add d.txt"),
                commit(),
                G("git revert -n HEAD"),
                commit(),
            ],
        ),
        (
            "stash",
            vec![
                W("f.txt", "1\n"),
                G("git add f.txt"),
                commit(),
                W("f.txt", "2\n"),
                G("git stash"),
                GS("git stash pop"),
                G("git stash push -m wip"),
                GC("git stash list", "wip"),
                GS("git stash apply stash@{0}"),
                G("git add f.txt"),
                G("git stash drop"),
                G("git stash list"),
                W("n.txt", "new\n"),
                W("f.txt", "3\n"),
                G("git stash -u"),
                G("git stash list"),
                G("git stash drop"),
            ],
        ),
        (
            "notes",
            vec![
                W("f.txt", "1\n"),
                G("git add f.txt"),
                commit(),
                W("f.txt", "2\n"),
                G("git add f.txt"),
                commit(),
                G("git notes add -m first HEAD~1"),
                O("git notes show HEAD~1"),
                BF("git notes show HEAD"),
                BF("git notes add -m again HEAD~1"),
                BF("git notes remove HEAD"),
                G("git notes add -f -m replaced HEAD"),
                O("git notes show HEAD"),
                G("git notes append -m more HEAD"),
                O("git notes show HEAD"),
                V("git notes list", " "),
                G("git notes remove HEAD"),
                BF("git notes show HEAD"),
                O("git notes show HEAD~1"),
            ],
        ),
        (
            "tags",
            vec![
                W("f.txt", "1\n"),
                G("git add f.txt"),
                commit(),
                W("f.txt", "2\n"),
                G("git add f.txt"),
                commit(),
                G("git tag v1"),
                GC("git tag", "v1"),
                G("git tag v0 HEAD~1"),
                BF("git tag v0"),
                G("git tag -f v0"),
                G("git tag -f v9"),
                G("git tag -d v9"),
                G("git tag -f v1"),
                G("git tag -d v0"),
                G("git tag -a v2 -m rel"),
                GC("git describe --tags", "v"),
                G("git describe --always"),
                GC("git describe --dirty", "v"),
                W("f.txt", "dirty\n"),
                GC("git describe --dirty", "-dirty"),
                G("git restore f.txt"),
                BF("git describe --dirty HEAD~1"),
            ],
        ),
        (
            "plumbing",
            vec![
                W("f.txt", "1\n"),
                G("git add f.txt"),
                commit(),
                V("git rev-parse --git-dir", ".vcs"),
            ],
        ),
        (
            "ignored",
            vec![
                W("f.txt", "1\n"),
                W(".gitignore", "*.log\nbuild/\n"),
                W(".vcsignore", "*.log\nbuild/\n"),
                G("git add f.txt .gitignore .vcsignore"),
                commit(),
                W("ignored.log", "x\n"),
                W("build/o.bin", "y\n"),
                W("keep.txt", "k\n"),
                L("git status -s --ignored"),
                GC("git status --ignored", "ignored.log"),
                GC("git check-ignore ignored.log", "ignored.log"),
                Op::BF("git check-ignore f.txt"),
                Op::BF("git check-ignore keep.txt"),
                G("git add -f ignored.log"),
                L("git status -s --ignored"),
                G("git add -f build"),
                L("git status -s --ignored"),
                G(r#"git commit -m "forced""#),
                L("git status -s --ignored"),
            ],
        ),
        (
            "reflog",
            vec![
                W("f.txt", "1\n"),
                G("git add f.txt"),
                G(r#"git commit -m "one""#),
                W("f.txt", "2\n"),
                G("git add f.txt"),
                G(r#"git commit -m "two""#),
                W("f.txt", "3\n"),
                G("git add f.txt"),
                G(r#"git commit -m "three""#),
                G("git reset --hard HEAD@{1}"),
                // the reset itself is the newest entry, so @{1} is where HEAD was before it
                G("git reset --hard HEAD@{1}"),
                G("git reset --hard HEAD@{2}"),
                G("git reset --soft HEAD@{0}"),
                Op::BF("git reset --hard HEAD@{99}"),
                X("git reset --hard HEAD@{yesterday}", "no vcs equivalent"),
                X("git reset --hard ORIG_HEAD", "no vcs equivalent"),
            ],
        ),
        (
            "refs",
            vec![
                W("f.txt", "1\n"),
                G("git add f.txt"),
                G(r#"git commit -m "one""#),
                W("f.txt", "2\n"),
                G("git add f.txt"),
                G(r#"git commit -m "two""#),
                W("f.txt", "3\n"),
                G("git add f.txt"),
                G(r#"git commit -m "three""#),
                O("git name-rev HEAD"),
                O("git name-rev HEAD~2"),
                O("git name-rev --name-only HEAD~1"),
                G("git tag v1 HEAD~1"),
                O("git name-rev HEAD~1"),
                O("git name-rev HEAD~2"),
                G("git switch -c feature"),
                W("g.txt", "g\n"),
                G("git add g.txt"),
                G(r#"git commit -m "topic one""#),
                G("git switch main"),
                O("git for-each-ref --format=%(refname)"),
                O("git for-each-ref --format=%(refname:short) refs/heads"),
                O("git for-each-ref --format=%(subject) refs/heads"),
                O("git for-each-ref --sort=-refname --format=%(refname)"),
                O("git for-each-ref --count=2 --format=%(refname)"),
                O("git for-each-ref --format=%(HEAD)%(refname:short) refs/heads"),
                GC("git show-ref", "refs/heads/feature"),
                GC("git show-ref --tags", "refs/tags/v1"),
                GC("git show-ref feature", "refs/heads/feature"),
                G("git show-ref --verify -q refs/heads/main"),
                Op::BF("git show-ref --verify -q refs/heads/nope"),
                Op::BF("git show-ref --tags nope"),
                // create, move and delete refs without touching the working tree
                G("git update-ref refs/heads/topic HEAD"),
                O("git for-each-ref --format=%(refname) refs/heads"),
                G("git update-ref -d refs/heads/topic"),
                G("git update-ref refs/heads/copy HEAD"),
                G("git update-ref refs/heads/copy HEAD~1"),
                O("git for-each-ref --format=%(refname)"),
                G("git update-ref -d refs/heads/copy"),
                G("git update-ref refs/tags/v2 HEAD"),
                O("git for-each-ref --format=%(refname) refs/tags"),
                G("git update-ref -d refs/tags/v2"),
                G("git update-ref refs/heads/main HEAD~2"),
                G("git update-ref refs/heads/main feature"),
            ],
        ),
        (
            "clean",
            vec![
                W("f.txt", "1\n"),
                G("git add f.txt"),
                commit(),
                W("junk.txt", "x\n"),
                W("tmp/j.txt", "y\n"),
                GC("git clean -n", "junk.txt"),
                L("git ls-files -o"),
                G("git clean -fd"),
                W(".gitignore", "*.log\n"),
                W(".vcsignore", "*.log\n"),
                G("git add .gitignore .vcsignore"),
                commit(),
                W("a.log", "x\n"),
                W("sub/b.log", "y\n"),
                W("sub/c.txt", "z\n"),
                L("git ls-files -o --exclude-standard"),
                G("git clean -fd"),
                G("git clean -fx"),
                G("git clean -fdx"),
            ],
        ),
        (
            "grep",
            vec![
                W("src/a.txt", "needle one\na.b here\n"),
                W("b.txt", "NEEDLE\n"),
                G("git add ."),
                commit(),
                GC("git grep -n needle", "src/a.txt"),
                GC("git grep -i -e needle HEAD", "b.txt"),
                GC("git grep -F a.b -- src", ""),
                W("c.txt", "staged needle\n"),
                G("git add c.txt"),
                W("c.txt", "on disk only\n"),
                GC("git grep --cached needle", "staged needle"),
                BF("git grep needle -- c.txt"),
            ],
        ),
    ]
}

#[test]
fn every_mapped_row_runs_and_means_the_same_as_git() {
    let git_on = have_git();
    let mut fails = vec![];
    let mut done: BTreeMap<&str, Vec<&'static str>> = BTreeMap::new();
    for (name, ops) in scenarios() {
        done.insert(name, run_scenario(name, &ops, git_on, &mut fails));
    }
    // the table's `test` column must be honest
    let rows = parse_rows();
    let mut covered = 0;
    for (git, status, test) in &rows {
        if test == "-" {
            continue;
        }
        match done.get(test.as_str()) {
            Some(lines) if lines.contains(&git.as_str()) => covered += 1,
            _ => fails.push(format!("row {git:?} says test {test:?} but that scenario never ran it")),
        }
        let _ = status;
    }
    eprintln!(
        "git binary used as yardstick: {git_on}; rows with an end-to-end scenario: {covered}/{}",
        rows.len()
    );
    assert!(fails.is_empty(), "{}", fails.join("\n"));
    let _ = BTreeSet::<u8>::new();
}

/// `(git, status, test)` from the table source (so no second copy to keep in sync).
fn parse_rows() -> Vec<(String, String, String)> {
    let mut out = vec![];
    for l in TABLE.lines() {
        let l = l.trim();
        if !l.starts_with("Row { git: \"") {
            continue;
        }
        let field = |key: &str| -> String {
            let i = l.find(&format!("{key}: \"")).unwrap() + key.len() + 3;
            let mut s = String::new();
            let mut it = l[i..].chars();
            while let Some(c) = it.next() {
                match c {
                    '\\' => s.push(it.next().unwrap()),
                    '"' => break,
                    c => s.push(c),
                }
            }
            s
        };
        let status = l.split("status: Status::").nth(1).unwrap().split(',').next().unwrap().to_owned();
        out.push((field("git"), status, field("test")));
    }
    out
}

#[test]
fn init_and_clone_forms() {
    let t = tempfile::tempdir().unwrap();
    let vcs = |dir: &Path, args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_vcs"))
            .arg("git")
            .args(args)
            .current_dir(dir)
            .env("VCS_AUTHOR_NAME", "Dev")
            .env("VCS_AUTHOR_EMAIL", "dev@example.com")
            .env("VCS_GIT_QUIET", "1")
            .output()
            .unwrap()
    };
    let o = vcs(t.path(), &["init", "proj"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(t.path().join("proj/.vcs").is_dir() && !t.path().join("proj/.git").exists());
    // push / clone / fetch / pull through a directory remote, with clone inferring the directory
    let proj = t.path().join("proj");
    std::fs::write(proj.join("f.txt"), "one\n").unwrap();
    assert!(vcs(&proj, &["add", "f.txt"]).status.success());
    assert!(vcs(&proj, &["commit", "-m", "first"]).status.success());
    let remote = t.path().join("srv/app.git");
    std::fs::create_dir_all(&remote).unwrap();
    let o = vcs(&proj, &["remote", "add", "origin", remote.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&vcs(&proj, &["remote", "-v"]).stdout).contains("app.git"));
    let o = vcs(&proj, &["push", "-u", "origin", "main"]);
    assert!(
        o.status.success(),
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let work = t.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let o = vcs(&work, &["clone", remote.to_str().unwrap()]);
    assert!(
        o.status.success(),
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let cl = work.join("app");
    assert_eq!(std::fs::read_to_string(cl.join("f.txt")).unwrap(), "one\n");
    // a second commit travels through push / fetch / pull
    std::fs::write(proj.join("f.txt"), "one\ntwo\n").unwrap();
    assert!(vcs(&proj, &["commit", "-a", "-m", "second"]).status.success());
    assert!(vcs(&proj, &["push"]).status.success());
    assert!(vcs(&cl, &["fetch"]).status.success());
    assert!(vcs(&cl, &["fetch", "origin"]).status.success());
    let o = vcs(&cl, &["pull"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(std::fs::read_to_string(cl.join("f.txt")).unwrap(), "one\ntwo\n");
    // refusals name the native way
    let o = vcs(&cl, &["pull", "--rebase", "origin", "main"]);
    assert!(!o.status.success());
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(e.contains("no vcs equivalent") && e.contains("native:"), "{e}");
    let o = vcs(&cl, &["submodule", "add", "x", "y"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("vcs zone create"));
    let o = vcs(&cl, &["checkout", "v9"]);
    assert!(!o.status.success());
    let o = vcs(&cl, &["frobnicate"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("unknown to `vcs git`"));
}

#[test]
fn native_reset_clean_stash_apply_switch_c() {
    let t = tempfile::tempdir().unwrap();
    let vcs = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
            .args(args)
            .current_dir(t.path())
            .env("VCS_AUTHOR_NAME", "Dev")
            .env("VCS_AUTHOR_EMAIL", "dev@example.com")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "vcs {args:?}: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    vcs(&["init"]);
    let f = t.path().join("f.txt");
    for (i, c) in ["1\n", "2\n", "3\n"].iter().enumerate() {
        std::fs::write(&f, c).unwrap();
        vcs(&["add", "f.txt"]);
        vcs(&["commit", "-m", &format!("c{i}")]);
    }
    vcs(&["reset", "--soft", "HEAD~1"]);
    assert!(vcs(&["status"]).contains("modified:  f.txt"), "soft keeps the change staged");
    vcs(&["commit", "-m", "again"]);
    vcs(&["reset", "HEAD~1"]);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "3\n", "mixed keeps the file");
    vcs(&["reset", "--hard", "HEAD"]);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "2\n", "hard resets the file");
    vcs(&["switch", "-c", "topic"]);
    std::fs::write(t.path().join("junk"), "j").unwrap();
    assert!(vcs(&["clean", "-n"]).contains("junk"));
    assert!(t.path().join("junk").exists());
    vcs(&["clean", "-f"]);
    assert!(!t.path().join("junk").exists());
    std::fs::write(&f, "stashed\n").unwrap();
    vcs(&["stash", "push"]);
    vcs(&["stash", "apply"]);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "stashed\n");
    assert!(vcs(&["stash", "list"]).contains("stash@{0}"), "apply keeps the entry");
    let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(["clean"])
        .current_dir(t.path())
        .output()
        .unwrap();
    assert!(!o.status.success(), "clean needs -f or -n");
}

#[test]
fn stash_include_untracked_round_trip() {
    let t = tempfile::tempdir().unwrap();
    let vcs = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
            .arg("git")
            .args(args)
            .current_dir(t.path())
            .env("VCS_AUTHOR_NAME", "Dev")
            .env("VCS_AUTHOR_EMAIL", "dev@example.com")
            .env("VCS_GIT_QUIET", "1")
            .output()
            .unwrap();
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    vcs(&["init"]);
    std::fs::write(t.path().join("f.txt"), "1\n").unwrap();
    vcs(&["add", "f.txt"]);
    vcs(&["commit", "-m", "base"]);
    std::fs::write(t.path().join("f.txt"), "2\n").unwrap();
    std::fs::create_dir(t.path().join("sub")).unwrap();
    std::fs::write(t.path().join("sub/n.txt"), "new\n").unwrap();
    vcs(&["stash", "-u"]);
    assert!(!t.path().join("sub/n.txt").exists(), "untracked file is set aside");
    assert_eq!(std::fs::read_to_string(t.path().join("f.txt")).unwrap(), "1\n");
    vcs(&["stash", "pop"]);
    assert_eq!(std::fs::read_to_string(t.path().join("sub/n.txt")).unwrap(), "new\n");
    assert_eq!(std::fs::read_to_string(t.path().join("f.txt")).unwrap(), "2\n");
}

#[test]
fn blame_line_ranges_select_the_lines_git_selects() {
    let t = tempfile::tempdir().unwrap();
    let vcs = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
            .arg("git")
            .args(args)
            .current_dir(t.path())
            .env("VCS_AUTHOR_NAME", "Dev")
            .env("VCS_AUTHOR_EMAIL", "dev@example.com")
            .env("VCS_GIT_QUIET", "1")
            .output()
            .unwrap();
        (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned())
    };
    vcs(&["init"]);
    std::fs::write(t.path().join("m.txt"), "a\nb\nc\nd\ne\nf\n").unwrap();
    vcs(&["add", "m.txt"]);
    vcs(&["commit", "-m", "base"]);
    // expected text per range, as `git blame -L` selects it
    for (range, want) in [
        ("2,3", "bc"),
        ("3", "cdef"),
        ("2,+2", "bc"),
        ("4,-2", "cd"),
        (",2", "ab"),
        ("5,", "ef"),
        ("3,1", "abc"),
    ] {
        let (ok, out) = vcs(&["blame", "-L", range, "m.txt"]);
        assert!(ok, "-L {range}");
        let got: String = out.lines().map(|l| l.chars().last().unwrap()).collect();
        assert_eq!(got, want, "-L {range}: {out}");
    }
    assert!(!vcs(&["blame", "-L", "9,12", "m.txt"]).0, "a range past the end is an error");
}

#[test]
fn cover_letter_has_gits_layout() {
    let t = tempfile::tempdir().unwrap();
    let mut out = vec![];
    for (name, use_git) in [("v", false), ("g", true)] {
        if use_git && !have_git() {
            continue;
        }
        let dir = t.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let mut c = if use_git {
                let mut c = Command::new("git");
                c.args(args);
                c
            } else {
                let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
                c.arg("git").args(args);
                c
            };
            let o = c
                .current_dir(&dir)
                .env("VCS_AUTHOR_NAME", "Dev")
                .env("VCS_AUTHOR_EMAIL", "dev@example.com")
                .env("VCS_GIT_QUIET", "1")
                .env("GIT_AUTHOR_NAME", "Dev")
                .env("GIT_AUTHOR_EMAIL", "dev@example.com")
                .env("GIT_COMMITTER_NAME", "Dev")
                .env("GIT_COMMITTER_EMAIL", "dev@example.com")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).into_owned()
        };
        run(&["init", "-q"]);
        for (f, c, m) in [("a.txt", "1\n", "first"), ("b.txt", "2\n", "second"), ("a.txt", "1\n1\n", "third")] {
            std::fs::write(dir.join(f), c).unwrap();
            run(&["add", f]);
            run(&["commit", "-m", m]);
        }
        out.push(run(&["format-patch", "--cover-letter", "-2", "--stdout"]));
    }
    for mail in &out {
        let cover: &str = mail.split("\nFrom ").next().unwrap();
        let subjects: Vec<&str> = mail.lines().filter(|l| l.starts_with("Subject:")).collect();
        assert_eq!(
            subjects,
            [
                "Subject: [PATCH 0/2] *** SUBJECT HERE ***",
                "Subject: [PATCH 1/2] second",
                "Subject: [PATCH 2/2] third"
            ],
            "{mail}"
        );
        assert!(cover.contains("\n*** BLURB HERE ***\n"), "{cover}");
        assert!(cover.contains("Dev (2):\n  second\n  third\n"), "{cover}");
        assert!(cover.contains(" a.txt |") && cover.contains(" b.txt |"), "{cover}");
        assert!(cover.contains("2 files changed, 2 insertions(+)"), "{cover}");
    }
}

#[test]
fn cat_file_batch_reads_names_from_stdin() {
    use std::io::Write;
    let t = tempfile::tempdir().unwrap();
    let vcs = |args: &[&str], input: &str| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"))
            .arg("git")
            .args(args)
            .current_dir(t.path())
            .env("VCS_AUTHOR_NAME", "Dev")
            .env("VCS_AUTHOR_EMAIL", "dev@example.com")
            .env("VCS_GIT_QUIET", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let o = c.wait_with_output().unwrap();
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    vcs(&["init"], "");
    std::fs::write(t.path().join("f.txt"), "hello\n").unwrap();
    vcs(&["add", "f.txt"], "");
    vcs(&["commit", "-m", "base"], "");
    let out = vcs(&["cat-file", "--batch"], "HEAD:f.txt\nnope\nHEAD\n");
    let mut it = out.lines();
    let head = it.next().unwrap();
    let parts: Vec<&str> = head.split(' ').collect();
    assert_eq!((parts.len(), parts[1], parts[2]), (3, "blob", "6"), "{out}");
    assert_eq!(parts[0].len(), 64);
    assert_eq!(it.next(), Some("hello"));
    assert_eq!(it.next(), Some(""), "the content is followed by a newline of its own");
    assert_eq!(it.next(), Some("nope missing"));
    let c = it.next().unwrap();
    assert!(c.len() > 65 && &c[64..72] == " commit ", "{c}");
    let check = vcs(&["cat-file", "--batch-check"], "HEAD:f.txt\n");
    assert_eq!(check.lines().count(), 1);
    assert!(check.trim_end().ends_with(" blob 6"), "{check}");
}

#[test]
fn init_initial_branch_names_the_first_branch_like_git() {
    let t = tempfile::tempdir().unwrap();
    let vcs = |dir: &Path, args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_vcs"))
            .arg("git")
            .args(args)
            .current_dir(dir)
            .env("VCS_AUTHOR_NAME", "Dev")
            .env("VCS_AUTHOR_EMAIL", "dev@example.com")
            .env("VCS_GIT_QUIET", "1")
            .output()
            .unwrap()
    };
    let o = vcs(t.path(), &["init", "-b", "trunk", "proj"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let proj = t.path().join("proj");
    std::fs::write(proj.join("f.txt"), "1\n").unwrap();
    assert!(vcs(&proj, &["add", "f.txt"]).status.success());
    assert!(vcs(&proj, &["commit", "-m", "first"]).status.success());
    let o = vcs(&proj, &["branch", "--show-current"]);
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "trunk");
    let o = vcs(&proj, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "trunk");
    assert!(
        !vcs(t.path(), &["init", "-b", "bad name", "other"]).status.success(),
        "invalid branch names are refused"
    );
    if have_git() {
        let g = t.path().join("g");
        assert!(Command::new("git")
            .args(["init", "-q", "-b", "trunk"])
            .arg(&g)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap()
            .success());
        let o = Command::new("git")
            .args(["symbolic-ref", "--short", "HEAD"])
            .current_dir(&g)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "trunk");
    }
}

/// `pull --rebase` and `rebase origin/<branch>`: local commits are replayed on top of the fetched
/// ones. Run with vcs and (when available) with real git against a twin remote; the histories must agree.
#[test]
fn pull_rebase_replays_local_commits_on_the_fetched_branch() {
    let t = tempfile::tempdir().unwrap();
    let git_on = have_git();
    let mut logs = vec![];
    for use_git in [false, true] {
        if use_git && !git_on {
            continue;
        }
        let root = t.path().join(if use_git { "g" } else { "v" });
        std::fs::create_dir_all(&root).unwrap();
        let remote = root.join("srv.git");
        let run = |dir: &Path, args: &[&str]| -> Output {
            let mut c = if use_git {
                let mut c = Command::new("git");
                c.args(args);
                c
            } else {
                let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
                c.arg("git").args(args);
                c
            };
            c.current_dir(dir)
                .env("VCS_AUTHOR_NAME", "Dev")
                .env("VCS_AUTHOR_EMAIL", "dev@example.com")
                .env("VCS_GIT_QUIET", "1")
                .env("GIT_AUTHOR_NAME", "Dev")
                .env("GIT_AUTHOR_EMAIL", "dev@example.com")
                .env("GIT_COMMITTER_NAME", "Dev")
                .env("GIT_COMMITTER_EMAIL", "dev@example.com")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap()
        };
        let ok = |dir: &Path, args: &[&str]| {
            let o = run(dir, args);
            assert!(
                o.status.success(),
                "{} {args:?}: {}{}",
                if use_git { "git" } else { "vcs" },
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            String::from_utf8_lossy(&o.stdout).into_owned()
        };
        if use_git {
            std::fs::create_dir_all(&remote).unwrap();
            ok(&remote, &["init", "-q", "--bare"]);
            ok(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        } else {
            std::fs::create_dir_all(&remote).unwrap();
        }
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        if use_git {
            ok(&a, &["init", "-q"]);
            ok(&a, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        } else {
            ok(&a, &["init"]);
        }
        let commit = |dir: &Path, f: &str, c: &str, m: &str| {
            std::fs::write(dir.join(f), c).unwrap();
            ok(dir, &["add", f]);
            ok(dir, &["commit", "-m", m]);
        };
        commit(&a, "base.txt", "1\n", "base");
        ok(&a, &["remote", "add", "origin", remote.to_str().unwrap()]);
        ok(&a, &["push", "-u", "origin", "main"]);
        std::fs::create_dir_all(&b).unwrap();
        ok(&root, &["clone", remote.to_str().unwrap(), "b"]);
        // upstream moves on in a; b has two local commits
        commit(&a, "up.txt", "u\n", "upstream");
        ok(&a, &["push"]);
        commit(&b, "l1.txt", "1\n", "local one");
        commit(&b, "l2.txt", "2\n", "local two");
        ok(&b, &["pull", "--rebase"]);
        logs.push(ok(&b, &["log", "--format=%s"]).lines().map(str::to_owned).collect::<Vec<_>>());
        for f in ["base.txt", "up.txt", "l1.txt", "l2.txt"] {
            assert!(b.join(f).exists(), "{f} after pull --rebase");
        }
        // again: the local commits are replayed once more, on top of the newest upstream
        commit(&a, "up2.txt", "u\n", "upstream two");
        ok(&a, &["push"]);
        ok(&b, &["pull", "--rebase"]);
        assert!(b.join("up2.txt").exists());
        let l: Vec<String> = ok(&b, &["log", "--format=%s"]).lines().map(str::to_owned).collect();
        assert_eq!(l[..3], ["local two", "local one", "upstream two"], "{l:?}");
        // a conflict stops the rebase with the usual workflow
        commit(&a, "c.txt", "from a\n", "a side");
        ok(&a, &["push"]);
        std::fs::write(b.join("c.txt"), "from b\n").unwrap();
        ok(&b, &["add", "c.txt"]);
        ok(&b, &["commit", "-m", "b side"]);
        assert!(
            !run(&b, &["pull", "--rebase"]).status.success(),
            "a conflicting pull --rebase stops"
        );
    }
    assert_eq!(logs[0], ["local two", "local one", "upstream", "base"], "{logs:?}");
    if logs.len() == 2 {
        assert_eq!(logs[0], logs[1], "vcs and git order the rebased history alike");
    }
}

#[test]
fn config_system_is_the_lowest_layer() {
    let t = tempfile::tempdir().unwrap();
    let sys = t.path().join("etc-vcs-config");
    let home = t.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let vcs = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_vcs"))
            .args(args)
            .current_dir(t.path())
            .env("VCS_SYSTEM_CONFIG", &sys)
            .env("VCS_CONFIG_HOME", &home)
            .env_remove("VCS_AUTHOR_NAME")
            .env_remove("VCS_AUTHOR_EMAIL")
            .output()
            .unwrap()
    };
    let out = |args: &[&str]| {
        let o = vcs(args);
        assert!(
            o.status.success(),
            "{args:?}: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_owned()
    };
    out(&["git", "config", "--system", "user.name", "SysDev"]);
    assert!(std::fs::read_to_string(&sys).unwrap().contains("user.name"));
    assert_eq!(out(&["config", "get", "user.name"]), "SysDev");
    assert!(out(&["config", "list"]).contains("user.name=SysDev  (system)"));
    // the user layer wins over the system one, and unsetting the user layer shows the system value again
    out(&["git", "config", "--global", "user.name", "Me"]);
    assert_eq!(out(&["config", "get", "user.name"]), "Me");
    out(&["git", "config", "--global", "--unset", "user.name"]);
    assert_eq!(out(&["config", "get", "user.name"]), "SysDev");
    out(&["config", "unset", "user.name", "--system"]);
    assert!(!vcs(&["config", "get", "user.name"]).status.success());
    assert!(
        !vcs(&["config", "set", "user.name", "x", "--user", "--system"]).status.success(),
        "one scope at a time"
    );
}

/// A runner for one side (`vcs git ...` or real git) with a fixed identity and quiet notes.
fn side_runner(use_git: bool) -> impl Fn(&Path, &[&str]) -> Output {
    move |dir: &Path, args: &[&str]| {
        let mut c = if use_git {
            let mut c = Command::new("git");
            c.args(args);
            c
        } else {
            let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
            c.arg("git").args(args);
            c
        };
        c.current_dir(dir)
            .env("VCS_AUTHOR_NAME", "Dev")
            .env("VCS_AUTHOR_EMAIL", "dev@example.com")
            .env("VCS_GIT_QUIET", "1")
            .env("GIT_AUTHOR_NAME", "Dev")
            .env("GIT_AUTHOR_EMAIL", "dev@example.com")
            .env("GIT_COMMITTER_NAME", "Dev")
            .env("GIT_COMMITTER_EMAIL", "dev@example.com")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_EDITOR", "true")
            .output()
            .unwrap()
    }
}

fn stdout_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// `write-tree` + `commit-tree` + `update-ref` build a commit by hand; the result is a branch whose log
/// and files are what git gives for the same steps. `hash-object` and `count-objects` answer too.
#[test]
fn write_tree_commit_tree_update_ref_build_a_commit_like_git() {
    let t = tempfile::tempdir().unwrap();
    let mut results = vec![];
    for use_git in [false, true] {
        if use_git && !have_git() {
            continue;
        }
        let run = side_runner(use_git);
        let dir = t.path().join(if use_git { "g" } else { "v" });
        std::fs::create_dir_all(&dir).unwrap();
        let ok = |args: &[&str]| {
            let o = run(&dir, args);
            assert!(
                o.status.success(),
                "{} {args:?}: {}{}",
                if use_git { "git" } else { "vcs" },
                stdout_of(&o),
                String::from_utf8_lossy(&o.stderr)
            );
            stdout_of(&o).trim().to_owned()
        };
        if use_git {
            ok(&["init", "-q"]);
            ok(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        } else {
            ok(&["init"]);
        }
        std::fs::write(dir.join("f.txt"), "one\n").unwrap();
        ok(&["add", "f.txt"]);
        ok(&["commit", "-m", "base"]);
        std::fs::write(dir.join("f.txt"), "two\n").unwrap();
        std::fs::write(dir.join("g.txt"), "g\n").unwrap();
        ok(&["add", "f.txt", "g.txt"]);
        let tree = ok(&["write-tree"]);
        assert!(tree.len() >= 40 && tree.bytes().all(|b| b.is_ascii_hexdigit()), "{tree}");
        let commit = ok(&["commit-tree", &tree, "-p", "HEAD", "-m", "made by hand"]);
        assert!(commit.len() >= 40, "{commit}");
        // nothing references it yet
        assert_eq!(ok(&["log", "--format=%s"]).lines().collect::<Vec<_>>(), ["base"]);
        ok(&["update-ref", "refs/heads/hand", &commit]);
        assert_eq!(
            ok(&["log", "--format=%s", "hand"]).lines().collect::<Vec<_>>(),
            ["made by hand", "base"]
        );
        // the old value is checked: a stale one is refused and moves nothing
        let head = ok(&["rev-parse", "HEAD"]);
        assert!(!run(&dir, &["update-ref", "refs/heads/hand", &head, &head]).status.success());
        ok(&["update-ref", "refs/heads/hand", &head, &commit]);
        assert_eq!(ok(&["log", "--format=%s", "hand"]).lines().collect::<Vec<_>>(), ["base"]);
        // a tree can be written again and is the same tree
        assert_eq!(ok(&["write-tree"]), tree);
        results.push((
            ok(&["show-ref", "-s", "--heads", "main"]).len() >= 40,
            ok(&["for-each-ref", "--format=%(refname)", "refs/heads"]),
        ));
    }
    if results.len() == 2 {
        assert_eq!(results[0], results[1]);
    }
}

#[test]
fn hash_object_and_count_objects_answer_for_the_vcs_store() {
    let t = tempfile::tempdir().unwrap();
    let run = side_runner(false);
    let ok = |args: &[&str]| {
        let o = run(t.path(), args);
        assert!(
            o.status.success(),
            "{args:?}: {}{}",
            stdout_of(&o),
            String::from_utf8_lossy(&o.stderr)
        );
        stdout_of(&o).trim().to_owned()
    };
    ok(&["init"]);
    std::fs::write(t.path().join("f.txt"), "hello\n").unwrap();
    let id = ok(&["hash-object", "f.txt"]);
    assert_eq!(id.len(), 64, "{id}");
    // -w stores it: the same id, readable with cat-file, and the same as `add` stores
    assert_eq!(ok(&["hash-object", "-w", "f.txt"]), id);
    assert_eq!(ok(&["cat-file", "-p", &id]), "hello");
    ok(&["add", "f.txt"]);
    assert!(ok(&["ls-files", "-s"]).contains(&id), "add stores the id hash-object printed");
    // stdin
    let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(["git", "hash-object", "--stdin"])
        .current_dir(t.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.take().unwrap().write_all(b"hello\n")?;
            c.wait_with_output()
        })
        .unwrap();
    assert_eq!(stdout_of(&o).trim(), id);
    // count-objects
    assert!(ok(&["count-objects"]).contains(" objects, "));
    let v = ok(&["count-objects", "-v"]);
    assert!(v.contains("count: ") && v.contains("size-pack: "), "{v}");
    assert!(!run(t.path(), &["hash-object", "-t", "tree", "f.txt"]).status.success());
    // the plumbing that has no faithful meaning is refused with the way out
    for c in ["update-index", "read-tree", "pack-objects", "check-attr", "var", "mktag"] {
        let o = run(t.path(), &[c]);
        assert!(!o.status.success());
        assert!(String::from_utf8_lossy(&o.stderr).contains("no vcs equivalent"), "{c}");
    }
}

/// `worktree prune` forgets a worktree whose directory was deleted by hand, and only that one.
#[test]
fn worktree_prune_forgets_only_vanished_worktrees() {
    let t = tempfile::tempdir().unwrap();
    let run = side_runner(false);
    let main = t.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    let ok = |dir: &Path, args: &[&str]| {
        let o = run(dir, args);
        assert!(
            o.status.success(),
            "{args:?}: {}{}",
            stdout_of(&o),
            String::from_utf8_lossy(&o.stderr)
        );
        stdout_of(&o)
    };
    ok(&main, &["init"]);
    std::fs::write(main.join("f.txt"), "1\n").unwrap();
    ok(&main, &["add", "f.txt"]);
    ok(&main, &["commit", "-m", "base"]);
    let (keep, gone) = (t.path().join("keep"), t.path().join("gone"));
    ok(&main, &["worktree", "add", keep.to_str().unwrap(), "keepb"]);
    ok(&main, &["worktree", "add", gone.to_str().unwrap(), "goneb"]);
    assert!(ok(&main, &["worktree", "prune"]).contains("nothing to prune"));
    std::fs::remove_dir_all(&gone).unwrap();
    let dry = ok(&main, &["worktree", "prune", "-n"]);
    assert!(dry.contains("would prune") && dry.contains("gone"), "{dry}");
    assert!(ok(&main, &["worktree", "list"]).contains("gone"), "a dry run forgets nothing");
    let done = ok(&main, &["worktree", "prune"]);
    assert!(done.contains("pruned") && done.contains("gone"), "{done}");
    let list = ok(&main, &["worktree", "list"]);
    assert!(!list.contains("gone") && list.contains("keep"), "{list}");
    assert!(keep.join("f.txt").exists(), "the live worktree is untouched");
    assert!(ok(&main, &["worktree", "prune"]).contains("nothing to prune"));
    // the real git agrees on the shape of the commands
    if have_git() {
        let g = t.path().join("g");
        std::fs::create_dir_all(&g).unwrap();
        let grun = side_runner(true);
        let gok = |args: &[&str]| assert!(grun(&g, args).status.success(), "{args:?}");
        gok(&["init", "-q"]);
        std::fs::write(g.join("f.txt"), "1\n").unwrap();
        gok(&["add", "f.txt"]);
        gok(&["commit", "-q", "-m", "base"]);
        let w = t.path().join("gw");
        gok(&["worktree", "add", "-q", "-b", "gb", w.to_str().unwrap()]);
        std::fs::remove_dir_all(&w).unwrap();
        gok(&["worktree", "prune"]);
        assert!(!stdout_of(&grun(&g, &["worktree", "list"])).contains("gw"));
    }
}

/// `bisect log` lists the marks in order and `bisect replay` redoes them to the same answer; real git
/// finds the same first bad commit for the same history.
#[test]
fn bisect_log_and_replay_reach_the_same_first_bad_commit() {
    let t = tempfile::tempdir().unwrap();
    let mut firsts = vec![];
    for use_git in [false, true] {
        if use_git && !have_git() {
            continue;
        }
        let run = side_runner(use_git);
        let dir = t.path().join(if use_git { "g" } else { "v" });
        std::fs::create_dir_all(&dir).unwrap();
        let ok = |args: &[&str]| {
            let o = run(&dir, args);
            assert!(
                o.status.success(),
                "{} {args:?}: {}{}",
                if use_git { "git" } else { "vcs" },
                stdout_of(&o),
                String::from_utf8_lossy(&o.stderr)
            );
            stdout_of(&o)
        };
        if use_git {
            ok(&["init", "-q"]);
            ok(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        } else {
            ok(&["init"]);
        }
        for i in 1..=8 {
            std::fs::write(dir.join("f.txt"), format!("v{i}\n")).unwrap();
            if i >= 5 {
                std::fs::write(dir.join("bug.txt"), "bug\n").unwrap();
                ok(&["add", "bug.txt"]);
            }
            ok(&["add", "f.txt"]);
            ok(&["commit", "-q", "-m", &format!("c{i}")]);
        }
        // c5 introduced bug.txt; mark by file presence
        let mark = |ok: &dyn Fn(&[&str]) -> String| {
            let mut guard = 0;
            let mut trail = String::new();
            loop {
                guard += 1;
                assert!(
                    guard < 10,
                    "{} bisect did not finish:\n{trail}",
                    if use_git { "git" } else { "vcs" }
                );
                let verb = if dir.join("bug.txt").exists() { "bad" } else { "good" };
                let out = ok(&["bisect", verb]);
                trail.push_str(&format!("-- {verb}: {out}\n"));
                if out.contains("first bad commit") {
                    return out;
                }
            }
        };
        ok(&["bisect", "start", "HEAD", "HEAD~7"]);
        let out = mark(&ok);
        let subject = |rev: &str| {
            ok(&["cat-file", "-p", rev])
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_owned()
        };
        let first = out
            .lines()
            .find(|l| l.contains("is the first bad commit"))
            .map(|l| l.split(' ').next().unwrap().to_owned())
            .unwrap();
        let first_subject = subject(&first);
        assert_eq!(first_subject, "c5");
        let log = ok(&["bisect", "log"]);
        let verbs: Vec<String> = log
            .lines()
            .filter(|l| l.starts_with("git bisect ") || l.starts_with("vcs bisect "))
            .map(|l| l.split_whitespace().nth(2).unwrap().to_owned())
            .collect();
        assert_eq!(verbs.first().map(String::as_str), Some("start"), "{log}");
        assert!(verbs.len() >= 4, "{log}");
        if !use_git {
            assert!(log.contains("# first bad commit"), "{log}");
        }
        // replay: end the bisection, redo the log, same answer
        std::fs::write(dir.join("x.log"), &log).unwrap();
        ok(&["bisect", "reset"]);
        let o = run(&dir, &["bisect", "replay", "x.log"]);
        if use_git {
            // git's own log replays in git
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        } else {
            assert!(o.status.success(), "{}{}", stdout_of(&o), String::from_utf8_lossy(&o.stderr));
            assert!(stdout_of(&o).contains(&first), "{}", stdout_of(&o));
            // replay needs a clean slate
            assert!(!run(&dir, &["bisect", "replay", "x.log"]).status.success());
        }
        ok(&["bisect", "reset"]);
        firsts.push(first_subject);
    }
    if firsts.len() == 2 {
        assert_eq!(firsts[0], firsts[1]);
    }
}

/// `ls-remote` reads the remote's refs now (not the ones fetched earlier) and filters like git.
#[test]
fn ls_remote_lists_the_remote_refs_live() {
    let t = tempfile::tempdir().unwrap();
    let run = side_runner(false);
    let ok = |dir: &Path, args: &[&str]| {
        let o = run(dir, args);
        assert!(
            o.status.success(),
            "{args:?}: {}{}",
            stdout_of(&o),
            String::from_utf8_lossy(&o.stderr)
        );
        stdout_of(&o)
    };
    let (a, remote) = (t.path().join("a"), t.path().join("srv"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&remote).unwrap();
    ok(&a, &["init"]);
    std::fs::write(a.join("f.txt"), "1\n").unwrap();
    ok(&a, &["add", "f.txt"]);
    ok(&a, &["commit", "-m", "base"]);
    ok(&a, &["remote", "add", "origin", remote.to_str().unwrap()]);
    ok(&a, &["push", "-u", "origin", "main"]);
    let head = ok(&a, &["rev-parse", "HEAD"]).trim().to_owned();
    let l = ok(&a, &["ls-remote", "origin"]);
    assert!(l.contains(&format!("{head}\trefs/heads/main")), "{l}");
    // a branch published later shows at once, without a fetch
    ok(&a, &["switch", "-c", "topic"]);
    std::fs::write(a.join("g.txt"), "g\n").unwrap();
    ok(&a, &["add", "g.txt"]);
    ok(&a, &["commit", "-m", "topic"]);
    ok(&a, &["push", "origin", "topic"]);
    let l = ok(&a, &["ls-remote", "--heads", remote.to_str().unwrap()]);
    assert!(l.contains("refs/heads/main") && l.contains("refs/heads/topic"), "{l}");
    let l = ok(&a, &["ls-remote", "origin", "topic"]);
    assert!(l.contains("refs/heads/topic") && !l.contains("refs/heads/main"), "{l}");
    assert!(ok(&a, &["ls-remote", "--tags", "origin"]).trim().is_empty());
}

/// `apply --cached` stages the patch and leaves the file alone; a patch that does not fit the STAGED
/// content is refused although it fits the working tree.
#[test]
fn apply_cached_checks_the_staged_content_and_leaves_the_file() {
    let t = tempfile::tempdir().unwrap();
    let run = side_runner(false);
    let ok = |args: &[&str]| {
        let o = run(t.path(), args);
        assert!(
            o.status.success(),
            "{args:?}: {}{}",
            stdout_of(&o),
            String::from_utf8_lossy(&o.stderr)
        );
        stdout_of(&o)
    };
    ok(&["init"]);
    std::fs::write(t.path().join("f.txt"), "a\nb\nc\n").unwrap();
    ok(&["add", "f.txt"]);
    ok(&["commit", "-m", "base"]);
    let patch = "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n";
    std::fs::write(t.path().join("x.patch"), patch).unwrap();
    ok(&["apply", "--cached", "x.patch"]);
    assert_eq!(
        std::fs::read_to_string(t.path().join("f.txt")).unwrap(),
        "a\nb\nc\n",
        "the file is untouched"
    );
    assert!(ok(&["diff", "--staged"]).contains("+B"), "the index holds the patched content");
    // the same patch again no longer fits the staged content
    assert!(!run(t.path(), &["apply", "--cached", "x.patch"]).status.success());
    assert!(!run(t.path(), &["apply", "--cached", "--index", "x.patch"]).status.success());
    // --check changes nothing
    ok(&["restore", "--staged", "f.txt"]);
    ok(&["apply", "--cached", "--check", "x.patch"]);
    assert!(!ok(&["diff", "--staged"]).contains("+B"));
}
