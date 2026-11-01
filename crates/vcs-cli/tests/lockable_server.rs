//! Lockable assets (`.vcsattributes`) against the real `vcs-server`: two users racing for a file,
//! the server-side `require-lock` policy, and `--break` needing an admin token.
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

fn server_bin() -> PathBuf {
    let vcs = PathBuf::from(env!("CARGO_BIN_EXE_vcs"));
    let p = vcs.with_file_name(if cfg!(windows) { "vcs-server.exe" } else { "vcs-server" });
    if !p.exists() {
        // `cargo test -p vcs-cli` does not build other packages' binaries
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let st = Command::new(cargo)
            .args(["build", "-p", "vcs-server", "--bin", "vcs-server"])
            .status()
            .unwrap();
        assert!(st.success() && p.exists(), "could not build vcs-server");
    }
    p
}

struct Srv {
    child: Child,
    url: String,
}
impl Drop for Srv {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn hash(tok: &str) -> String {
    let o = Command::new(server_bin()).args(["hash-token", tok]).output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_owned()
}

fn start_server(dir: &Path, backend: &[&str], extra: &[&str], envs: &[(&str, String)]) -> Srv {
    let tokens = dir.join("tokens");
    std::fs::write(
        &tokens,
        format!(
            "# principal role zones token-hash\nalice write * {}\nbob write * {}\nrita read * {}\nroot admin * {}\n",
            hash("tok-alice"),
            hash("tok-bob"),
            hash("tok-rita"),
            hash("tok-root")
        ),
    )
    .unwrap();
    let mut cmd = Command::new(server_bin());
    cmd.args(backend)
        .args(["--tokens", tokens.to_str().unwrap(), "--listen", "127.0.0.1:0"])
        .args(extra)
        .stderr(Stdio::piped())
        .stdout(Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stderr.take().unwrap()).read_line(&mut line).unwrap();
    let url = line
        .split_whitespace()
        .find(|w| w.starts_with("http://"))
        .unwrap_or_else(|| panic!("server did not report its address: {line:?}"))
        .to_owned();
    Srv { child, url }
}

fn vcs_as(dir: &Path, who: &str, token: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", who)
        .env("VCS_AUTHOR_EMAIL", format!("{who}@example.com"))
        .env("VCS_PRINCIPAL", who)
        .env("VCS_TOKEN", token)
        .output()
        .unwrap()
}
fn ok(dir: &Path, who: &str, args: &[&str]) -> String {
    let o = vcs_as(dir, who, &format!("tok-{who}"), args);
    assert!(
        o.status.success(),
        "vcs {args:?} as {who} failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn fail(dir: &Path, who: &str, args: &[&str]) -> String {
    let o = vcs_as(dir, who, &format!("tok-{who}"), args);
    assert!(!o.status.success(), "vcs {args:?} as {who} unexpectedly succeeded");
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn write(dir: &Path, f: &str, c: &str) {
    let p = dir.join(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    if p.exists() {
        // edit a read-only file the way a tool would after chmod
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

/// alice has published a lockable texture; bob and root (admin) have cloned.
fn setup(root: &Path, url: &str) {
    let a = root.join("a");
    ok(root, "alice", &["init", "a"]);
    write(&a, ".vcsattributes", "*.psd lockable\n");
    write(&a, "art/hero.psd", "v1\n");
    write(&a, "notes.txt", "n1\n");
    ok(&a, "alice", &["add", "."]);
    ok(&a, "alice", &["commit", "-m", "init"]);
    ok(&a, "alice", &["remote", "set", url]);
    // require-lock covers new files too: lock the (not yet published) path, publish, release
    ok(&a, "alice", &["lock", "art/hero.psd", "--lease-secs", "3600"]);
    ok(&a, "alice", &["publish"]);
    ok(&a, "alice", &["unlock", "art/hero.psd"]);
    ok(root, "bob", &["clone", url, "b"]);
    ok(root, "root", &["clone", url, "r"]);
}

fn start(t: &Path) -> Srv {
    std::fs::write(t.join("policy"), "allow * * **\nrequire-lock * * art/**\n").unwrap();
    let data = t.join("data");
    start_server(
        t,
        &["--root", data.to_str().unwrap()],
        &["--policy", t.join("policy").to_str().unwrap()],
        &[],
    )
}

#[test]
fn two_users_racing_for_the_same_file_exactly_one_wins() {
    let t = tempfile::tempdir().unwrap();
    let srv = start(t.path());
    setup(t.path(), &srv.url);
    let (a, b) = (t.path().join("a"), t.path().join("b"));
    for round in 0..3 {
        let (a2, b2) = (a.clone(), b.clone());
        let ha = std::thread::spawn(move || vcs_as(&a2, "alice", "tok-alice", &["lock", "art/hero.psd", "--lease-secs", "3600"]));
        let hb = std::thread::spawn(move || vcs_as(&b2, "bob", "tok-bob", &["lock", "art/hero.psd", "--lease-secs", "3600"]));
        let (ra, rb) = (ha.join().unwrap(), hb.join().unwrap());
        let (wa, wb) = (ra.status.success(), rb.status.success());
        assert!(wa ^ wb, "round {round}: exactly one lock must win (alice {wa}, bob {wb})");
        // the winner's copy is writable, the loser's stays read-only
        assert_eq!(!ro(&a, "art/hero.psd"), wa);
        assert_eq!(!ro(&b, "art/hero.psd"), wb);
        let (winner, wdir, loser, ldir) = if wa { ("alice", &a, "bob", &b) } else { ("bob", &b, "alice", &a) };
        let err = String::from_utf8_lossy(if wa { &rb.stderr } else { &ra.stderr }).into_owned();
        assert!(err.contains(winner), "loser is told who holds it: {err}");
        assert!(ok(ldir, loser, &["locks"]).contains(&format!("{winner}@example.com")) || ok(ldir, loser, &["locks"]).contains(winner));
        ok(wdir, winner, &["unlock", "art/hero.psd"]);
    }
}

#[test]
fn server_require_lock_policy_and_admin_break_end_to_end() {
    let t = tempfile::tempdir().unwrap();
    let srv = start(t.path());
    setup(t.path(), &srv.url);
    let (a, b, r) = (t.path().join("a"), t.path().join("b"), t.path().join("r"));

    // bob sidesteps the client guard; the server's require-lock policy refuses the publish
    write(&b, "art/hero.psd", "bob blind\n");
    ok(&b, "bob", &["add", "--force-no-lock", "art/hero.psd"]);
    ok(&b, "bob", &["commit", "-m", "blind", "--force-no-lock"]);
    let err = fail(&b, "bob", &["publish"]);
    assert!(err.contains("428") && err.contains("hold a lock"), "{err}");

    // with the lock the same commit publishes
    ok(&b, "bob", &["lock", "art/hero.psd", "--lease-secs", "3600"]);
    assert!(ok(&b, "bob", &["publish"]).contains("published"));

    // alice is locked out; a plain write token cannot break bob's lock, an admin token can
    write(&a, "art/hero.psd", "alice\n");
    let err = fail(&a, "alice", &["add", "art/hero.psd"]);
    assert!(err.contains("refusing to stage lockable"), "{err}");
    let err = fail(&a, "alice", &["unlock", "art/hero.psd", "--break"]);
    assert!(err.contains("admin"), "{err}");
    ok(&r, "root", &["unlock", "art/hero.psd", "--break"]);
    assert!(ok(&a, "alice", &["locks"]).contains("no locks"));

    // bob's lock was broken: his next change is refused locally with the reason, and still
    // would be by the server (nothing publishes without a lock)
    write(&b, "art/hero.psd", "bob again\n");
    let err = fail(&b, "bob", &["add", "art/hero.psd"]);
    assert!(err.contains("expired or was broken"), "{err}");
    ok(&b, "bob", &["add", "--force-no-lock", "art/hero.psd"]);
    ok(&b, "bob", &["commit", "-m", "again", "--force-no-lock"]);
    let err = fail(&b, "bob", &["publish"]);
    assert!(err.contains("428") && err.contains("hold a lock"), "{err}");

    // alice takes the lock the broken one left behind and stages her change under it
    ok(&a, "alice", &["lock", "art/hero.psd", "--lease-secs", "3600"]);
    ok(&a, "alice", &["add", "art/hero.psd"]);
    ok(&a, "alice", &["commit", "-m", "alice"]);
}
