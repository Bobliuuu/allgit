//! `vcs zone suggest|apply|subscribe` and `vcs clone --for` with the real binary on generated
//! multi-zone repositories: dependency closure, cycles, a directory without a manifest, owners.
use std::path::Path;
use std::process::{Command, Output};

fn vcs(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .output()
        .unwrap()
}
fn ok(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(o.status.success(), "vcs {args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn fail(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(
        !o.status.success(),
        "vcs {args:?} unexpectedly succeeded: {}",
        String::from_utf8_lossy(&o.stdout)
    );
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn json(dir: &Path, args: &[&str]) -> serde_json::Value {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    let out = ok(dir, &a);
    serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("bad json {e}: {out}"))
}
fn write(dir: &Path, f: &str, c: &str) {
    let p = dir.join(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}
fn names(v: &serde_json::Value, key: &str) -> Vec<String> {
    let mut o: Vec<String> = v[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|z| z["name"].as_str().unwrap().to_owned())
        .collect();
    o.sort();
    o
}

fn crate_dir(a: &Path, name: &str, deps: &[&str]) {
    let mut toml = format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n");
    for d in deps {
        toml.push_str(&format!("{d} = {{ path = \"../{d}\" }}\n"));
    }
    write(a, &format!("crates/{name}/Cargo.toml"), &toml);
    write(a, &format!("crates/{name}/src/lib.rs"), &format!("pub fn {name}_entry() {{}}\n"));
    write(a, &format!("crates/{name}/src/extra.rs"), "// extra\n");
}

/// A monorepo with a Cargo workspace (app -> core -> util, cyc_a <-> cyc_b), an npm package, a Go
/// service, a docs directory and a `misc` directory with no manifest. One commit.
fn generate(root: &Path) -> std::path::PathBuf {
    ok(root, &["init", "mono"]);
    let a = root.join("mono");
    write(&a, "Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n");
    crate_dir(&a, "app", &["core"]);
    crate_dir(&a, "core", &["util"]);
    crate_dir(&a, "util", &[]);
    crate_dir(&a, "cyc_a", &["cyc_b"]);
    crate_dir(&a, "cyc_b", &["cyc_a"]);
    write(&a, "crates/core/src/ledger.rs", "pub fn compute_balance() -> i64 { 0 }\n");
    write(&a, "web/package.json", "{\"name\":\"web\",\"version\":\"1.0.0\"}\n");
    write(&a, "web/src/index.js", "console.log('hi')\n");
    write(&a, "web/src/login.js", "// login\n");
    write(&a, "services/api/go.mod", "module example.com/api\n");
    write(&a, "services/api/main.go", "package main\n");
    write(&a, "services/api/handler.go", "package main\n");
    for i in 0..4 {
        write(&a, &format!("docs/page{i}.md"), "text\n");
        write(&a, &format!("misc/tool{i}.txt"), "tool\n");
    }
    write(&a, "CODEOWNERS", "/web/ @alice\n/crates/core/ @bob\n");
    write(&a, "README", "hello\n");
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "init"]);
    a
}

#[test]
fn suggest_finds_manifest_zones_cycles_and_hides_weak_ones() {
    let t = tempfile::tempdir().unwrap();
    let a = generate(t.path());
    let before = ok(&a, &["zone", "list"]);
    let v = json(&a, &["zone", "suggest"]);
    assert_eq!(v["command"], "zone suggest");
    assert_eq!(v["history_rewritten"], false);
    let paths: Vec<&str> = v["zones"].as_array().unwrap().iter().map(|z| z["path"].as_str().unwrap()).collect();
    for p in [
        "crates/app",
        "crates/core",
        "crates/util",
        "crates/cyc_a",
        "crates/cyc_b",
        "web",
        "services/api",
    ] {
        assert!(paths.contains(&p), "{p} missing from {paths:?}");
    }
    // docs/ and misc/ only have the weak directory signal: hidden by default, never invented as strong.
    assert!(!paths.contains(&"docs") && !paths.contains(&"misc"), "{paths:?}");
    assert!(v["hidden_low_confidence"].as_u64().unwrap() >= 2);
    let app = v["zones"].as_array().unwrap().iter().find(|z| z["path"] == "crates/app").unwrap();
    assert_eq!(app["depends_on"], serde_json::json!(["core"]));
    assert!(app["signals"][0]["kind"].as_str().unwrap().contains("cargo"));
    let cyc = v["cycles"].as_array().unwrap();
    assert_eq!(cyc.len(), 1, "{cyc:?}");
    assert_eq!(cyc[0].as_array().unwrap().len(), 2);
    // with the weak signals on, the manifest-less directory shows up with low confidence
    let w = json(&a, &["zone", "suggest", "--min-confidence", "0.2"]);
    let weak = w["zones"].as_array().unwrap().iter().find(|z| z["path"] == "misc").unwrap();
    assert!(weak["confidence_pct"].as_u64().unwrap() < 40);
    // read-only
    assert_eq!(ok(&a, &["zone", "list"]), before);
    assert!(ok(&a, &["status"]).contains("nothing") || !ok(&a, &["status"]).contains("modified"));
    // text output works too
    assert!(ok(&a, &["zone", "suggest"]).contains("crates/core"));
}

