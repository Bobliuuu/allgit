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
    std::fs::write(p, c).unwrap();
}

fn scenario(root: &Path, url: &str) {
    let (a, b) = (root.join("a"), root.join("b"));
    ok(root, "alice", &["init", "a"]);
    write(&a, "art/hero.bin", "v1\n");
    write(&a, "notes.txt", "n1\n");
    ok(&a, "alice", &["add", "."]);
    ok(&a, "alice", &["commit", "-m", "init"]);
    assert!(ok(&a, "alice", &["remote", "set", url]).contains(url));
    assert!(ok(&a, "alice", &["publish"]).contains("published"));
    // the token never lands in repository metadata
    for e in std::fs::read_dir(a.join(".vcs")).unwrap() {
        let p = e.unwrap().path();
        if p.is_file() {
            assert!(!String::from_utf8_lossy(&std::fs::read(&p).unwrap()).contains("tok-alice"), "{p:?}");
        }
    }

    // bob clones and sees the files
    ok(root, "bob", &["clone", url, "b"]);
    assert_eq!(std::fs::read_to_string(b.join("art/hero.bin")).unwrap(), "v1\n");

    // alice locks; bob sees it, and his publish touching the locked file is refused
    let out = ok(
        &a,
        "alice",
        &["lock", "art/hero.bin", "--lease-secs", "3600", "--reason", "repaint"],
    );
    assert!(out.contains("locked art/hero.bin"), "{out}");
    let listing = ok(&b, "bob", &["locks"]);
    assert!(
        listing.contains("art/hero.bin") && listing.contains("alice") && listing.contains("repaint"),
        "{listing}"
    );
    write(&b, "art/hero.bin", "bob's version\n");
    write(&b, "notes.txt", "n2\n");
    ok(&b, "bob", &["add", "."]);
    ok(&b, "bob", &["commit", "-m", "bob edits"]);
    let err = fail(&b, "bob", &["publish"]);
    assert!(err.contains("art/hero.bin") && err.contains("alice"), "{err}");

    // alice publishes her own change to the locked file, then unlocks
    write(&a, "art/hero.bin", "v2 by alice\n");
    ok(&a, "alice", &["add", "."]);
    ok(&a, "alice", &["commit", "-m", "alice repaint"]);
    assert!(ok(&a, "alice", &["publish"]).contains("published"));
    assert!(fail(&b, "bob", &["unlock", "art/hero.bin"]).contains("only the holder"));
    ok(&a, "alice", &["unlock", "art/hero.bin"]);
    assert!(ok(&b, "bob", &["locks"]).contains("no locks"));

    // bob fetches and syncs alice's work (his local commit diverged on hero.bin)
    assert!(ok(&b, "bob", &["fetch"]).contains("fetched"));
    ok(&b, "bob", &["sync"]);
    ok(&a, "alice", &["fetch"]);
    ok(&a, "alice", &["sync"]);

    // auth: a wrong token is refused (401), a read-only token can fetch but not publish
    let o = vcs_as(&b, "bob", "wrong", &["fetch"]);
    assert!(
        !o.status.success() && String::from_utf8_lossy(&o.stderr).contains("401"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(["fetch"])
        .current_dir(&b)
        .env_remove("VCS_TOKEN")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&o.stderr).contains("VCS_TOKEN"));
    let rita = root.join("rita");
    let o = vcs_as(root, "rita", "tok-rita", &["clone", url, "rita"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    write(&rita, "x.txt", "x\n");
    for args in [&["add", "."][..], &["commit", "-m", "x"]] {
        assert!(vcs_as(&rita, "rita", "tok-rita", args).status.success());
    }
    let o = vcs_as(&rita, "rita", "tok-rita", &["publish"]);
    assert!(
        !o.status.success() && String::from_utf8_lossy(&o.stderr).contains("403"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn cli_workflow_against_vcs_server_on_a_directory() {
    let t = tempfile::tempdir().unwrap();
    let data = t.path().join("data");
    let srv = start_server(t.path(), &["--root", data.to_str().unwrap()], &[], &[]);
    scenario(t.path(), &srv.url);
}

#[test]
fn cli_workflow_against_vcs_server_on_s3() {
    let Ok(ep) = std::env::var("VCS_TEST_S3_ENDPOINT") else {
        eprintln!("skipping: VCS_TEST_S3_ENDPOINT not set (see docs/deploy.md)");
        return;
    };
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
    let (ak, sk) = (
        var("VCS_TEST_S3_ACCESS_KEY", "minioadmin"),
        var("VCS_TEST_S3_SECRET_KEY", "minioadmin"),
    );
    let bucket = var("VCS_TEST_S3_BUCKET", "vcs-test");
    let cfg = vcs_store_s3::S3Config::new(&ep, &bucket, &ak, &sk);
    vcs_store_s3::S3Backend::new_unchecked(cfg).unwrap().ensure_bucket().unwrap();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let backend = format!("s3://{bucket}/srv-e2e-{nanos}?endpoint={ep}");
    let t = tempfile::tempdir().unwrap();
    let srv = start_server(
        t.path(),
        &["--backend", &backend],
        &[],
        &[("AWS_ACCESS_KEY_ID", ak), ("AWS_SECRET_ACCESS_KEY", sk)],
    );
    scenario(t.path(), &srv.url);
}
