//! `add -p`, `restore -p [--staged]`, `checkout -p [<rev>]`, `stash push -p` and `mergetool`.
//!
//! Every `-p` scenario feeds the same answers to vcs and, when a `git` binary is on the PATH, to real
//! git in a twin repository; the working tree and what the index would commit must agree. The
//! expected texts are also written out by hand, so the scenarios mean something without git.
//! vcs never calls git: it is only the yardstick here.
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn have_git() -> bool {
    Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
}

fn spawn(mut c: Command, stdin: &str) -> Output {
    let mut child = c
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(stdin.as_bytes());
    child.wait_with_output().unwrap()
}

fn vcs_in(dir: &Path, args: &[&str], stdin: &str) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
    c.args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com");
    spawn(c, stdin)
}

fn git_in(dir: &Path, args: &[&str], stdin: &str) -> Output {
    let mut c = Command::new("git");
    c.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Dev")
        .env("GIT_AUTHOR_EMAIL", "dev@example.com")
        .env("GIT_COMMITTER_NAME", "Dev")
        .env("GIT_COMMITTER_EMAIL", "dev@example.com");
    spawn(c, stdin)
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

type Files = BTreeMap<String, String>;
type Observed = (Files, Files, Option<(Files, Files)>);

struct Twin {
    _t: tempfile::TempDir,
    v: PathBuf,
    g: Option<PathBuf>,
    /// extra environment for both tools (editor)
    editor: Option<PathBuf>,
}

impl Twin {
    fn new() -> Twin {
        let t = tempfile::tempdir().unwrap();
        let v = t.path().join("v");
        std::fs::create_dir_all(&v).unwrap();
        assert!(vcs_in(&v, &["init"], "").status.success());
        let g = have_git().then(|| {
            let g = t.path().join("g");
            std::fs::create_dir_all(&g).unwrap();
            assert!(git_in(&g, &["init", "-q", "-b", "main"], "").status.success());
            g
        });
        Twin { _t: t, v, g, editor: None }
    }

    fn write(&self, f: &str, c: &str) {
        for d in std::iter::once(&self.v).chain(self.g.iter()) {
            let p = d.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        }
    }

    fn remove(&self, f: &str) {
        for d in std::iter::once(&self.v).chain(self.g.iter()) {
            std::fs::remove_file(d.join(f)).unwrap();
        }
    }

    fn commit(&self, msg: &str) {
        assert!(vcs_in(&self.v, &["add", "."], "").status.success());
        let o = vcs_in(&self.v, &["commit", "-m", msg], "");
        assert!(o.status.success(), "{}", text(&o));
        if let Some(g) = &self.g {
            assert!(git_in(g, &["add", "."], "").status.success());
            let o = git_in(g, &["commit", "-q", "-m", msg], "");
            assert!(o.status.success(), "{}", text(&o));
        }
    }

    /// Run `vcs <v...>` and `git <g...>` with the same answers; both must succeed.
    fn run(&self, v: &str, g: &str, answers: &str) -> String {
        let va: Vec<&str> = v.split_whitespace().collect();
        let ov = if let Some(ed) = &self.editor {
            let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
            c.args(&va)
                .current_dir(&self.v)
                .env("VCS_AUTHOR_NAME", "Dev")
                .env("VCS_AUTHOR_EMAIL", "dev@example.com")
                .env("VCS_EDITOR", ed);
            spawn(c, answers)
        } else {
            vcs_in(&self.v, &va, answers)
        };
        assert!(ov.status.success(), "vcs {v} failed:\n{}", text(&ov));
        if let Some(gd) = &self.g {
            let ga: Vec<&str> = g.split_whitespace().collect();
            let mut c = Command::new("git");
            c.args(&ga)
                .current_dir(gd)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_AUTHOR_NAME", "Dev")
                .env("GIT_AUTHOR_EMAIL", "dev@example.com")
                .env("GIT_COMMITTER_NAME", "Dev")
                .env("GIT_COMMITTER_EMAIL", "dev@example.com");
            if let Some(ed) = &self.editor {
                c.env("GIT_EDITOR", ed);
            }
            let og = spawn(c, answers);
            assert!(og.status.success(), "git {g} failed:\n{}", text(&og));
        }
        text(&ov)
    }

    /// Working-tree files, then (after committing what is staged) the committed files.
    fn observe(&self) -> Observed {
        fn tree(d: &Path) -> BTreeMap<String, String> {
            fn walk(base: &Path, d: &Path, out: &mut BTreeMap<String, String>) {
                for e in std::fs::read_dir(d).unwrap() {
                    let p = e.unwrap().path();
                    let n = p.file_name().unwrap().to_string_lossy().into_owned();
                    if n == ".git" || n == ".vcs" {
                        continue;
                    }
                    if p.is_dir() {
                        walk(base, &p, out);
                    } else {
                        out.insert(
                            p.strip_prefix(base).unwrap().display().to_string(),
                            std::fs::read_to_string(&p).unwrap(),
                        );
                    }
                }
            }
            let mut m = BTreeMap::new();
            walk(d, d, &mut m);
            m
        }
        let committed = |d: &Path, git: bool| -> BTreeMap<String, String> {
            if git {
                let _ = git_in(d, &["commit", "-q", "-m", "probe"], "");
            } else {
                let _ = vcs_in(d, &["commit", "-m", "probe"], "");
            }
            let list = if git {
                git_in(d, &["ls-files"], "")
            } else {
                vcs_in(d, &["ls-files"], "")
            };
            let mut m = BTreeMap::new();
            for f in String::from_utf8_lossy(&list.stdout).lines() {
                let o = if git {
                    git_in(d, &["show", &format!("HEAD:{f}")], "")
                } else {
                    vcs_in(d, &["cat-file", "-p", &format!("HEAD:{f}")], "")
                };
                m.insert(f.to_owned(), String::from_utf8_lossy(&o.stdout).into_owned());
            }
            m
        };
        let vw = tree(&self.v);
        let vc = committed(&self.v, false);
        let g = self.g.as_ref().map(|g| (tree(g), committed(g, true)));
        (vw, vc, g)
    }

    /// Assert the working tree and what is staged (as committed next) against the hand-written
    /// expectation, and against git.
    fn expect(&self, work: &str, staged: &str) {
        let (vw, vc, g) = self.observe();
        assert_eq!(vw.get("f.txt").map(String::as_str), Some(work), "vcs working tree");
        assert_eq!(vc.get("f.txt").map(String::as_str), Some(staged), "vcs staged/committed");
        if let Some((gw, gc)) = g {
            assert_eq!(gw, vw, "git and vcs working trees differ");
            assert_eq!(gc, vc, "git and vcs staged content differ");
        }
    }
}

fn nums(n: usize) -> String {
    (1..=n).map(|i| format!("l{i}\n")).collect()
}

/// `nums(n)` with the given lines (1-based) upper-cased.
fn with(base: &str, changed: &[usize]) -> String {
    let mut s = base.to_owned();
    for i in changed {
        s = s.replacen(&format!("l{i}\n"), &format!("L{i}\n"), 1);
    }
    s
}

fn pair(old: &str, new: &str) -> Twin {
    let t = Twin::new();
    t.write("f.txt", old);
    t.write("other.txt", "keep\n");
    t.commit("base");
    t.write("f.txt", new);
    t
}

#[test]
fn add_p_yes_no_per_hunk() {
    let old = nums(30);
    let new = with(&old, &[2, 15, 28]);
    let t = pair(&old, &new);
    t.run("add -p f.txt", "add -p f.txt", "y\nn\ny\n");
    t.expect(&new, &with(&old, &[2, 28]));
}

#[test]
fn add_p_quit_all_and_done() {
    let old = nums(30);
    let new = with(&old, &[2, 15, 28]);
    for (answers, staged) in [
        ("n\nq\n", with(&old, &[])),
        ("y\nq\n", with(&old, &[2])),
        ("n\na\n", with(&old, &[15, 28])),
        ("y\nd\n", with(&old, &[2])),
        ("y\n", with(&old, &[2])), // end of input quits
    ] {
        let t = pair(&old, &new);
        t.run("add -p f.txt", "add -p f.txt", answers);
        t.expect(&new, &staged);
    }
}

#[test]
fn add_p_split_hunk() {
    let old = nums(12);
    let new = with(&old, &[3, 6, 9]); // close together: one hunk of three changes
    let t = pair(&old, &new);
    t.run("add -p f.txt", "add -p f.txt", "s\nn\ny\ny\n");
    t.expect(&new, &with(&old, &[6, 9]));
    let t = pair(&old, &new);
    let out = t.run("add -p f.txt", "add -p f.txt", "s\ny\nn\nn\n");
    assert!(out.contains("Split into 3 hunks"), "{out}");
    t.expect(&new, &with(&old, &[3]));
}

#[test]
fn add_p_without_a_path_goes_through_every_changed_file() {
    let old = nums(5);
    let t = pair(&old, &with(&old, &[3]));
    t.write("other.txt", "changed\n");
    let out = t.run("add -p", "add -p", "y\ny\n");
    assert!(out.contains("a/f.txt") && out.contains("a/other.txt"), "{out}");
    let (_, vc, _) = t.observe();
    assert_eq!(vc["f.txt"], with(&old, &[3]));
    assert_eq!(vc["other.txt"], "changed\n");
}

#[test]
fn add_p_edit_hunk() {
    let old = "a\nb\nc\n";
    let new = "a\nB\nB2\nc\n";
    let mut t = pair(old, new);
    // the editor drops the "+B2" line
    let ed = t._t.path().join("ed.sh");
    std::fs::write(&ed, "#!/bin/sh\ngrep -v '^+B2' \"$1\" > \"$1.n\" && mv \"$1.n\" \"$1\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ed, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    t.editor = Some(ed);
    t.run("add -p f.txt", "add -p f.txt", "e\n");
    t.expect(new, "a\nB\nc\n");
}

#[test]
fn add_p_new_line_at_end_without_newline() {
    let t = pair("a\nb", "a\nb\nc\n");
    t.run("add -p f.txt", "add -p f.txt", "y\n");
    t.expect("a\nb\nc\n", "a\nb\nc\n");
}

#[test]
fn add_p_deleted_file_and_untracked_is_not_offered() {
    let t = pair("a\nb\n", "a\nb\n");
    t.remove("f.txt");
    t.write("new.txt", "untracked\n");
    let out = t.run("add -p", "add -p", "y\n");
    assert!(out.contains("a/f.txt"), "{out}");
    assert!(!out.contains("new.txt"), "{out}");
    let (_, vc, _) = t.observe();
    assert!(!vc.contains_key("f.txt"), "{vc:?}");
}

#[test]
fn restore_p_discards_chosen_hunks_from_the_working_file() {
    let old = nums(30);
    let new = with(&old, &[2, 15, 28]);
    let t = pair(&old, &new);
    t.run("restore -p f.txt", "restore -p f.txt", "y\nn\ny\n");
    t.expect(&with(&old, &[15]), &old);
}

#[test]
fn restore_staged_p_unstages_chosen_hunks() {
    let old = nums(30);
    let new = with(&old, &[2, 15, 28]);
    let t = pair(&old, &new);
    t.run("add f.txt", "add f.txt", "");
    t.run("restore --staged -p f.txt", "restore --staged -p f.txt", "n\ny\nn\n");
    t.expect(&new, &with(&old, &[2, 28]));
}

#[test]
fn checkout_p_discards_like_restore_p() {
    let old = nums(30);
    let new = with(&old, &[2, 15, 28]);
    let t = pair(&old, &new);
    t.run("checkout -p -- f.txt", "checkout -p -- f.txt", "n\ny\ny\n");
    t.expect(&with(&old, &[2]), &old);
}

#[test]
fn checkout_p_rev_changes_index_and_working_tree() {
    let old = nums(30);
    let t = Twin::new();
    t.write("f.txt", &old);
    t.commit("one");
    let mid = with(&old, &[2, 15, 28]);
    t.write("f.txt", &mid);
    t.commit("two");
    // working tree: further edit of another line, committed state is the index
    t.run("checkout -p HEAD~1 -- f.txt", "checkout -p HEAD~1 -- f.txt", "y\nn\ny\n");
    t.expect(&with(&old, &[15]), &with(&old, &[15]));
}

#[test]
fn checkout_p_rev_refuses_when_index_and_working_tree_differ() {
    let old = nums(30);
    let t = Twin::new();
    t.write("f.txt", &old);
    t.commit("one");
    t.write("f.txt", &with(&old, &[2]));
    t.commit("two");
    t.write("f.txt", &with(&old, &[2, 28]));
    let o = vcs_in(&t.v, &["checkout", "-p", "HEAD~1", "--", "f.txt"], "y\ny\n");
    assert!(!o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("index and the working tree differ"), "{}", text(&o));
    assert_eq!(std::fs::read_to_string(t.v.join("f.txt")).unwrap(), with(&old, &[2, 28]));
}

#[test]
fn stash_p_keeps_the_unselected_hunks_in_the_working_tree() {
    let old = nums(30);
    let new = with(&old, &[2, 15, 28]);
    let t = pair(&old, &new);
    t.write("other.txt", "edited\n");
    t.run("stash push -p", "stash push -p", "y\nn\nn\nn\n");
    // the stashed hunk (line 2) is gone from the file; line 15 and 28 and other.txt stay
    let (vw, _, g) = t.observe_work_only();
    assert_eq!(vw["f.txt"], with(&old, &[15, 28]));
    assert_eq!(vw["other.txt"], "edited\n");
    if let Some(gw) = g {
        assert_eq!(gw, vw);
    }
    // popping (vcs wants a clean tree) brings the stashed hunk back
    t.run("restore f.txt other.txt", "restore f.txt other.txt", "");
    t.run("stash pop", "stash pop", "");
    let (vw, _, g) = t.observe_work_only();
    assert_eq!(vw["f.txt"], with(&old, &[2]));
    assert_eq!(vw["other.txt"], "keep\n");
    if let Some(gw) = g {
        assert_eq!(gw, vw);
    }
}

#[test]
fn stash_p_with_a_path_and_nothing_selected() {
    let old = nums(30);
    let new = with(&old, &[2, 28]);
    let t = pair(&old, &new);
    t.write("other.txt", "edited\n");
    let o = vcs_in(&t.v, &["stash", "push", "-p", "--", "other.txt"], "y\n");
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(std::fs::read_to_string(t.v.join("other.txt")).unwrap(), "keep\n");
    assert_eq!(std::fs::read_to_string(t.v.join("f.txt")).unwrap(), new);
    // nothing selected: nothing is stashed and nothing changes
    let o = vcs_in(&t.v, &["stash", "push", "-p"], "n\n");
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("No hunks selected"), "{}", text(&o));
    assert_eq!(std::fs::read_to_string(t.v.join("f.txt")).unwrap(), new);
    let list = vcs_in(&t.v, &["stash", "list"], "");
    assert_eq!(String::from_utf8_lossy(&list.stdout).lines().count(), 1);
}

#[test]
fn stash_p_refuses_a_staged_new_file() {
    let t = pair("a\n", "b\n");
    t.write("new.txt", "n\n");
    assert!(vcs_in(&t.v, &["add", "new.txt"], "").status.success());
    let o = vcs_in(&t.v, &["stash", "push", "-p"], "y\n");
    assert!(!o.status.success());
    assert!(text(&o).contains("staged as a new file"), "{}", text(&o));
    assert_eq!(std::fs::read_to_string(t.v.join("f.txt")).unwrap(), "b\n");
}

#[test]
fn p_commands_say_so_when_there_is_nothing_to_choose() {
    let t = pair("a\n", "a\n");
    let o = vcs_in(&t.v, &["add", "-p"], "y\n");
    assert!(o.status.success());
    assert!(text(&o).contains("No changes"), "{}", text(&o));
    let o = vcs_in(&t.v, &["checkout", "x"], "");
    assert!(!o.status.success());
    assert!(text(&o).contains("only exists for `-p`"), "{}", text(&o));
}

impl Twin {
    fn observe_work_only(&self) -> (BTreeMap<String, String>, (), Option<BTreeMap<String, String>>) {
        fn tree(d: &Path) -> BTreeMap<String, String> {
            let mut m = BTreeMap::new();
            for e in std::fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                let n = p.file_name().unwrap().to_string_lossy().into_owned();
                if p.is_file() {
                    m.insert(n, std::fs::read_to_string(&p).unwrap());
                }
            }
            m
        }
        (tree(&self.v), (), self.g.as_ref().map(|g| tree(g)))
    }
}