#[test]
fn apply_splits_without_rewriting_history() {
    let t = tempfile::tempdir().unwrap();
    let a = generate(t.path());
    // The existing creation machinery refuses a path that already has committed files.
    assert!(fail(&a, &["zone", "create", "util", "--path", "crates/util"]).contains("tracked files exist"));
    let old_log = ok(&a, &["log"]);
    assert!(fail(&a, &["zone", "apply", "no/such/dir"]).contains("not a current suggestion"));
    let plan = ok(&a, &["zone", "apply", "crates/util"]);
    assert!(plan.contains("plan (nothing changed)"), "{plan}");
    assert!(!ok(&a, &["zone", "list"]).contains("util"));
    let out = ok(&a, &["zone", "apply", "crates/util", "--yes"]);
    assert!(out.contains("history was not rewritten"), "{out}");
    assert!(ok(&a, &["zone", "list"]).contains("util"));
    // working tree untouched, status clean, repository consistent
    assert_eq!(
        std::fs::read_to_string(a.join("crates/util/src/lib.rs")).unwrap(),
        "pub fn util_entry() {}\n"
    );
    let st = ok(&a, &["status"]);
    assert!(
        !st.contains("new file") && !st.contains("deleted") && !st.contains("modified"),
        "{st}"
    );
    assert!(ok(&a, &["fsck"]).contains("ok"));
    // old commits are still in the root zone's log, plus the split commit on top
    let log = ok(&a, &["log"]);
    assert!(log.contains("init") && log.contains("split crates/util/"), "{log}");
    assert!(log.len() > old_log.len());
    // the new zone starts at the split
    let zl = ok(&a, &["log", "--zone", "util"]);
    assert!(zl.contains("start of zone util"), "{zl}");
    // and it keeps working: edit in the new zone, commit, both zones advance independently
    write(&a, "crates/util/src/lib.rs", "pub fn util_entry() { /* v2 */ }\n");
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "touch util"]);
    assert!(ok(&a, &["log", "--zone", "util"]).contains("touch util"));
    assert!(!ok(&a, &["log"]).contains("touch util"));
    assert!(ok(&a, &["fsck"]).contains("ok"));
    // a dirty root refuses
    write(&a, "README", "dirty\n");
    assert!(fail(&a, &["zone", "apply", "web", "--yes"]).contains("uncommitted"));
}

/// Apply every strong suggestion plus `misc`, publish to a directory remote.
fn publish_split(t: &Path) -> (std::path::PathBuf, String) {
    let a = generate(t);
    for p in [
        "crates/app",
        "crates/core",
        "crates/util",
        "crates/cyc_a",
        "crates/cyc_b",
        "web",
        "services/api",
        "misc",
    ] {
        ok(&a, &["zone", "apply", p, "--yes"]);
    }
    let remote = t.join("remote");
    let r = remote.to_str().unwrap().to_owned();
    ok(&a, &["remote", "set", &r]);
    ok(&a, &["publish"]);
    (a, r)
}

