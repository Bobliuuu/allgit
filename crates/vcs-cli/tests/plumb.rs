//! `rev-parse`, `ls-files`, `cat-file`, `shortlog` (text and --json).
use serde_json::{json, Value};
use std::path::Path;
use std::process::{Command, Output};

fn vcs_as(dir: &Path, who: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", who)
        .env("VCS_AUTHOR_EMAIL", format!("{who}@example.com"))
        .env("VCS_TIMESTAMP", "1700000000")
        .output()
        .unwrap()
}
fn ok_as(dir: &Path, who: &str, args: &[&str]) -> String {
    let o = vcs_as(dir, who, args);
    assert!(
        o.status.success(),
        "vcs {args:?} failed: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn ok(dir: &Path, args: &[&str]) -> String {
    ok_as(dir, "Dev", args)
}
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
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("a.txt"), "one\n").unwrap();
    std::fs::write(d.join("sub/b.txt"), "bee\n").unwrap();
    ok(&d, &["add", "."]);
    ok_as(&d, "Ann", &["commit", "-m", "first"]);
    std::fs::write(d.join("a.txt"), "two\n").unwrap();
    ok(&d, &["add", "a.txt"]);
    ok_as(&d, "Bob", &["commit", "-m", "second"]);
    std::fs::write(d.join("a.txt"), "three\n").unwrap();
    ok(&d, &["add", "a.txt"]);
    ok_as(&d, "Ann", &["commit", "-m", "third"]);
    (t, d)
}

#[test]
fn rev_parse() {
    let (_t, d) = repo();
    let head = ok(&d, &["rev-parse", "HEAD"]).trim().to_owned();
    assert_eq!(head.len(), 64);
    let short = ok(&d, &["rev-parse", "--short", "HEAD"]);
    assert!(head.starts_with(short.trim()) && short.trim().len() < 64);
    let prev = ok(&d, &["rev-parse", "HEAD~1"]).trim().to_owned();
    assert_ne!(prev, head);
    assert_eq!(ok(&d, &["rev-parse", "HEAD", &prev[..10]]), format!("{head}\n{prev}\n"));
    assert!(!vcs_as(&d, "Dev", &["rev-parse", "nope"]).status.success());
    assert_eq!(
        ok(&d, &["rev-parse", "--show-toplevel"]).trim(),
        d.canonicalize().unwrap().to_str().unwrap()
    );
    let v = js(&d, &["rev-parse", "HEAD"]);
    assert_eq!(v["ids"], json!([head]));
    assert_eq!(v["command"], "rev-parse");
    assert_eq!(v["version"], 1);
}

#[test]
fn ls_files_text_and_json() {
    let (_t, d) = repo();
    assert_eq!(ok(&d, &["ls-files"]), "a.txt\nsub/b.txt\n");
    assert_eq!(ok(&d, &["ls-files", "sub"]), "sub/b.txt\n");
    let s = ok(&d, &["ls-files", "-s", "a.txt"]);
    assert!(s.starts_with("100644 ") && s.trim_end().ends_with(" a.txt"), "{s}");
    assert_eq!(ok(&d, &["ls-files", "--rev", "HEAD~2"]), "a.txt\nsub/b.txt\n");
    let v = js(&d, &["ls-files"]);
    assert_eq!(v["files"][1]["path"], "sub/b.txt");
    assert_eq!(v["files"][0]["mode"], "100644");
    assert_eq!(v["files"][0]["id"].as_str().unwrap().len(), 64);
}

#[test]
fn cat_file() {
    let (_t, d) = repo();
    assert_eq!(ok(&d, &["cat-file", "-p", "HEAD:a.txt"]), "three\n");
    assert_eq!(ok(&d, &["cat-file", "-p", "HEAD~2:a.txt"]), "one\n");
    assert_eq!(ok(&d, &["cat-file", "-t", "HEAD:a.txt"]), "blob\n");
    assert_eq!(ok(&d, &["cat-file", "-s", "HEAD:sub/b.txt"]), "4\n");
    assert_eq!(ok(&d, &["cat-file", "-t", "HEAD"]), "commit\n");
    let p = ok(&d, &["cat-file", "-p", "HEAD"]);
    assert!(p.contains("author Ann <Ann@example.com>") && p.contains("\nthird"), "{p}");
    let blob = ok(&d, &["ls-files", "-s", "a.txt"]).split(' ').nth(1).unwrap().to_owned();
    assert_eq!(ok(&d, &["cat-file", "-p", &blob]), "three\n");
    assert!(!vcs_as(&d, "Dev", &["cat-file", "-p", "HEAD:missing"]).status.success());
    assert!(!vcs_as(&d, "Dev", &["cat-file", "HEAD"]).status.success());
    let v = js(&d, &["cat-file", "-p", "HEAD:a.txt"]);
    assert_eq!(v["type"], "blob");
    assert_eq!(v["size"], 6);
    assert_eq!(v["content"], "three\n");
}

