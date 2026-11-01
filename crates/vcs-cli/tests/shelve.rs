//! Shelves (docs/shelves.md) through the real binaries. Setup helpers as in server_e2e.rs.
//! End to end through the real binaries: `vcs-server` on an ephemeral port, the `vcs` CLI
//! talking to it over `http://` with `VCS_TOKEN`. The same scenario runs against a directory
//! backend and, when `VCS_TEST_S3_ENDPOINT` is set (docs/deploy.md), an S3 backend
//! (`vcs-server --backend s3://...`).
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
fn write(dir: &Path, f: &str, c: &str) {
    let p = dir.join(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}

fn file_ok(dir: &Path, who: &str, args: &[&str]) -> String {
    ok_tok(dir, who, "unused", args)
}
fn ok_tok(dir: &Path, who: &str, tok: &str, args: &[&str]) -> String {
    let o = vcs_as(dir, who, tok, args);
    assert!(
        o.status.success(),
        "vcs {args:?} as {who} failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn file_fail(dir: &Path, who: &str, args: &[&str]) -> String {
    let o = vcs_as(dir, who, "unused", args);
    assert!(!o.status.success(), "vcs {args:?} as {who} unexpectedly succeeded");
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn read(p: PathBuf) -> String {
    std::fs::read_to_string(p).unwrap()
}

#[test]
fn shelve_and_unshelve_over_a_file_remote() {
    let t = tempfile::tempdir().unwrap();
    let remote = t.path().join("remote");
    shelve_scenario(t.path(), remote.to_str().unwrap());
}

/// Gated on `VCS_TEST_S3_ENDPOINT` (docs/deploy.md): the same scenario with the shelves stored in
/// an S3 bucket (RustFS / MinIO / moto), directly from the CLI.
#[test]
fn shelve_and_unshelve_over_s3() {
    let Some((url, ak, sk, _)) = s3_env("shelve-direct") else { return };
    std::env::set_var("AWS_ACCESS_KEY_ID", ak);
    std::env::set_var("AWS_SECRET_ACCESS_KEY", sk);
    let t = tempfile::tempdir().unwrap();
    shelve_scenario(t.path(), &url);
}

/// `(s3 url, access key, secret key, bucket)` for a fresh prefix, or None when no S3 server is configured.
fn s3_env(tag: &str) -> Option<(String, String, String, String)> {
    let Ok(ep) = std::env::var("VCS_TEST_S3_ENDPOINT") else {
        eprintln!("skipping: VCS_TEST_S3_ENDPOINT not set (see docs/deploy.md)");
        return None;
    };
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
    let (ak, sk) = (var("VCS_TEST_S3_ACCESS_KEY", "minioadmin"), var("VCS_TEST_S3_SECRET_KEY", "minioadmin"));
    let bucket = var("VCS_TEST_S3_BUCKET", "vcs-test");
    vcs_store_s3::S3Backend::new_unchecked(vcs_store_s3::S3Config::new(&ep, &bucket, &ak, &sk))
        .unwrap()
        .ensure_bucket()
        .unwrap();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    Some((format!("s3://{bucket}/{tag}-{nanos}?endpoint={ep}"), ak, sk, bucket))
}

fn shelve_scenario(root: &Path, url: &str) {
    let (a, b) = (root.join("a"), root.join("b"));
    file_ok(root, "alice", &["init", "a"]);
    write(&a, "src/lib.rs", "line1\nline2\nline3\n");
    write(&a, "notes.txt", "n1\n");
    file_ok(&a, "alice", &["add", "."]);
    file_ok(&a, "alice", &["commit", "-m", "init"]);
    file_ok(&a, "alice", &["remote", "set", url]);
    file_ok(&a, "alice", &["publish"]);
    file_ok(root, "bob", &["clone", url, "b"]);

    // nothing to shelve yet
    assert!(file_fail(&a, "alice", &["shelve", "w1"]).contains("no local changes"));
    // alice edits, adds a new file, and shelves; her working tree is untouched
    write(&a, "src/lib.rs", "line1 ALICE\nline2\nline3\n");
    write(&a, "new.txt", "brand new\n");
    file_ok(&a, "alice", &["add", "new.txt"]);
    let out = file_ok(&a, "alice", &["shelve", "-m", "half done", "--expires", "30d", "w1"]);
    assert!(out.contains("alice/w1"), "{out}");
    assert_eq!(read(a.join("src/lib.rs")), "line1 ALICE\nline2\nline3\n");
    // the manifest was not touched: publish still has nothing new to say about the remote head
    let listing = file_ok(&a, "alice", &["shelves"]);
    assert!(
        listing.contains("alice/w1") && listing.contains("half done") && listing.contains("expires in"),
        "{listing}"
    );
    assert!(file_fail(&a, "alice", &["shelve", "w1"]).contains("already exists"));
    assert!(file_fail(&a, "alice", &["shelve", "bad name"]).contains("shelf name"));
    // bob sees it with --all but not without
    assert!(file_ok(&b, "bob", &["shelves"]).contains("no shelves"));
    assert!(file_ok(&b, "bob", &["shelves", "--all"]).contains("alice/w1"));
    // bob unshelves it
    let out = file_ok(&b, "bob", &["unshelve", "alice/w1"]);
    assert!(out.contains("applied shelf alice/w1"), "{out}");
    assert_eq!(read(b.join("src/lib.rs")), "line1 ALICE\nline2\nline3\n");
    assert_eq!(read(b.join("new.txt")), "brand new\n");
    let st = file_ok(&b, "bob", &["status"]);
    assert!(st.contains("new.txt") && st.contains("src/lib.rs"), "{st}");
    // unshelving over a dirty zone is refused like stash pop
    assert!(!file_fail(&b, "bob", &["unshelve", "alice/w1"]).is_empty());
    // a conflicting unshelve leaves markers and keeps the shelf
    file_ok(&b, "bob", &["stash", "push"]);
    write(&b, "src/lib.rs", "line1 BOB\nline2\nline3\n");
    file_ok(&b, "bob", &["add", "."]);
    file_ok(&b, "bob", &["commit", "-m", "bob edit"]);
    let out = file_ok(&b, "bob", &["unshelve", "alice/w1"]);
    assert!(out.contains("conflicts in 1 file"), "{out}");
    assert!(read(b.join("src/lib.rs")).contains("<<<<<<<"));
    assert!(file_ok(&b, "bob", &["shelves", "--all"]).contains("alice/w1"));
    // delete; deleting a missing shelf is an error
    file_ok(&a, "alice", &["shelve", "--delete", "w1"]);
    assert!(file_ok(&a, "alice", &["shelves"]).contains("no shelves"));
    assert!(!file_fail(&a, "alice", &["shelve", "--delete", "w1"]).is_empty());
    assert!(file_fail(&b, "bob", &["unshelve", "alice/w1"]).contains("no such shelf"));
}

#[test]
fn a_corrupted_shelf_is_refused_before_anything_is_applied() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    let remote = root.join("remote");
    let url = remote.to_str().unwrap();
    let (a, b) = (root.join("a"), root.join("b"));
    file_ok(root, "alice", &["init", "a"]);
    write(&a, "f.txt", "one\n");
    file_ok(&a, "alice", &["add", "."]);
    file_ok(&a, "alice", &["commit", "-m", "init"]);
    file_ok(&a, "alice", &["remote", "set", url]);
    file_ok(&a, "alice", &["publish"]);
    file_ok(root, "bob", &["clone", url, "b"]);
    write(&a, "f.txt", "two\n");
    file_ok(&a, "alice", &["shelve", "w"]);
    let pack = remote.join("shelves/alice/w/pack");
    let mut bytes = std::fs::read(&pack).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&pack, bytes).unwrap();
    let err = file_fail(&b, "bob", &["unshelve", "alice/w"]);
    assert!(err.contains("invalid shelf"), "{err}");
    assert_eq!(read(b.join("f.txt")), "one\n");
}

#[test]
fn shelves_on_vcs_server_are_private_until_a_shelve_read_rule() {
    let t = tempfile::tempdir().unwrap();
    let data = t.path().join("data");
    private_shelves(t.path(), &["--root", data.to_str().unwrap()], &[]);
}

/// Gated on `VCS_TEST_S3_ENDPOINT`: the same privacy rules with `vcs-server --backend s3://...`.
#[test]
fn shelves_on_vcs_server_over_s3_are_private_until_a_shelve_read_rule() {
    let Some((url, ak, sk, _)) = s3_env("shelve-srv") else { return };
    let t = tempfile::tempdir().unwrap();
    private_shelves(t.path(), &["--backend", &url], &[("AWS_ACCESS_KEY_ID", ak), ("AWS_SECRET_ACCESS_KEY", sk)]);
}

fn private_shelves(root: &Path, backend: &[&str], envs: &[(&str, String)]) {
    let pol = root.join("policy");
    std::fs::write(&pol, "shelve-read bob alice **\n").unwrap();
    let srv = start_server(root, backend, &["--policy", pol.to_str().unwrap()], envs);
    let url = srv.url.as_str();
    let (a, b, c) = (root.join("a"), root.join("b"), root.join("c"));
    ok_tok(root, "alice", "tok-alice", &["init", "a"]);
    write(&a, "f.txt", "one\n");
    ok_tok(&a, "alice", "tok-alice", &["add", "."]);
    ok_tok(&a, "alice", "tok-alice", &["commit", "-m", "init"]);
    ok_tok(&a, "alice", "tok-alice", &["remote", "set", url]);
    ok_tok(&a, "alice", "tok-alice", &["publish"]);
    ok_tok(root, "bob", "tok-bob", &["clone", url, "b"]);
    // carol-like second write principal without a rule: use rita (read) and a fresh bob2 clone
    ok_tok(root, "rita", "tok-rita", &["clone", url, "c"]);

    write(&a, "f.txt", "two\n");
    write(&a, "g.txt", "g\n");
    ok_tok(&a, "alice", "tok-alice", &["add", "g.txt"]);
    assert!(ok_tok(&a, "alice", "tok-alice", &["shelve", "w"]).contains("alice/w"));
    assert!(ok_tok(&a, "alice", "tok-alice", &["shelves"]).contains("alice/w"));
    // bob has a shelve-read rule for alice: sees and unshelves it, but cannot delete it
    assert!(ok_tok(&b, "bob", "tok-bob", &["shelves", "--all"]).contains("alice/w"));
    ok_tok(&b, "bob", "tok-bob", &["unshelve", "alice/w"]);
    assert_eq!(read(b.join("f.txt")), "two\n");
    let err = vcs_as(&b, "bob", "tok-bob", &["shelve", "--delete", "alice/w"]);
    assert!(
        !err.status.success() && String::from_utf8_lossy(&err.stderr).contains("403"),
        "{}",
        String::from_utf8_lossy(&err.stderr)
    );
    // rita has no rule: sees nothing, cannot read it, cannot create one (read-only token)
    assert!(ok_tok(&c, "rita", "tok-rita", &["shelves", "--all"]).contains("no shelves"));
    let o = vcs_as(&c, "rita", "tok-rita", &["unshelve", "alice/w"]);
    assert!(
        !o.status.success() && String::from_utf8_lossy(&o.stderr).contains("no such shelf"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    write(&c, "f.txt", "rita\n");
    let o = vcs_as(&c, "rita", "tok-rita", &["shelve", "r"]);
    assert!(
        !o.status.success() && String::from_utf8_lossy(&o.stderr).contains("403"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    // bob cannot write under alice's prefix even by hand, but may shelve under his own name
    write(&b, "f.txt", "bob wip\n");
    ok_tok(&b, "bob", "tok-bob", &["stash", "push"]);
    write(&b, "f.txt", "bob wip\n");
    assert!(ok_tok(&b, "bob", "tok-bob", &["shelve", "mine"]).contains("bob/mine"));
    assert!(!ok_tok(&a, "alice", "tok-alice", &["shelves", "--all"]).contains("bob/mine"));
    // an admin can delete anyone's shelf
    ok_tok(&a, "root", "tok-root", &["shelve", "--delete", "alice/w"]);
    assert!(ok_tok(&a, "alice", "tok-alice", &["shelves"]).contains("no shelves"));
    ok_tok(&b, "bob", "tok-bob", &["shelve", "--delete", "mine"]);
}