#[test]
fn clone_for_pulls_minimal_closure_and_explains() {
    let t = tempfile::tempdir().unwrap();
    let (_a, r) = publish_split(t.path());
    let w = t.path();
    // dry run: closure app -> core -> util, nothing created on disk
    let v = json(w, &["clone", &r, "dst0", "--for", "crates/app/src/lib.rs", "--dry-run"]);
    assert_eq!(v["dry_run"], true);
    assert_eq!(names(&v, "zones"), ["app", "core", "util"]);
    let util = v["zones"].as_array().unwrap().iter().find(|z| z["name"] == "util").unwrap();
    assert_eq!(util["direct"], false);
    assert!(util["reasons"][0].as_str().unwrap().contains("dependency of core"), "{util}");
    assert!(!w.join("dst0").exists());
    // a path query is "lean": only the zones involved are looked at, so the whole-repository totals are unknown, not zero
    let tot = &v["totals"];
    assert!(tot["files_all"].is_null() && tot["bytes_all"].is_null());
    assert!(tot["files_selected"].as_u64().unwrap() > 0 && tot["bytes_selected"].as_u64().unwrap() > 0);
    assert!(v["not_selected"]
        .as_array()
        .unwrap()
        .iter()
        .any(|z| z["name"] == "web" && z["files"].is_null()));
    assert!(v["planning_cost"]["requests"].as_u64().unwrap() > 0);
    // no dependencies on request
    assert_eq!(
        names(
            &json(w, &["clone", &r, "dstx", "--for", "crates/app", "--dry-run", "--no-deps"]),
            "zones"
        ),
        ["app"]
    );
    // cycle terminates and includes both
    assert_eq!(
        names(&json(w, &["clone", &r, "dstx", "--for", "crates/cyc_a", "--dry-run"]), "zones"),
        ["cyc_a", "cyc_b"]
    );
    // a directory without any manifest still resolves by path
    assert_eq!(
        names(&json(w, &["clone", &r, "dstx", "--for", "misc/tool1.txt", "--dry-run"]), "zones"),
        ["misc"]
    );
    // a zone name
    assert_eq!(
        names(&json(w, &["clone", &r, "dstx", "--for", "web", "--dry-run"]), "zones"),
        ["web"]
    );
    // owners via CODEOWNERS (root-zone file) and task keywords
    let o = json(w, &["clone", &r, "dstx", "--for-owner", "alice", "--dry-run", "--partial"]);
    assert!(names(&o, "zones").contains(&"web".to_owned()), "{o}");
    assert!(o["zones"][0]["reasons"][0].as_str().unwrap().contains("CODEOWNERS"));
    let k = json(
        w,
        &["clone", &r, "dstx", "--task", "fix the login page in the web frontend", "--dry-run"],
    );
    assert_eq!(names(&k, "zones"), ["web"]);
    // keyword/symbol/owner queries need every tree, so the totals are complete and the selection is a strict subset
    let tk = &k["totals"];
    assert!(tk["files_selected"].as_u64().unwrap() < tk["files_all"].as_u64().unwrap());
    assert!(tk["bytes_selected"].as_u64().unwrap() < tk["bytes_all"].as_u64().unwrap());
    assert!(k["zones"][0]["reasons"][0].as_str().unwrap().contains("task keywords"));
    // symbol-ish: needs content, so refuses by default (never guesses) and works with --search-content
    let e = fail(w, &["clone", &r, "dstx", "--for", "compute_balance", "--dry-run"]);
    assert!(e.contains("could not resolve"), "{e}");
    let sc = json(
        w,
        &["clone", &r, "dstx", "--for", "compute_balance", "--search-content", "--dry-run"],
    );
    assert_eq!(names(&sc, "zones"), ["core", "util"]);
    // unknown queries refuse unless --partial
    assert!(fail(w, &["clone", &r, "dstx", "--for", "no/such/thing", "--dry-run"]).contains("could not resolve"));
    let p = json(
        w,
        &[
            "clone",
            &r,
            "dstx",
            "--for",
            "web",
            "--for",
            "no/such/thing",
            "--partial",
            "--dry-run",
        ],
    );
    assert_eq!(names(&p, "zones"), ["web"]);
    assert_eq!(p["unresolved"].as_array().unwrap().len(), 1);
    // the real clone brings only the closure
    let out = ok(w, &["clone", &r, "dst", "--for", "crates/app", "--explain"]);
    assert!(out.contains("because"), "{out}");
    let dst = w.join("dst");
    assert!(
        dst.join("crates/app/src/lib.rs").exists()
            && dst.join("crates/core/src/lib.rs").exists()
            && dst.join("crates/util/src/lib.rs").exists()
    );
    assert!(!dst.join("web").exists() && !dst.join("misc").exists() && !dst.join("crates/cyc_a").exists());
    assert!(ok(&dst, &["fsck"]).contains("ok"));
    // the rest can be added later
    ok(&dst, &["fetch", "--zone", "web"]);
    ok(&dst, &["zone", "add", "web"]);
    assert!(dst.join("web/src/index.js").exists());
}