// ---------------------------------------------------------------- mergetool

fn ok(dir: &Path, args: &[&str]) -> String {
    let o = vcs_in(dir, args, "");
    assert!(o.status.success(), "vcs {args:?} failed: {}", text(&o));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn with_home(dir: &Path, home: &Path, args: &[&str], stdin: &str) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
    c.args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_CONFIG_HOME", home);
    spawn(c, stdin)
}

/// A merge of `main` into `feature` that conflicts in `f` and `g`.
fn merging(t: &Path) -> PathBuf {
    let d = t.join("r");
    ok(t, &["init", "r"]);
    let body = |x: &str| format!("1\n2\n3\n4\n5\n6\n7\n{x}\n");
    std::fs::write(d.join("f"), body("8")).unwrap();
    std::fs::write(d.join("g"), body("8")).unwrap();
    ok(&d, &["add", "."]);
    ok(&d, &["commit", "-m", "base"]);
    ok(&d, &["branch", "feature"]);
    ok(&d, &["switch", "feature"]);
    std::fs::write(d.join("f"), body("FEAT")).unwrap();
    std::fs::write(d.join("g"), body("FEAT")).unwrap();
    ok(&d, &["add", "."]);
    ok(&d, &["commit", "-m", "feature"]);
    ok(&d, &["switch", "main"]);
    std::fs::write(d.join("f"), body("MAIN")).unwrap();
    std::fs::write(d.join("g"), body("MAIN")).unwrap();
    ok(&d, &["add", "."]);
    ok(&d, &["commit", "-m", "main"]);
    ok(&d, &["switch", "feature"]);
    let o = vcs_in(&d, &["merge", "main"], "");
    assert!(!o.status.success());
    d
}

