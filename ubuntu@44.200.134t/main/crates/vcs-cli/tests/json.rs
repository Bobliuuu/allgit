//! `--json` output: golden documents and parse-back checks for each schema (see docs/cli.md).
use serde_json::{json, Value};
use std::path::Path;
use std::process::{Command, Output};

fn vcs(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_TIMESTAMP", "1700000000")
        .output()
        .unwrap()
}
fn ok(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(o.status.success(), "vcs {args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}
/// Run with --json: stdout must be exactly one JSON document and nothing else.
fn js(dir: &Path, args: &[&str]) -> Value {
    let mut a = args.to_vec();
    a.push("--json");
    let out = ok(dir, &a);
    assert_eq!(out.matches('\n').count(), 1, "one line: {out:?}");
    serde_json::from_str(&out).unwrap()
}

fn repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    (t, d)
}

#[test]
fn status_golden() {
    let (_t, d) = repo();
    std::fs::write(d.join("a.txt"), "a\n").unwrap();
    let v = js(&d, &["status"]);
    assert_eq!(v["untracked"], json!(["a.txt"]));
    ok(&d, &["add", "a.txt"]);
    ok(&d, &["commit", "-m", "first"]);
    std::fs::write(d.join("a.txt"), "a\nb\n").unwrap();
    std::fs::write(d.join("new.txt"), "n\n").unwrap();
    ok(&d, &["add", "new.txt"]);
    let mut v = js(&d, &["status"]);
    let head = v["zones"][0]["head"].as_str().unwrap().to_owned();
    assert_eq!(head.len(), 64.min(head.len()));
    v["zones"][0]["head"] = json!("HEAD");
    assert_eq!(
        v,
        json!({
            "version": 1, "command": "status",
            "zones": [{
                "zone": "root", "zone_id": v["zones"][0]["zone_id"], "path": "", "branch": "main", "head": "HEAD",
                "staged": [{"path": "new.txt", "kind": "added"}],
                "unstaged": [{"path": "a.txt", "kind": "modified"}]
            }],
            "untracked": [], "unsubscribed": [], "unsupported": [],
            "merging": null, "rebasing": false, "clean": false
        })
    );
}

#[test]
fn log_branch_zone_op_schemas() {
    let (_t, d) = repo();
    std::fs::write(d.join("a.txt"), "x \"q\"\n").unwrap();
    ok(&d, &["add", "a.txt"]);
    ok(&d, &["commit", "-m", "first \"line\"\nsecond"]);

    let log = js(&d, &["log"]);
    assert_eq!(log["command"], "log");
    let c = &log["commits"][0];
    assert_eq!(c["message"].as_str().unwrap().trim_end(), "first \"line\"\nsecond");
    assert_eq!(
        c["author"],
        json!({"name": "Dev", "email": "dev@example.com", "timestamp": 1700000000})
    );
    assert!(c["id"].as_str().unwrap().len() >= 16 && c["change_id"].is_string());
    assert!(ok(&d, &["log"]).contains(c["id"].as_str().unwrap()));

    let br = js(&d, &["branch"]);
    assert_eq!(br["branches"][0]["name"], "main");
    assert_eq!(br["branches"][0]["current"], true);
    assert_eq!(br["branches"][0]["head"], c["id"]);

    let zl = js(&d, &["zone", "list"]);
    assert_eq!(zl["command"], "zone-list");
    assert_eq!(zl["zones"][0]["subscribed"], true);
    assert_eq!(zl["zones"][0]["branch"], "main");
    assert_eq!(zl["zones"][0]["branch_count"], 1);

    let op = js(&d, &["op", "log"]);
    assert_eq!(op["command"], "op-log");
    assert_eq!(op["operations"][0]["current"], true);
    assert!(op["operations"][0]["description"].is_string());
}

#[test]
fn diff_stat_json_and_text() {
    let (_t, d) = repo();
    std::fs::write(d.join("a.txt"), "1\n2\n").unwrap();
    ok(&d, &["add", "a.txt"]);
    ok(&d, &["commit", "-m", "c"]);
    std::fs::write(d.join("a.txt"), "1\n3\n4\n").unwrap();
    let v = js(&d, &["diff", "--stat"]);
    assert_eq!(
        v,
        json!({"version":1,"command":"diff","staged":false,
               "files":[{"path":"a.txt","added":2,"deleted":1,"binary":false}],
               "added":2,"deleted":1})
    );
    assert_eq!(js(&d, &["diff"]), v);
    let t = ok(&d, &["diff", "--stat"]);
    assert!(t.contains("a.txt | +2 -1") && t.contains("1 file(s) changed, 2 insertion(s), 1 deletion(s)"));
    assert_eq!(js(&d, &["diff", "--staged"])["files"], json!([]));
}

#[test]
fn conflict_list_schema_and_errors_on_stderr() {
    let (_t, d) = repo();
    std::fs::write(d.join("f.txt"), "base\n").unwrap();
    ok(&d, &["add", "f.txt"]);
    ok(&d, &["commit", "-m", "base"]);
    ok(&d, &["branch", "side"]);
    std::fs::write(d.join("f.txt"), "main\n").unwrap();
    ok(&d, &["add", "f.txt"]);
    ok(&d, &["commit", "-m", "main"]);
    ok(&d, &["switch", "side"]);
    std::fs::write(d.join("f.txt"), "side\n").unwrap();
    ok(&d, &["add", "f.txt"]);
    ok(&d, &["commit", "-m", "side"]);

    // no merge yet: error on stderr, empty stdout, nonzero exit
    let o = vcs(&d, &["conflict", "list", "--json"]);
    assert!(!o.status.success() && o.stdout.is_empty());
    assert!(String::from_utf8_lossy(&o.stderr).contains("no merge in progress"));

    assert!(!vcs(&d, &["merge", "main"]).status.success());
    let v = js(&d, &["conflict", "list"]);
    assert_eq!(
        v,
        json!({"version":1,"command":"conflict-list",
               "conflicts":[{"path":"f.txt","kind":"modify/modify","resolved":false}]})
    );
    let s = js(&d, &["status"]);
    assert_eq!(s["merging"]["theirs"], "main");
    assert_eq!(s["merging"]["conflicts"], v["conflicts"]);
}

#[test]
fn unsupported_command_fails_cleanly() {
    let (_t, d) = repo();
    let o = vcs(&d, &["fsck", "--json"]);
    assert!(!o.status.success() && o.stdout.is_empty());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--json is not supported"));
}
