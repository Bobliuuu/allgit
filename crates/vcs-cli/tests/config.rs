//! `vcs config`: layers, precedence, parse errors, identity and editor flow.
use std::path::Path;
use std::process::{Command, Output};

struct Env {
    home: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        Env {
            home: tempfile::tempdir().unwrap(),
        }
    }
    /// No author env: identity comes from config only.
    fn vcs(&self, dir: &Path, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
        c.args(args)
            .current_dir(dir)
            .env_remove("VCS_AUTHOR_NAME")
            .env_remove("VCS_AUTHOR_EMAIL")
            .env_remove("VCS_EDITOR")
            .env_remove("VISUAL")
            .env_remove("EDITOR")
            .env_remove("XDG_CONFIG_HOME")
            .env("VCS_CONFIG_HOME", self.home.path());
        for (k, v) in extra {
            c.env(k, v);
        }
        c.output().unwrap()
    }
    fn ok(&self, dir: &Path, args: &[&str]) -> String {
        let o = self.vcs(dir, args, &[]);
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_owned()
    }
    fn err(&self, dir: &Path, args: &[&str], extra: &[(&str, &str)]) -> String {
        let o = self.vcs(dir, args, extra);
        assert!(!o.status.success(), "{args:?} unexpectedly succeeded");
        let e = String::from_utf8_lossy(&o.stderr).into_owned();
        assert!(!e.contains("panicked"), "{e}");
        e
    }
}

fn repo(e: &Env) -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    e.ok(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    (t, d)
}

#[test]
fn precedence_env_over_repo_over_user() {
    let e = Env::new();
    let (_t, d) = repo(&e);
    e.ok(&d, &["config", "set", "--user", "user.name", "User"]);
    assert_eq!(e.ok(&d, &["config", "get", "user.name"]), "User");
    e.ok(&d, &["config", "set", "user.name", "Repo"]);
    assert_eq!(e.ok(&d, &["config", "get", "user.name"]), "Repo");
    let o = e.vcs(&d, &["config", "get", "user.name"], &[("VCS_AUTHOR_NAME", "Env")]);
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "Env");
    assert!(e.ok(&d, &["config", "list"]).contains("user.name=Repo  (repo)"));
    e.ok(&d, &["config", "unset", "user.name"]);
    assert_eq!(e.ok(&d, &["config", "get", "user.name"]), "User");
    assert!(e.err(&d, &["config", "unset", "user.name"], &[]).contains("not set"));
    assert!(std::fs::read_to_string(e.home.path().join("config"))
        .unwrap()
        .contains("user.name = User"));
}

#[test]
fn xdg_location_used_without_override() {
    let e = Env::new();
    let (_t, d) = repo(&e);
    let x = tempfile::tempdir().unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(["config", "set", "--user", "core.editor", "ed"])
        .current_dir(&d)
        .env_remove("VCS_CONFIG_HOME")
        .env("XDG_CONFIG_HOME", x.path())
        .output()
        .unwrap();
    assert!(o.status.success());
    assert!(x.path().join("vcs/config").is_file());
}

#[test]
fn bad_keys_values_and_files_never_panic() {
    let e = Env::new();
    let (_t, d) = repo(&e);
    assert!(e.err(&d, &["config", "set", "nope.key", "x"], &[]).contains("unknown config key"));
    assert!(e
        .err(&d, &["config", "set", "merge.structural", "maybe"], &[])
        .contains("'on' or 'off'"));
    assert!(e.err(&d, &["config", "set", "log.default-format", "x"], &[]).contains("oneline"));
    for bad in [
        "no equals here\n",
        "= v\n",
        "a b = c\n",
        "user.name = \"open\n",
        "user.name = \"x\\q\"\n",
        "user.name = \"x\" junk\n",
    ] {
        std::fs::write(d.join(".vcs/config"), bad).unwrap();
        let err = e.err(&d, &["config", "list"], &[]);
        assert!(err.contains("config:1"), "{bad:?} -> {err}");
    }
    std::fs::write(d.join(".vcs/config"), b"\xff\xfe\n").unwrap();
    assert!(e.err(&d, &["config", "get", "user.name"], &[]).contains("UTF-8"));
    std::fs::write(d.join(".vcs/config"), "# c\n; c\n\nmerge.structural = maybe\n").unwrap();
    assert!(e.err(&d, &["merge", "x"], &[]).contains("merge.structural"));
}

