//! Client-side hooks with the real binary (docs/hooks.md).
#![cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn cmd(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
    c.args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env_remove("VCS_NO_HOOKS");
    c
}
fn vcs(dir: &Path, args: &[&str]) -> Output {
    cmd(dir, args).output().unwrap()
}
fn ok(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(o.status.success(), "vcs {args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn fail(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(!o.status.success(), "vcs {args:?} unexpectedly succeeded");
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn hook(repo: &Path, name: &str, body: &str) -> PathBuf {
    let d = repo.join(".vcs/hooks");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
    let p = d.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}
fn repo() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let r = t.path().join("r");
    std::fs::write(r.join("f"), "one").unwrap();
    ok(&r, &["add", "."]);
    ok(&r, &["commit", "-m", "one"]);
    (t, r)
}
fn change(r: &Path, c: &str) {
    std::fs::write(r.join("f"), c).unwrap();
    ok(r, &["add", "."]);
}
fn log_len(r: &Path) -> usize {
    ok(r, &["log", "--oneline"]).lines().count()
}

#[test]
fn pre_commit_failure_aborts_and_shows_stderr() {
    let (_t, r) = repo();
    hook(&r, "pre-commit", "echo 'lint failed: bad thing' >&2; exit 3");
    change(&r, "two");
    let e = fail(&r, &["commit", "-m", "two"]);
    assert!(e.contains("pre-commit") && e.contains("status 3") && e.contains("lint failed: bad thing"), "{e}");
    assert_eq!(log_len(&r), 1);
    // skipped by flag and by env
    ok(&r, &["--no-hooks", "commit", "-m", "two"]);
    assert_eq!(log_len(&r), 2);
    change(&r, "three");
    let o = cmd(&r, &["commit", "-m", "three"]).env("VCS_NO_HOOKS", "1").output().unwrap();
    assert!(o.status.success());
    assert_eq!(log_len(&r), 3);
}

#[test]
fn environment_is_documented_and_scrubbed() {
    let (_t, r) = repo();
    let out = r.join("env.out");
    hook(&r, "pre-commit", &format!("env > {}", out.display()));
    change(&r, "two");
    let o = cmd(&r, &["commit", "-m", "two"])
        .env("VCS_TOKEN", "secret-token")
        .env("ANTHROPIC_API_KEY", "sk-secret")
        .env("MY_PASSED", "yes")
        .output()
        .unwrap();
    assert!(o.status.success());
    let env = std::fs::read_to_string(&out).unwrap();
    assert!(env.contains("VCS_HOOK=pre-commit"), "{env}");
    assert!(env.contains("VCS_ZONE=root"), "{env}");
    assert!(env.contains("VCS_BRANCH=main"), "{env}");
    assert!(env.contains("VCS_DIR="), "{env}");
    assert!(!env.contains("secret-token") && !env.contains("sk-secret") && !env.contains("MY_PASSED"), "{env}");
    // opt in through hooks.env
    ok(&r, &["config", "set", "hooks.env", "MY_PASSED, VCS_TOKEN"]);
    change(&r, "three");
    let o = cmd(&r, &["commit", "-m", "three"]).env("VCS_TOKEN", "secret-token").env("MY_PASSED", "yes").output().unwrap();
    assert!(o.status.success());
    let env = std::fs::read_to_string(&out).unwrap();
    assert!(env.contains("MY_PASSED=yes") && env.contains("VCS_TOKEN=secret-token"), "{env}");
}

#[test]
fn commit_msg_can_rewrite_and_reject() {
    let (_t, r) = repo();
    hook(&r, "commit-msg", "printf 'PREFIX: ' | cat - \"$1\" > \"$1.new\"; mv \"$1.new\" \"$1\"");
    change(&r, "two");
    ok(&r, &["commit", "-m", "two"]);
    assert!(ok(&r, &["log", "--oneline"]).contains("PREFIX: two"));
    hook(&r, "commit-msg", "grep -q WIP \"$1\" && { echo 'no WIP commits' >&2; exit 1; }; exit 0");
    change(&r, "three");
    let e = fail(&r, &["commit", "-m", "WIP stuff"]);
    assert!(e.contains("no WIP commits"), "{e}");
    assert_eq!(log_len(&r), 2);
}

#[test]
fn post_hooks_run_but_cannot_abort() {
    let (_t, r) = repo();
    let out = r.join("post.out");
    hook(&r, "post-commit", &format!("echo ran >> {}; exit 9", out.display()));
    change(&r, "two");
    let o = vcs(&r, &["commit", "-m", "two"]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("post-commit"), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "ran\n");
    assert_eq!(log_len(&r), 2);
}

#[test]
fn writable_hook_is_refused() {
    let (_t, r) = repo();
    let p = hook(&r, "pre-commit", "exit 0");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o775)).unwrap();
    change(&r, "two");
    let e = fail(&r, &["commit", "-m", "two"]);
    assert!(e.contains("refusing") && e.contains("writable"), "{e}");
    assert!(ok(&r, &["hook", "list"]).contains("REFUSED"));
    // world-writable hooks directory is refused too
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(p.parent().unwrap(), std::fs::Permissions::from_mode(0o757)).unwrap();
    assert!(fail(&r, &["commit", "-m", "two"]).contains("writable"));
    std::fs::set_permissions(p.parent().unwrap(), std::fs::Permissions::from_mode(0o755)).unwrap();
    ok(&r, &["commit", "-m", "two"]);
}

