//! `vcs lock` / `unlock` / `locks` end to end with two users on one directory remote.
use std::path::Path;
use std::process::{Command, Output};

fn vcs(dir: &Path, who: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", who)
        .env("VCS_AUTHOR_EMAIL", format!("{who}@example.com"))
        .env_remove("VCS_PRINCIPAL")
        .output()
        .unwrap()
}
fn ok(dir: &Path, who: &str, args: &[&str]) -> String {
    let o = vcs(dir, who, args);
    assert!(o.status.success(), "vcs {args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn fail(dir: &Path, who: &str, args: &[&str]) -> String {
    let o = vcs(dir, who, args);
    assert!(!o.status.success(), "vcs {args:?} unexpectedly succeeded");
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn write(dir: &Path, f: &str, c: &str) {
    let p = dir.join(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}

#[test]
fn second_users_publish_is_refused_while_a_lock_is_held() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    let remote = root.join("remote");
    let (a, b) = (root.join("a"), root.join("b"));
    ok(root, "alice", &["init", "a"]);
    write(&a, "art/hero.bin", "v1\n");
    write(&a, "notes.txt", "n1\n");
    ok(&a, "alice", &["add", "."]);
    ok(&a, "alice", &["commit", "-m", "init"]);
    ok(&a, "alice", &["remote", "set", remote.to_str().unwrap()]);
    ok(&a, "alice", &["publish"]);
    let url = format!("file://{}", remote.display());
    ok(root, "bob", &["clone", &url, "b"]);

    // alice locks the texture
    assert!(fail(&a, "alice", &["lock", "art/hero.bin"]).contains("--lease-secs"));
    let out = ok(
        &a,
        "alice",
        &["lock", "art/hero.bin", "--lease-secs", "3600", "--reason", "repaint"],
    );
    assert!(out.contains("locked art/hero.bin"), "{out}");
    let listing = ok(&b, "bob", &["locks"]);
    assert!(
        listing.contains("art/hero.bin") && listing.contains("alice@example.com") && listing.contains("repaint"),
        "{listing}"
    );

    // bob edits the locked file and an unrelated one: publish refused, nothing reaches the remote
    write(&b, "art/hero.bin", "bob's version\n");
    write(&b, "notes.txt", "n2\n");
    ok(&b, "bob", &["add", "."]);
    ok(&b, "bob", &["commit", "-m", "bob edits"]);
    let err = fail(&b, "bob", &["publish"]);
    assert!(err.contains("art/hero.bin") && err.contains("locked by alice@example.com"), "{err}");
    assert!(err.contains("local commits are kept"), "{err}");

    // alice publishes her own change to the locked file (token presented automatically)
    write(&a, "art/hero.bin", "v2 by alice\n");
    ok(&a, "alice", &["add", "."]);
    ok(&a, "alice", &["commit", "-m", "alice repaint"]);
    assert!(ok(&a, "alice", &["publish"]).contains("published atomically"));

    // alice releases; bob can unlock nothing of hers without --break, and then it works after release
    assert!(fail(&b, "bob", &["unlock", "art/hero.bin"]).contains("only the holder"));
    ok(&a, "alice", &["unlock", "art/hero.bin"]);
    assert!(ok(&b, "bob", &["locks"]).contains("no locks"));
    assert!(ok(&b, "bob", &["fetch"]).contains("fetched"));
}
