//! Peering end to end: client -> edge peer -> regional peer -> origin, all real `vcs-server`
//! processes (`--upstream`), driven by the real `vcs` CLI (docs/peering.md).
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

fn peer_args(url: &str, cache: &Path, extra: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = [
        "--upstream",
        url,
        "--upstream-token",
        "tok-alice",
        "--cache-dir",
        cache.to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

#[test]
fn client_edge_regional_origin_chain_and_origin_outage() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    let mk = |n: &str| {
        let d = root.join(n);
        std::fs::create_dir_all(&d).unwrap();
        d
    };
    let mut origin = start_server(&mk("o"), &["--root", mk("odata").to_str().unwrap()], &[], &[]);
    let regional_dir = mk("r");
    let ra = peer_args(&origin.url, &mk("rcache"), &["--upstream-stale-if-error", "300"]);
    let ra: Vec<&str> = ra.iter().map(String::as_str).collect();
    let regional = start_server(&regional_dir, &[], &ra, &[]);
    let edge_dir = mk("e");
    let ea = peer_args(&regional.url, &mk("ecache"), &["--upstream-stale-if-error", "300"]);
    let ea: Vec<&str> = ea.iter().map(String::as_str).collect();
    let edge = start_server(&edge_dir, &[], &ea, &[]);
    let url = edge.url.clone();

    // alice publishes THROUGH the edge: the manifest CAS is decided by the origin
    let a = root.join("a");
    ok(root, "alice", &["init", "a"]);
    write(&a, "f.txt", "v1\n");
    write(&a, "dir/g.bin", "payload\n");
    ok(&a, "alice", &["add", "."]);
    ok(&a, "alice", &["commit", "-m", "init"]);
    ok(&a, "alice", &["remote", "set", &url]);
    assert!(ok(&a, "alice", &["publish"]).contains("published"));
    // bob clones through the edge and gets exactly the content
    ok(root, "bob", &["clone", &url, "b"]);
    let b = root.join("b");
    assert_eq!(std::fs::read_to_string(b.join("f.txt")).unwrap(), "v1\n");
    assert_eq!(std::fs::read_to_string(b.join("dir/g.bin")).unwrap(), "payload\n");
    // a publish made directly at the origin is visible through both peers at once
    let a2 = root.join("a2");
    ok(root, "alice", &["clone", &origin.url, "a2"]);
    write(&a2, "f.txt", "v2 via origin\n");
    ok(&a2, "alice", &["add", "."]);
    ok(&a2, "alice", &["commit", "-m", "v2"]);
    assert!(ok(&a2, "alice", &["publish"]).contains("published"));
    assert!(ok(&b, "bob", &["fetch"]).contains("fetched"));
    ok(&b, "bob", &["sync"]);
    assert_eq!(std::fs::read_to_string(b.join("f.txt")).unwrap(), "v2 via origin\n");
    // push through the edge, then read it back through the edge: never stale
    write(&b, "f.txt", "v3 via edge\n");
    ok(&b, "bob", &["add", "."]);
    ok(&b, "bob", &["commit", "-m", "v3"]);
    assert!(ok(&b, "bob", &["publish"]).contains("published"));
    ok(&a2, "alice", &["fetch"]);
    ok(&a2, "alice", &["sync"]);
    assert_eq!(std::fs::read_to_string(a2.join("f.txt")).unwrap(), "v3 via edge\n");
    ok(&a, "alice", &["fetch"]);
    ok(&a, "alice", &["sync"]);
    assert_eq!(std::fs::read_to_string(a.join("f.txt")).unwrap(), "v3 via edge\n");

    // warm the edge with the final state, then:
    ok(root, "bob", &["clone", &url, "warm"]);
    // origin goes away: a warm clone through the edge still works, a publish fails cleanly
    let _ = origin.child.kill();
    let _ = origin.child.wait();
    ok(root, "bob", &["clone", &url, "c"]);
    assert_eq!(std::fs::read_to_string(root.join("c/f.txt")).unwrap(), "v3 via edge\n");
    write(&b, "f.txt", "v4 must not be accepted\n");
    ok(&b, "bob", &["add", "."]);
    ok(&b, "bob", &["commit", "-m", "v4"]);
    let err = fail(&b, "bob", &["publish"]);
    assert!(!err.is_empty() && !err.contains("panicked"), "{err}");
}