fn tool_config(home: &Path, cmd: &str, extra: &str) {
    std::fs::write(
        home.join("config"),
        format!(
            "merge.tool = t\nmergetool.t.cmd = \"{}\"\n{extra}",
            cmd.replace('\\', "\\\\").replace('"', "\\\"")
        ),
    )
    .unwrap();
}

#[test]
fn mergetool_runs_the_tool_per_file_and_marks_resolved() {
    let t = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let d = merging(t.path());
    // the tool sees all four names; it merges by concatenating LOCAL's last line and REMOTE's last line
    tool_config(
        home.path(),
        "{ head -n 7 \"$LOCAL\"; echo \"$(tail -n 1 \"$LOCAL\")+$(tail -n 1 \"$REMOTE\")\"; } > \"$MERGED\"; test -s \"$BASE\"",
        "",
    );
    // without -y it waits for a return per file
    let o = with_home(&d, home.path(), &["mergetool"], "\n\n");
    assert!(o.status.success(), "{}", text(&o));
    let out = text(&o);
    assert!(out.contains("Hit return to start merge resolution tool (t)"), "{out}");
    assert!(out.contains("resolved f") && out.contains("resolved g"), "{out}");
    assert_eq!(std::fs::read_to_string(d.join("f")).unwrap(), "1\n2\n3\n4\n5\n6\n7\nFEAT+MAIN\n");
    assert_eq!(
        ok(&d, &["resolve", "--check"]).trim(),
        "all conflicts resolved, no conflict markers left"
    );
    // the backup of the conflicted text is kept, like git
    assert!(std::fs::read_to_string(d.join("f.orig")).unwrap().contains("<<<<<<<"));
    std::fs::remove_file(d.join("f.orig")).unwrap();
    std::fs::remove_file(d.join("g.orig")).unwrap();
    ok(&d, &["commit", "-m", "merged"]);
}