#[test]
fn non_executable_hook_is_skipped_with_warning() {
    let (_t, r) = repo();
    let p = hook(&r, "pre-commit", "exit 1");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    change(&r, "two");
    let o = vcs(&r, &["commit", "-m", "two"]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("not executable"));
}

#[test]
fn timeout_kills_the_whole_process_group() {
    let (_t, r) = repo();
    let marker = r.join("child-survived");
    // A background grandchild that would write a marker after 3s, plus a foreground sleep.
    hook(&r, "pre-commit", &format!("(sleep 3; touch {}) &\nsleep 30", marker.display()));
    ok(&r, &["config", "set", "hooks.timeout", "1"]);
    change(&r, "two");
    let t0 = Instant::now();
    let e = fail(&r, &["commit", "-m", "two"]);
    assert!(t0.elapsed() < Duration::from_secs(15), "took {:?}", t0.elapsed());
    assert!(e.contains("timed out after 1s"), "{e}");
    std::thread::sleep(Duration::from_millis(3500));
    assert!(!marker.exists(), "grandchild was not killed");
    assert_eq!(log_len(&r), 1);
}

#[test]
fn crash_and_signal_are_reported_and_abort() {
    let (_t, r) = repo();
    hook(&r, "pre-commit", "kill -9 $$");
    change(&r, "two");
    let e = fail(&r, &["commit", "-m", "two"]);
    assert!(e.contains("terminated by signal 9"), "{e}");
    hook(&r, "pre-commit", "echo boom >&2; exit 127");
    let e = fail(&r, &["commit", "-m", "two"]);
    assert!(e.contains("status 127") && e.contains("boom"), "{e}");
    // a hook that is not a valid executable (no shebang, garbage) fails cleanly
    let p = hook(&r, "pre-commit", "");
    std::fs::write(&p, [0u8, 1, 2, 3]).unwrap();
    let e = fail(&r, &["commit", "-m", "two"]);
    assert!(e.contains("pre-commit"), "{e}");
    assert_eq!(log_len(&r), 1);
}

#[test]
fn hook_that_ignores_stdin_and_floods_output_does_not_hang() {
    let (_t, r) = repo();
    hook(&r, "pre-commit", "head -c 3000000 /dev/zero | tr '\\0' x; exit 0");
    change(&r, "two");
    ok(&r, &["commit", "-m", "two"]);
}

#[test]
fn merge_rebase_checkout_hooks_and_args() {
    let (t, r) = repo();
    let log = t.path().join("calls.out");
    for h in ["pre-merge", "post-merge", "pre-rebase", "post-checkout"] {
        hook(&r, h, &format!("echo \"$VCS_HOOK $VCS_ZONE $VCS_BRANCH $*\" >> {}", log.display()));
    }
    ok(&r, &["zone", "branch", "root", "topic"]);
    ok(&r, &["zone", "checkout", "root", "topic"]);
    std::fs::write(r.join("g"), "g").unwrap();
    ok(&r, &["add", "."]);
    ok(&r, &["commit", "-m", "on topic"]);
    ok(&r, &["switch", "main"]);
    ok(&r, &["merge", "topic"]);
    ok(&r, &["rebase", "topic"]);
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("post-checkout root topic"), "{calls}");
    assert!(calls.contains("pre-merge root main topic"), "{calls}");
    assert!(calls.contains("post-merge root main topic"), "{calls}");
    assert!(calls.contains("pre-rebase root main topic"), "{calls}");
    // pre-merge failure aborts
    hook(&r, "pre-merge", "echo nope >&2; exit 1");
    assert!(fail(&r, &["merge", "topic"]).contains("nope"));
}

#[test]
fn publish_hooks_get_updates_on_stdin() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let r = t.path().join("r");
    let remote = t.path().join("remote");
    std::fs::write(r.join("f"), "one").unwrap();
    ok(&r, &["add", "."]);
    ok(&r, &["commit", "-m", "one"]);
    ok(&r, &["remote", "set", remote.to_str().unwrap()]);
    let pre = r.join("pre.out");
    let post = r.join("post.out");
    hook(&r, "pre-publish", &format!("cat > {}", pre.display()));
    hook(&r, "post-publish", &format!("cat > {}", post.display()));
    ok(&r, &["publish"]);
    for f in [&pre, &post] {
        let s = std::fs::read_to_string(f).unwrap();
        let l: Vec<&str> = s.trim().split(' ').collect();
        assert_eq!(l.len(), 4, "{s}");
        assert_eq!((l[0], l[1]), ("root", "main"));
        assert_eq!(l[2], "0".repeat(64));
        assert_eq!(l[3].len(), 64);
    }
    // a failing pre-publish stops before anything is sent
    std::fs::write(r.join("f"), "two").unwrap();
    ok(&r, &["add", "."]);
    ok(&r, &["commit", "-m", "two"]);
    hook(&r, "pre-publish", "echo 'not now' >&2; exit 1");
    assert!(fail(&r, &["publish"]).contains("not now"));
    ok(&r, &["--no-hooks", "publish"]);
}

