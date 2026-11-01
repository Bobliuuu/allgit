//! Lockable binary assets (`.vcsattributes`) against a directory remote, with the real binary.
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
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}
fn fail(dir: &Path, who: &str, args: &[&str]) -> String {
    let o = vcs(dir, who, args);
    assert!(!o.status.success(), "vcs {args:?} unexpectedly succeeded");
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}
fn write(dir: &Path, f: &str, c: &str) {
    let p = dir.join(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    // an edit of a read-only file the way an artist's tool would after chmod
    if p.exists() {
        let mut perm = std::fs::metadata(&p).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        std::fs::set_permissions(&p, perm).unwrap();
    }
    std::fs::write(p, c).unwrap();
}
fn ro(dir: &Path, f: &str) -> bool {
    std::fs::metadata(dir.join(f)).unwrap().permissions().readonly()
}

struct World {
    _t: tempfile::TempDir,
    root: std::path::PathBuf,
    a: std::path::PathBuf,
    b: std::path::PathBuf,
}

/// alice's repo with a published lockable texture and a plain file; bob has cloned it.
fn world() -> World {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().to_path_buf();
    let remote = root.join("remote");
    let (a, b) = (root.join("a"), root.join("b"));
    ok(&root, "alice", &["init", "a"]);
    write(
        &a,
        ".vcsattributes",
        "# binaries\n*.psd lockable\nassets/** lockable merge=binary\n*.dat merge=binary\n",
    );
    write(&a, "art/hero.psd", "v1\n");
    write(&a, "notes.txt", "n1\n");
    ok(&a, "alice", &["add", "."]);
    ok(&a, "alice", &["commit", "-m", "init"]);
    ok(&a, "alice", &["remote", "set", remote.to_str().unwrap()]);
    ok(&a, "alice", &["publish"]);
    let url = format!("file://{}", remote.display());
    ok(&root, "bob", &["clone", &url, "b"]);
    World { _t: t, root, a, b }
}

#[test]
fn lockable_files_are_read_only_until_locked_and_guarded_at_add_and_commit() {
    let w = world();
    let (a, b) = (&w.a, &w.b);
    // checked out read-only; unmatched files stay writable
    assert!(ro(b, "art/hero.psd"));
    assert!(!ro(b, "notes.txt"));
    assert!(ro(a, "art/hero.psd"), "alice's own copy is read-only after commit");

    // alice locks: writable for her, still read-only in bob's checkout; bob sees owner and age
    let out = ok(a, "alice", &["lock", "art/hero.psd", "--lease-secs", "3600", "--reason", "repaint"]);
    assert!(out.contains("locked art/hero.psd"), "{out}");
    assert!(!ro(a, "art/hero.psd"));
    assert!(ro(b, "art/hero.psd"));
    let l = ok(b, "bob", &["locks"]);
    assert!(
        l.contains("alice@example.com") && l.contains("age ") && l.contains("expires in") && l.contains("repaint"),
        "{l}"
    );

    // bob edits anyway (chmod): status flags it, add and commit refuse, --force-no-lock is explicit
    write(b, "art/hero.psd", "bob's version\n");
    let st = ok(b, "bob", &["status"]);
    assert!(
        st.contains("lockable files modified without a lock") && st.contains("art/hero.psd"),
        "{st}"
    );
    let err = fail(b, "bob", &["add", "art/hero.psd"]);
    assert!(
        err.contains("refusing to stage lockable") && err.contains("vcs lock") && err.contains("--force-no-lock"),
        "{err}"
    );
    let err = fail(b, "bob", &["add", "."]);
    assert!(err.contains("art/hero.psd"), "{err}");
    let forced = ok(b, "bob", &["add", "--force-no-lock", "art/hero.psd"]);
    assert!(forced.contains("--force-no-lock") && forced.contains("staged 1"), "{forced}");
    let err = fail(b, "bob", &["commit", "-m", "x"]);
    assert!(err.contains("refusing to commit lockable"), "{err}");
    ok(b, "bob", &["commit", "-m", "forced", "--force-no-lock"]);
    // the shared lock still stops the publish
    let err = fail(b, "bob", &["publish"]);
    assert!(err.contains("art/hero.psd") && err.contains("locked by alice@example.com"), "{err}");

    // alice edits, commits and publishes under her lock, then unlocks: read-only again
    write(a, "art/hero.psd", "v2 by alice\n");
    ok(a, "alice", &["add", "art/hero.psd"]);
    ok(a, "alice", &["commit", "-m", "repaint"]);
    assert!(ok(a, "alice", &["publish"]).contains("published atomically"));
    ok(a, "alice", &["unlock", "art/hero.psd"]);
    assert!(ro(a, "art/hero.psd"));
    assert!(ok(a, "alice", &["status"]).contains("clean"));
}

#[test]
fn release_on_publish_makes_the_file_read_only_and_frees_the_lock() {
    let w = world();
    let a = &w.a;
    ok(a, "alice", &["config", "set", "lock.releaseOnPublish", "on"]);
    ok(a, "alice", &["lock", "art/hero.psd", "--lease-secs", "3600"]);
    write(a, "art/hero.psd", "v2\n");
    ok(a, "alice", &["add", "art/hero.psd"]);
    ok(a, "alice", &["commit", "-m", "v2"]);
    assert!(!ro(a, "art/hero.psd"), "still locked until publish");
    let out = ok(a, "alice", &["publish"]);
    assert!(out.contains("released 1 lock"), "{out}");
    assert!(ro(a, "art/hero.psd"));
    assert!(ok(a, "alice", &["locks"]).contains("no locks"));
    // without the option the lock survives a publish
    ok(a, "alice", &["config", "set", "lock.releaseOnPublish", "off"]);
    ok(a, "alice", &["lock", "art/hero.psd", "--lease-secs", "3600"]);
    write(a, "art/hero.psd", "v3\n");
    ok(a, "alice", &["add", "art/hero.psd"]);
    ok(a, "alice", &["commit", "-m", "v3"]);
    ok(a, "alice", &["publish"]);
    assert!(ok(a, "alice", &["locks"]).contains("art/hero.psd"));
    assert!(!ro(a, "art/hero.psd"));
    assert!(fail(&w.root, "alice", &["config", "set", "lock.offline", "maybe"]).contains("warn"));
}

#[test]
fn offline_policy_warn_or_refuse() {
    let w = world();
    let a = &w.a;
    ok(a, "alice", &["lock", "art/hero.psd", "--lease-secs", "3600"]);
    write(a, "art/hero.psd", "offline edit\n");
    // the remote becomes unreachable
    ok(a, "alice", &["remote", "set", "http://127.0.0.1:1/"]);
    let out = ok(a, "alice", &["add", "art/hero.psd"]);
    assert!(
        out.contains("cannot reach the remote") && out.contains("lock.offline=warn"),
        "{out}"
    );
    ok(a, "alice", &["restore", "--staged", "art/hero.psd"]);
    ok(a, "alice", &["config", "set", "lock.offline", "refuse"]);
    let err = fail(a, "alice", &["add", "art/hero.psd"]);
    assert!(err.contains("lock.offline=refuse"), "{err}");
    // taking a lock offline is impossible and says so
    let err = fail(a, "alice", &["lock", "notes.txt", "--lease-secs", "60"]);
    assert!(!err.is_empty());
}

#[test]
fn expired_lock_is_stale_and_can_be_taken_over() {
    let w = world();
    let (a, b) = (&w.a, &w.b);
    ok(a, "alice", &["lock", "art/hero.psd", "--lease-secs", "1"]);
    std::thread::sleep(std::time::Duration::from_millis(2200));
    let l = ok(b, "bob", &["locks"]);
    assert!(l.contains("expired") && l.contains("stale"), "{l}");
    ok(b, "bob", &["lock", "art/hero.psd", "--lease-secs", "3600"]);
    assert!(!ro(b, "art/hero.psd"));
    // alice's local record is stale: her change is refused with a clear reason
    write(a, "art/hero.psd", "late\n");
    let err = fail(a, "alice", &["add", "art/hero.psd"]);
    assert!(err.contains("art/hero.psd") && err.contains("expired or was broken"), "{err}");
    // without a server there is no admin role: --break is unrestricted on a directory remote
    ok(a, "alice", &["unlock", "art/hero.psd", "--break"]);
    assert!(ok(a, "alice", &["locks"]).contains("no locks"));
}

#[test]
fn merge_binary_attribute_refuses_a_text_merge() {
    let w = world();
    let (a, b) = (&w.a, &w.b);
    write(a, "m.dat", "1\n2\n3\n4\n5\n6\n7\n8\n");
    ok(a, "alice", &["add", "."]);
    ok(a, "alice", &["commit", "-m", "dat"]);
    ok(a, "alice", &["publish"]);
    ok(b, "bob", &["sync"]);
    write(a, "m.dat", "ONE\n2\n3\n4\n5\n6\n7\n8\n");
    ok(a, "alice", &["add", "."]);
    ok(a, "alice", &["commit", "-m", "a"]);
    ok(a, "alice", &["publish"]);
    write(b, "m.dat", "1\n2\n3\n4\n5\n6\n7\nEIGHT\n");
    ok(b, "bob", &["add", "."]);
    ok(b, "bob", &["commit", "-m", "b"]);
    ok(b, "bob", &["fetch"]);
    let err = fail(b, "bob", &["merge", "--remote", "main"]);
    assert!(err.contains("conflict: m.dat") && err.contains("merge has conflicts"), "{err}");
    assert_eq!(
        std::fs::read_to_string(b.join("m.dat")).unwrap(),
        "1\n2\n3\n4\n5\n6\n7\nEIGHT\n",
        "ours stays, no markers"
    );
    let show = ok(b, "bob", &["conflict", "show", "m.dat"]);
    assert!(show.contains("modify/modify") && !show.contains("<<<<<<<"), "{show}");
    ok(b, "bob", &["resolve", "m.dat", "--theirs"]);
    ok(b, "bob", &["add", "m.dat"]);
    ok(b, "bob", &["commit", "-m", "take theirs"]);
    assert_eq!(std::fs::read_to_string(b.join("m.dat")).unwrap(), "ONE\n2\n3\n4\n5\n6\n7\n8\n");
}

#[test]
fn worktrees_share_locks_and_modes() {
    let w = world();
    let a = &w.a;
    ok(a, "alice", &["worktree", "add", "../wt", "topic"]);
    let wt = w.root.join("wt");
    assert!(ro(&wt, "art/hero.psd"), "new worktree checks lockable files out read-only");
    ok(a, "alice", &["lock", "art/hero.psd", "--lease-secs", "3600"]);
    assert!(!ro(&wt, "art/hero.psd") && !ro(a, "art/hero.psd"));
    // the lock held by this repository lets the worktree stage the change too
    write(&wt, "art/hero.psd", "from worktree\n");
    ok(&wt, "alice", &["add", "art/hero.psd"]);
    ok(&wt, "alice", &["commit", "-m", "wt edit"]);
    ok(a, "alice", &["unlock", "art/hero.psd"]);
    assert!(ro(&wt, "art/hero.psd") && ro(a, "art/hero.psd"));
}