#[test]
fn subscribe_adds_only_what_is_needed_and_never_removes() {
    let t = tempfile::tempdir().unwrap();
    let (_a, r) = publish_split(t.path());
    let w = t.path();
    ok(w, &["clone", &r, "c", "--zone", "web"]);
    let c = w.join("c");
    let d = json(&c, &["zone", "subscribe", "--for", "crates/app", "--dry-run"]);
    assert_eq!(names(&d, "zones"), ["app", "core", "util"]);
    assert!(!c.join("crates").exists());
    ok(&c, &["zone", "subscribe", "--for", "crates/app"]);
    assert!(c.join("crates/util/src/lib.rs").exists());
    assert!(c.join("web/src/index.js").exists(), "existing subscriptions are kept");
    assert!(!c.join("misc").exists());
    // idempotent
    assert!(ok(&c, &["zone", "subscribe", "--for", "crates/app"]).contains("0 zone(s)"));
    assert!(ok(&c, &["fsck"]).contains("ok"));
}

#[test]
fn cochange_marks_coupled_zones() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let a = t.path().join("r");
    for d in ["alpha", "beta", "gamma"] {
        write(&a, &format!("{d}/package.json"), &format!("{{\"name\":\"{d}\"}}"));
        write(&a, &format!("{d}/a.js"), "0");
        write(&a, &format!("{d}/b.js"), "0");
    }
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "init"]);
    for i in 1..=6 {
        write(&a, "alpha/a.js", &i.to_string());
        write(&a, "beta/a.js", &i.to_string());
        if i % 3 == 0 {
            write(&a, "gamma/a.js", &i.to_string());
        }
        ok(&a, &["add", "."]);
        ok(&a, &["commit", "-m", &format!("c{i}")]);
    }
    let v = json(&a, &["zone", "suggest", "--cochange"]);
    let c = v["coupling"].as_array().unwrap();
    // alpha+beta change together in every commit (100%); gamma joins in 3 of 7 (43%): coupled, but below the cluster bar
    assert_eq!(c.len(), 3, "{v}");
    assert_eq!((c[0]["a"].as_str().unwrap(), c[0]["b"].as_str().unwrap()), ("alpha", "beta"));
    assert_eq!(c[0]["jaccard_pct"], 100);
    assert_eq!(c[1]["jaccard_pct"], 43);
    assert_eq!(v["clusters"], serde_json::json!([["alpha", "beta"]]));
    // without --cochange no history is read
    assert_eq!(json(&a, &["zone", "suggest"])["coupling"].as_array().unwrap().len(), 0);
}

#[test]
fn broken_and_missing_manifests_are_reported_not_fatal() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let a = t.path().join("r");
    write(&a, "bad/package.json", "{ this is not json");
    write(&a, "bad/x.js", "1");
    write(&a, "bad/y.js", "1");
    write(
        &a,
        "cargo1/Cargo.toml",
        "[package]\nname=\"c1\"\n[dependencies]\nghost = { path = \"../ghost\" }\n",
    );
    write(&a, "cargo1/src/lib.rs", "1");
    write(&a, "cargo1/src/b.rs", "1");
    write(&a, "plain/a.txt", "1");
    write(&a, "plain/b.txt", "1");
    write(&a, "plain/c.txt", "1");
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "init"]);
    let v = json(&a, &["zone", "suggest", "--min-confidence", "0.2"]);
    let notes = v["notes"].to_string();
    assert!(notes.contains("could not be read or parsed"), "{notes}");
    let paths: Vec<&str> = v["zones"].as_array().unwrap().iter().map(|z| z["path"].as_str().unwrap()).collect();
    assert!(
        paths.contains(&"cargo1") && paths.contains(&"plain") && paths.contains(&"bad"),
        "{paths:?}"
    );
    // a dependency on a directory that does not exist is not an edge to anything
    let c = v["zones"].as_array().unwrap().iter().find(|z| z["path"] == "cargo1").unwrap();
    assert_eq!(c["depends_on"].as_array().unwrap().len(), 0);
    // an empty repository is not an error
    ok(t.path(), &["init", "empty"]);
    let e = json(&t.path().join("empty"), &["zone", "suggest"]);
    assert_eq!(e["zones"].as_array().unwrap().len(), 0);
}