#[test]
fn shortlog() {
    let (_t, d) = repo();
    assert_eq!(
        ok(&d, &["shortlog"]),
        "Ann (2):\n      third\n      first\n\nBob (1):\n      second\n\n"
    );
    assert_eq!(ok(&d, &["shortlog", "-s", "-n"]), "     2\tAnn\n     1\tBob\n");
    assert!(ok(&d, &["shortlog", "-s", "-e"]).contains("Bob <Bob@example.com>"));
    let v = js(&d, &["shortlog"]);
    assert_eq!(v["authors"][0]["author"], "Ann");
    assert_eq!(v["authors"][0]["count"], 2);
    assert_eq!(v["authors"][0]["subjects"], json!(["third", "first"]));
}

#[test]
fn ls_files_depth_one_lists_the_top_tree_only() {
    let (_t, d) = repo();
    let flat = ok(&d, &["ls-files", "-s", "--rev", "HEAD"]);
    let top = ok(&d, &["ls-files", "-s", "--rev", "HEAD", "--depth", "1"]);
    let lines: Vec<&str> = top.lines().collect();
    assert_eq!(lines.len(), 2, "{top}");
    // the file entry is the same as in the recursive listing, the directory is a tree entry
    let file = flat.lines().find(|l| l.ends_with(" a.txt")).unwrap();
    assert!(lines.contains(&file));
    let dir = lines.iter().find(|l| l.ends_with(" sub")).unwrap();
    assert!(dir.starts_with("040000 "), "{dir}");
    assert_eq!(ok(&d, &["ls-files", "--rev", "HEAD", "--depth", "1"]), "a.txt\nsub\n");
    // the git spelling maps onto it; -r keeps the full listing
    assert_eq!(ok(&d, &["git", "ls-tree", "--name-only", "HEAD"]), "a.txt\nsub\n");
    assert_eq!(ok(&d, &["git", "ls-tree", "-r", "--name-only", "HEAD"]), "a.txt\nsub/b.txt\n");
    let v = js(&d, &["ls-files", "--rev", "HEAD", "--depth", "1"]);
    assert_eq!(v["files"].as_array().unwrap().len(), 2);
    // depth needs --rev, takes no path and only knows depth 1
    assert!(!vcs_as(&d, "Dev", &["ls-files", "--depth", "1"]).status.success());
    assert!(!vcs_as(&d, "Dev", &["ls-files", "--rev", "HEAD", "--depth", "2"]).status.success());
    assert!(!vcs_as(&d, "Dev", &["ls-files", "--rev", "HEAD", "--depth", "1", "sub"])
        .status
        .success());
}

/// `ls-files --rev` must equal the index listing and the `--json` path, including names where
/// tree order is not path order
/// (`a/x` is stored before `a.b` in a tree but sorts after it as a path) and with several zones.
#[test]
fn ls_files_rev_stream_matches_the_index_and_the_json_list() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    ok(&d, &["zone", "create", "engine", "--path", "engine"]);
    for p in [
        "a/x",
        "a.b",
        "a-c",
        "a/y/z",
        "a/y.w",
        "b",
        "engine/core.rs",
        "engine/e/f",
        "engine/e.g",
        "z/1",
        "z0",
    ] {
        let f = d.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, format!("{p}\n")).unwrap();
    }
    ok(&d, &["add", "."]);
    ok(&d, &["commit", "-m", "tricky"]);
    for extra in [&[][..], &["-s"][..], &["a"][..], &["-s", "engine"][..], &["a/y", "z"][..]] {
        let mut idx = vec!["ls-files"];
        idx.extend_from_slice(extra);
        let mut rev = vec!["ls-files", "--rev", "HEAD"];
        rev.extend_from_slice(extra);
        assert_eq!(ok(&d, &rev), ok(&d, &idx), "{extra:?}");
    }
    let v = js(&d, &["ls-files", "--rev", "HEAD"]);
    let from_json: Vec<String> = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_owned())
        .collect();
    let text = ok(&d, &["ls-files", "--rev", "HEAD"]);
    assert_eq!(text.lines().map(str::to_owned).collect::<Vec<_>>(), from_json);
    assert!(from_json.windows(2).all(|w| w[0] < w[1]), "{from_json:?}");
}