#[test]
fn mergetool_asks_when_the_file_is_unchanged_and_obeys_the_answer() {
    let t = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let d = merging(t.path());
    // a tool that only looks: $MERGED stays as it was
    tool_config(home.path(), "true", "mergetool.keepBackup = off\n");
    let o = with_home(&d, home.path(), &["mergetool", "-y", "f"], "n\n");
    assert!(!o.status.success());
    assert!(text(&o).contains("Was the merge successful"), "{}", text(&o));
    assert!(!d.join("f.orig").exists());
    // the markers are still there, so even "y" does not resolve it
    let o = with_home(&d, home.path(), &["mergetool", "-y", "f"], "y\n");
    assert!(!o.status.success());
    assert!(text(&o).contains("conflict markers remain"), "{}", text(&o));
    assert!(ok(&d, &["conflict", "list"]).contains("unresolved"));
}

#[test]
fn mergetool_trust_exit_code_and_only_named_paths() {
    let t = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let d = merging(t.path());
    tool_config(
        home.path(),
        "cp \"$REMOTE\" \"$MERGED\"; exit 3",
        "mergetool.t.trustExitCode = on\n",
    );
    let o = with_home(&d, home.path(), &["mergetool", "-y", "f"], "");
    assert!(!o.status.success(), "a failing tool is not a resolution: {}", text(&o));
    tool_config(home.path(), "cp \"$REMOTE\" \"$MERGED\"", "mergetool.t.trustExitCode = on\n");
    let o = with_home(&d, home.path(), &["mergetool", "-y", "f"], "");
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("1 conflict(s) left"), "{}", text(&o));
    assert_eq!(std::fs::read_to_string(d.join("f")).unwrap(), "1\n2\n3\n4\n5\n6\n7\nMAIN\n");
    assert!(std::fs::read_to_string(d.join("g")).unwrap().contains("<<<<<<<"), "g was not named");
}

#[test]
fn mergetool_needs_a_tool_a_command_and_a_merge() {
    let t = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let plain = t.path().join("p");
    ok(t.path(), &["init", "p"]);
    let o = with_home(&plain, home.path(), &["mergetool"], "");
    assert!(text(&o).contains("no merge or rebase in progress"), "{}", text(&o));
    let d = merging(t.path());
    let o = with_home(&d, home.path(), &["mergetool", "-y"], "");
    assert!(text(&o).contains("no merge tool configured"), "{}", text(&o));
    let o = with_home(&d, home.path(), &["mergetool", "-y", "--tool", "ghost"], "");
    assert!(text(&o).contains("has no command"), "{}", text(&o));
    // a command is refused in the repository config
    let o = vcs_in(&d, &["config", "set", "mergetool.t.cmd", "true"], "");
    assert!(!o.status.success());
    assert!(text(&o).contains("commands from a repository config are refused"), "{}", text(&o));
}
