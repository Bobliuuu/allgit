//! `vcs merge --no-ff|--squash|--ff-only` and `vcs publish --force|--force-with-lease|--delete`
//! through the real binaries, against a plain directory remote and against `vcs-server` (which
//! audits forced updates). Two users race; the stale one must lose.
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

fn commit_all(dir: &Path, who: &str, msg: &str) {
    ok(dir, who, &["add", "."]);
    ok(dir, who, &["commit", "-m", msg]);
}

fn hex_after(text: &str, marker: &str) -> String {
    let i = text.find(marker).unwrap_or_else(|| panic!("{marker:?} not in {text}")) + marker.len();
    text[i..].split_whitespace().next().unwrap().to_owned()
}

fn scenario(root: &Path, url: &str) {
    let (a, b) = (root.join("a"), root.join("b"));
    ok(root, "alice", &["init", "a"]);
    write(&a, "f.txt", "one\n");
    ok(&a, "alice", &["add", "."]);
    ok(&a, "alice", &["commit", "-m", "init"]);
    ok(&a, "alice", &["remote", "set", url]);
    ok(&a, "alice", &["publish"]);
    ok(root, "bob", &["clone", url, "b"]);

    // alice moves on and publishes; bob commits on the old base: a plain publish is refused
    write(&a, "f.txt", "one\nalice\n");
    commit_all(&a, "alice", "alice");
    ok(&a, "alice", &["publish"]);
    write(&b, "f.txt", "one\nbob\n");
    commit_all(&b, "bob", "bob");
    let e = fail(&b, "bob", &["publish"]);
    assert!(e.contains("vcs publish --force"), "{e}");

    // --force is a lease: bob has not fetched alice's head, so it refuses and changes nothing
    let e = fail(&b, "bob", &["publish", "--force"]);
    assert!(e.contains("stale info") && e.contains("Nothing was changed"), "{e}");
    let e = fail(&b, "bob", &["publish", "--force-with-lease=main:0000"]);
    assert!(e.contains("stale lease"), "{e}");
    ok(&a, "alice", &["fetch"]);
    assert!(ok(&a, "alice", &["log", "-n", "1", "--oneline"]).contains("alice"));

    // after fetching, bob knows what he is replacing: the forced publish works and says how to undo it
    ok(&b, "bob", &["fetch"]);
    let out = ok(&b, "bob", &["publish", "--force", "main"]);
    assert!(out.contains("FORCED") && out.contains("--at"), "{out}");
    let old = hex_after(&out, "--at ");
    assert_eq!(old.len(), 64, "{old}");
    let oplog = ok(&b, "bob", &["op", "log"]);
    assert!(oplog.contains("publish --force") && oplog.contains(&old), "{oplog}");
    ok(&b, "bob", &["branch", "rescue", "--at", &old]);
    assert!(ok(&b, "bob", &["branch"]).contains("rescue"));

    // two-user race: alice (who never fetched bob's rewrite) tries to force over it and loses
    write(&a, "a2.txt", "again\n");
    commit_all(&a, "alice", "alice again");
    let e = fail(&a, "alice", &["publish", "--force"]);
    assert!(e.contains("stale info"), "{e}");
    // ...and so does a plain publish
    assert!(fail(&a, "alice", &["publish"]).contains("Cannot publish"));
    // having fetched bob's rewrite, alice may now deliberately replace it (her lease is current)
    ok(&a, "alice", &["fetch"]);
    ok(&b, "bob", &["fetch"]);
    let out = ok(&a, "alice", &["publish", "--force", "main"]);
    assert!(out.contains("FORCED"), "{out}");
    ok(&b, "bob", &["fetch"]);

    // delete a branch: leased against what was last fetched
    ok(&b, "bob", &["sync"]);
    ok(&b, "bob", &["switch", "-c", "topic"]);
    write(&b, "t.txt", "t\n");
    ok(&b, "bob", &["add", "."]);
    ok(&b, "bob", &["commit", "-m", "topic"]);
    ok(&b, "bob", &["publish"]);
    ok(&b, "bob", &["switch", "main"]);
    let out = ok(&b, "bob", &["publish", "--delete", "topic"]);
    assert!(out.contains("deleted remote branch") && out.contains("--at"), "{out}");
    assert!(fail(&b, "bob", &["publish", "--delete", "topic"]).contains("no branch"));
}

fn merge_shapes(root: &Path) {
    let d = root.join("m");
    ok(root, "alice", &["init", "m"]);
    write(&d, "f.txt", "base\n");
    ok(&d, "alice", &["add", "."]);
    ok(&d, "alice", &["commit", "-m", "base"]);
    ok(&d, "alice", &["switch", "-c", "feature"]);
    write(&d, "g.txt", "g\n");
    ok(&d, "alice", &["add", "."]);
    ok(&d, "alice", &["commit", "-m", "feat"]);
    ok(&d, "alice", &["switch", "main"]);
    // --squash: staged, branch unmoved, one single-parent commit afterwards
    let out = ok(&d, "alice", &["merge", "--squash", "feature"]);
    assert!(out.contains("squashed") && out.contains("branch not moved"), "{out}");
    assert!(ok(&d, "alice", &["status"]).contains("g.txt"));
    assert_eq!(ok(&d, "alice", &["log", "-n", "5", "--oneline"]).lines().count(), 1);
    ok(&d, "alice", &["commit", "-m", "squashed"]);
    assert_eq!(ok(&d, "alice", &["log", "-n", "5", "--oneline"]).lines().count(), 2);
    // --ff-only refuses on divergence; --no-ff makes a merge commit
    write(&d, "h.txt", "h\n");
    ok(&d, "alice", &["add", "."]);
    ok(&d, "alice", &["commit", "-m", "main moves"]);
    assert!(fail(&d, "alice", &["merge", "--ff-only", "feature"]).contains("cannot fast-forward"));
    let out = ok(&d, "alice", &["merge", "--no-ff", "-m", "join", "feature"]);
    assert!(out.contains("merged"), "{out}");
    assert!(ok(&d, "alice", &["log", "-n", "1", "--oneline"]).contains("join"));
}

#[test]
fn merge_shapes_through_the_cli() {
    let t = tempfile::tempdir().unwrap();
    merge_shapes(t.path());
}

#[test]
fn force_lease_delete_and_race_on_a_directory_remote() {
    let t = tempfile::tempdir().unwrap();
    let remote = t.path().join("remote");
    std::fs::create_dir_all(&remote).unwrap();
    scenario(t.path(), remote.to_str().unwrap());
}

#[test]
fn force_lease_delete_and_race_through_vcs_server_with_audit() {
    let t = tempfile::tempdir().unwrap();
    let store = t.path().join("store");
    std::fs::create_dir_all(&store).unwrap();
    let audit = t.path().join("audit.jsonl");
    let srv = start_server(
        t.path(),
        &["--root", store.to_str().unwrap()],
        &["--audit-log", audit.to_str().unwrap()],
        &[],
    );
    scenario(t.path(), &srv.url);
    let text = std::fs::read_to_string(&audit).unwrap();
    let force = text
        .lines()
        .find(|l| l.contains("\"op\":\"FORCE\""))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(force.contains("\"principal\":\"bob\"") && force.contains("main"), "{force}");
    assert!(text.lines().any(|l| l.contains("DELETE-BRANCH") && l.contains("topic")), "{text}");
    let forces: Vec<&str> = text.lines().filter(|l| l.contains("\"op\":\"FORCE\"")).collect();
    assert!(
        forces.len() == 2 && forces[1].contains("\"principal\":\"alice\""),
        "stale forces must not reach the audit log: {text}"
    );
}