#[test]
fn deep_scan_finds_import_dependencies_manifests_miss() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let a = t.path().join("r");
    // Go: two modules, a imports b with no require line; Python: svc imports lib's package
    write(&a, "go/a/go.mod", "module example.com/a\n");
    write(
        &a,
        "go/a/main.go",
        "package main\n\nimport (\n\t\"fmt\"\n\t\"example.com/b/util\"\n)\n",
    );
    write(&a, "go/a/x.go", "package main\n");
    write(&a, "go/b/go.mod", "module example.com/b\n");
    write(&a, "go/b/util/u.go", "package util\n");
    write(&a, "go/b/util/v.go", "package util\n");
    write(&a, "py/svc/pyproject.toml", "[project]\nname = \"svc\"\ndependencies = []\n");
    write(&a, "py/svc/svc/__init__.py", "");
    write(&a, "py/svc/svc/main.py", "import json\nfrom mylib.core import f\n");
    write(&a, "py/lib/pyproject.toml", "[project]\nname = \"mylib\"\n");
    write(&a, "py/lib/mylib/__init__.py", "");
    write(&a, "py/lib/mylib/core.py", "def f(): pass\n");
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "init"]);
    for p in ["go/a", "go/b", "py/svc", "py/lib"] {
        ok(&a, &["zone", "apply", p, "--yes"]);
    }
    let r = t.path().join("remote");
    let r = r.to_str().unwrap();
    ok(&a, &["remote", "set", r]);
    ok(&a, &["publish"]);
    let w = t.path();
    assert_eq!(names(&json(w, &["clone", r, "d", "--for", "go/a", "--dry-run"]), "zones"), ["a"]);
    let deep = json(w, &["clone", r, "d", "--for", "go/a", "--dry-run", "--deep"]);
    assert_eq!(names(&deep, "zones"), ["a", "b"], "{deep}");
    assert!(deep["zones"].to_string().contains("imports example.com/b/util"));
    assert_eq!(
        names(&json(w, &["clone", r, "d", "--for", "py/svc", "--dry-run", "--deep"]), "zones"),
        ["mylib", "svc"]
    );
}

#[test]
fn task_assist_is_opt_in_validated_and_proposal_only() {
    let t = tempfile::tempdir().unwrap();
    let (_a, r) = publish_split(t.path());
    let w = t.path();
    // keywords alone cannot map this text
    assert!(fail(w, &["clone", &r, "d", "--task", "quarterly zebra reconciliation", "--dry-run"]).contains("could not resolve"));
    let script = w.join("assist.sh");
    std::fs::write(&script, "#!/bin/sh\ncat >/dev/null\necho '{\"zones\":[\"web\"]}'\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let v = json(
        w,
        &[
            "clone",
            &r,
            "d",
            "--task",
            "quarterly zebra reconciliation",
            "--task-assist",
            script.to_str().unwrap(),
            "--dry-run",
        ],
    );
    assert_eq!(names(&v, "zones"), ["web"]);
    assert!(v["zones"][0]["reasons"][0].as_str().unwrap().contains("--task-assist"));
    assert!(!w.join("d").exists());
    // an invented zone name is refused, not guessed
    std::fs::write(&script, "#!/bin/sh\ncat >/dev/null\necho '{\"zones\":[\"nonesuch\"]}'\n").unwrap();
    let e = fail(
        w,
        &[
            "clone",
            &r,
            "d",
            "--task",
            "quarterly zebra",
            "--task-assist",
            script.to_str().unwrap(),
            "--dry-run",
        ],
    );
    assert!(e.contains("unknown zone"), "{e}");
}

#[test]
fn split_zones_publish_and_clone_in_full_unchanged() {
    let t = tempfile::tempdir().unwrap();
    let (a, r) = publish_split(t.path());
    ok(t.path(), &["clone", &r, "full"]);
    let f = t.path().join("full");
    assert!(ok(&f, &["fsck"]).contains("ok"));
    for p in [
        "crates/app/src/lib.rs",
        "web/src/index.js",
        "misc/tool3.txt",
        "README",
        "services/api/main.go",
    ] {
        assert_eq!(std::fs::read(a.join(p)).unwrap(), std::fs::read(f.join(p)).unwrap(), "{p}");
    }
}