#[test]
fn commit_requires_identity_then_uses_it() {
    let e = Env::new();
    let (_t, d) = repo(&e);
    std::fs::write(d.join("a.txt"), "a\n").unwrap();
    e.ok(&d, &["add", "a.txt"]);
    let err = e.err(&d, &["commit", "-m", "x"], &[]);
    assert!(
        err.contains("user.name and user.email are not set") && err.contains("vcs config set"),
        "{err}"
    );
    e.ok(&d, &["config", "set", "--user", "user.name", "Ann Dev"]);
    assert!(e.err(&d, &["commit", "-m", "x"], &[]).contains("user.email is not set"));
    e.ok(&d, &["config", "set", "--user", "user.email", "ann@example.com"]);
    e.ok(&d, &["commit", "-m", "first"]);
    assert!(e.ok(&d, &["log"]).contains("Author: Ann Dev <ann@example.com>"));
    assert!(e.ok(&d, &["log", "--oneline"]).lines().next().unwrap().ends_with("first"));
    e.ok(&d, &["config", "set", "log.default-format", "oneline"]);
    assert!(!e.ok(&d, &["log"]).contains("Author:"));
    // tag and stash need identity too
    e.ok(&d, &["config", "unset", "--user", "user.email"]);
    assert!(e.err(&d, &["tag", "v1", "-m", "rel"], &[]).contains("user.email is not set"));
    std::fs::write(d.join("a.txt"), "b\n").unwrap();
    assert!(e.err(&d, &["stash", "push"], &[]).contains("user.email is not set"));
}

#[cfg(unix)]
#[test]
fn editor_flow() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    let (t, d) = repo(&e);
    e.ok(&d, &["config", "set", "user.name", "Ann"]);
    e.ok(&d, &["config", "set", "user.email", "ann@example.com"]);
    std::fs::write(d.join("a.txt"), "a\n").unwrap();
    e.ok(&d, &["add", "a.txt"]);
    // no editor at all
    assert!(e.err(&d, &["commit"], &[]).contains("no editor configured"));
    let script = |name: &str, body: &str| {
        let p = t.path().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_string_lossy().into_owned()
    };
    // empty message aborts
    let noop = script("noop.sh", "true");
    assert!(e.err(&d, &["commit"], &[("EDITOR", &noop)]).contains("empty message"));
    // failing editor aborts
    let fail = script("fail.sh", "exit 3");
    assert!(e.err(&d, &["commit"], &[("EDITOR", &fail)]).contains("commit aborted"));
    // $EDITOR works; comments are stripped
    let good = script("good.sh", "printf 'from editor\\n\\nbody line\\n# dropped\\n' > \"$1\"");
    let o = e.vcs(&d, &["commit"], &[("EDITOR", &good)]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let log = e.ok(&d, &["log"]);
    assert!(
        log.contains("from editor") && log.contains("body line") && !log.contains("dropped"),
        "{log}"
    );
    // core.editor wins over $EDITOR
    std::fs::write(d.join("a.txt"), "c\n").unwrap();
    e.ok(&d, &["add", "a.txt"]);
    let cfgd = script("cfg.sh", "echo 'via core.editor' > \"$1\"");
    e.ok(&d, &["config", "set", "--user", "core.editor", &cfgd]);
    let o = e.vcs(&d, &["commit"], &[("EDITOR", &fail)]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(e.ok(&d, &["log", "-n", "1"]).contains("via core.editor"));
}

/// Security review finding 15: a program named by the repository's own config is never run, exactly
/// like `credential.helper = !cmd`. The user config and the environment still work.
#[cfg(unix)]
#[test]
fn repo_config_cannot_name_an_editor_or_assistant() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    let (t, d) = repo(&e);
    e.ok(&d, &["config", "set", "user.name", "Ann"]);
    e.ok(&d, &["config", "set", "user.email", "ann@example.com"]);
    let marker = d.join("pwned");
    let evil_path = t.path().join("evil.sh");
    std::fs::write(&evil_path, format!("#!/bin/sh\ntouch {}\necho evil > \"$1\"\n", marker.display())).unwrap();
    std::fs::set_permissions(&evil_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let evil = evil_path.to_string_lossy().into_owned();
    // refused at set time at repository scope
    for key in ["core.editor", "merge.assist"] {
        let err = e.err(&d, &["config", "set", key, &evil], &[]);
        assert!(err.contains("commands from a repository config are refused"), "{err}");
    }
    // a hand-edited .vcs/config is ignored with a warning, not executed
    std::fs::write(
        d.join(".vcs/config"),
        format!("user.name = Ann\nuser.email = ann@example.com\ncore.editor = {evil}\nmerge.assist = {evil}\n"),
    )
    .unwrap();
    std::fs::write(d.join("f.txt"), "x\n").unwrap();
    e.ok(&d, &["add", "f.txt"]);
    let out = e.vcs(&d, &["commit"], &[]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("ignoring core.editor from the repository config"), "{stderr}");
    assert!(!marker.exists(), "repo-config editor was executed");
    // `off` and the user config remain allowed
    e.ok(&d, &["config", "set", "merge.assist", "off"]);
}
