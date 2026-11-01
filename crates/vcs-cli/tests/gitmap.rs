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
                        "HEAD".to_owned()
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
                X("git diff --exit-code", "no vcs equivalent"),
                X("git add -p", "no vcs equivalent"),
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
                G("git tag -d v0"),
                G("git tag -a v2 -m rel"),
                GC("git describe --tags", "v"),
                G("git describe --always"),
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
                G("git clean -fd"),
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
    let o = vcs(&cl, &["pull", "--rebase"]);
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