#[test]
fn hook_list_install_run() {
    let (t, r) = repo();
    assert!(ok(&r, &["hook", "list"]).contains("not installed"));
    let src = t.path().join("my-hook");
    std::fs::write(&src, "#!/bin/sh\necho hello-from-hook \"$@\"\n").unwrap();
    ok(&r, &["hook", "install", "post-commit", src.to_str().unwrap()]);
    let mode = std::fs::metadata(r.join(".vcs/hooks/post-commit")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755);
    assert!(fail(&r, &["hook", "install", "post-commit", src.to_str().unwrap()]).contains("--force"));
    ok(&r, &["hook", "install", "--force", "post-commit", src.to_str().unwrap()]);
    assert!(fail(&r, &["hook", "install", "../evil", src.to_str().unwrap()]).contains("unknown hook"));
    assert!(ok(&r, &["hook", "list"]).contains("post-commit"));
    let out = ok(&r, &["hook", "run", "post-commit", "a", "b"]);
    assert!(out.contains("hello-from-hook a b"), "{out}");
    assert!(fail(&r, &["hook", "run", "pre-commit"]).contains("not installed"));
}

#[test]
fn linked_worktrees_use_the_main_repositorys_hooks() {
    let (t, r) = repo();
    let wt = t.path().join("wt");
    ok(&r, &["worktree", "add", wt.to_str().unwrap(), "topic"]);
    hook(&r, "pre-commit", "echo 'from common' >&2; exit 1");
    std::fs::write(wt.join("f"), "x").unwrap();
    ok(&wt, &["add", "."]);
    assert!(fail(&wt, &["commit", "-m", "x"]).contains("from common"));
    // a hooks directory inside the linked worktree's own .vcs is ignored
    std::fs::remove_file(r.join(".vcs/hooks/pre-commit")).unwrap();
    hook(&wt, "pre-commit", "exit 1");
    ok(&wt, &["commit", "-m", "x"]);
}

#[test]
fn hostile_trees_cannot_install_hooks() {
    use vcs_objects::names::{validate_component, validate_path};
    for p in [".vcs/hooks/pre-commit", ".VCS/hooks/pre-commit", "a/.vcs/hooks/pre-commit", ".vcs", "x/.vcs/hooks"] {
        assert!(validate_path(p).is_err(), "{p} accepted");
    }
    assert!(validate_component(".vcs").is_err());

    // import-git: a stream that tries to write .vcs/hooks/pre-commit
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let r = t.path().join("r");
    let script = "#!/bin/sh\ntouch /tmp/x\n";
    let stream = format!(
        "blob\nmark :1\ndata {}\n{script}\ncommit refs/heads/main\nmark :2\ncommitter A <a@x> 1000 +0000\ndata 3\nhi\nM 100755 :1 .vcs/hooks/pre-commit\nM 100644 inline ok.txt\ndata 3\nok\n\n",
        script.len()
    );
    let sp = t.path().join("s.fi");
    std::fs::write(&sp, stream).unwrap();
    let o = vcs(&r, &["import-git", sp.to_str().unwrap()]);
    let all = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(!r.join(".vcs/hooks").exists(), "import created hooks: {all}");
    if o.status.success() {
        assert!(all.contains("skipped path"), "{all}");
    }
    assert!(!r.join(".vcs/hooks/pre-commit").exists());
}

#[test]
fn clone_and_bundle_never_bring_hooks() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "a"]);
    let a = t.path().join("a");
    std::fs::write(a.join("f"), "one").unwrap();
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "one"]);
    hook(&a, "post-checkout", "touch /tmp/should-never-run-vcs-hooks");
    let remote = t.path().join("remote");
    ok(&a, &["remote", "set", remote.to_str().unwrap()]);
    ok(&a, &["publish"]);
    ok(t.path(), &["clone", remote.to_str().unwrap(), "b"]);
    assert!(!t.path().join("b/.vcs/hooks").exists());
    let bundle = t.path().join("x.bundle");
    ok(&a, &["bundle", "create", bundle.to_str().unwrap()]);
    ok(t.path(), &["clone", bundle.to_str().unwrap(), "c"]);
    assert!(!t.path().join("c/.vcs/hooks").exists());
    ok(&t.path().join("b"), &["fetch"]);
    assert!(!t.path().join("b/.vcs/hooks").exists());
}
