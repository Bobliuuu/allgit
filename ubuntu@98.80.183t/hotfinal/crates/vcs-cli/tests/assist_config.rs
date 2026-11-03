//! The opt-in `merge.assist` setting: unset means the assistant never runs.
#![cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn vcs(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_CONFIG_HOME", home)
        .env_remove("VCS_MERGE_ASSIST")
        .output()
        .unwrap()
}
fn ok(dir: &Path, home: &Path, args: &[&str]) -> String {
    let o = vcs(dir, home, args);
    assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn out_of(dir: &Path, home: &Path, args: &[&str]) -> String {
    let o = vcs(dir, home, args);
    assert!(!o.status.success());
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn repo(root: &Path, home: &Path) -> PathBuf {
    ok(root, home, &["init", "r"]);
    let r = root.join("r");
    let w = |c: &str| std::fs::write(r.join("f.rs"), c).unwrap();
    let commit = |m: &str| {
        ok(&r, home, &["add", "."]);
        ok(&r, home, &["commit", "-m", m]);
    };
    w("1\n2\n3\n4\n5\n");
    commit("init");
    ok(&r, home, &["branch", "side"]);
    w("1\n2\n3\nMAIN\n5\n");
    commit("main");
    ok(&r, home, &["switch", "side"]);
    w("1\n2\n3\nSIDE\n5\n");
    commit("side");
    ok(&r, home, &["switch", "main"]);
    r
}

const BODY: &str = r#"cat >/dev/null; printf '%s' '{"resolution":"1\n2\n3\nBOTH\n5\n"}'"#;

#[test]
fn setting_is_known_and_opt_in() {
    let t = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let r = repo(t.path(), home.path());
    let script = t.path().join("fake.sh");
    std::fs::write(&script, BODY).unwrap();
    let cmd = format!("sh {}", script.display());
    let cmd = cmd.as_str();
    // Unset: no assistant, no proposals.
    let out = out_of(&r, home.path(), &["merge", "side"]);
    assert!(!out.contains("merge.assist"), "{out}");
    assert!(!out.contains("assist:"), "{out}");
    ok(&r, home.path(), &["merge", "--abort"]);
    // Set: runs, announces itself.
    ok(&r, home.path(), &["config", "set", "--user", "merge.assist", cmd]);
    assert_eq!(ok(&r, home.path(), &["config", "get", "merge.assist"]).trim(), cmd);
    let out = out_of(&r, home.path(), &["merge", "side"]);
    assert!(out.contains("merge.assist is set") && out.contains("proposal saved"), "{out}");
    assert!(ok(&r, home.path(), &["conflict", "proposals"]).contains("+ BOTH"));
    ok(&r, home.path(), &["merge", "--abort"]);
    // --no-assist wins over the setting.
    let out = out_of(&r, home.path(), &["merge", "side", "--no-assist"]);
    assert!(!out.contains("proposal saved") && !out.contains("merge.assist is set"), "{out}");
    ok(&r, home.path(), &["merge", "--abort"]);
    // `off` disables.
    ok(&r, home.path(), &["config", "set", "--user", "merge.assist", "off"]);
    let out = out_of(&r, home.path(), &["merge", "side"]);
    assert!(!out.contains("proposal saved"), "{out}");
}
