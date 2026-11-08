//! `vcs zone suggest --assist <cmd>` and the `zone.assist` setting, with a fake assistant script (no network,
//! no model). The assistant only proposes: nothing is created, and a repository config cannot name it.
#![cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Env {
    _t: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
    req: PathBuf,
    resp: PathBuf,
    script: PathBuf,
}

fn run(e: &Env, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(&e.repo)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_CONFIG_HOME", &e.home)
        .env("REQ_FILE", &e.req)
        .env("RESP_FILE", &e.resp)
        .env_remove("VCS_ZONE_ASSIST")
        .output()
        .unwrap()
}
fn ok(e: &Env, args: &[&str]) -> String {
    let o = run(e, args);
    assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn json(e: &Env, args: &[&str]) -> serde_json::Value {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    let out = ok(e, &a);
    serde_json::from_str(out.trim()).unwrap_or_else(|x| panic!("bad json {x}: {out}"))
}
fn write(dir: &Path, f: &str, c: &str) {
    let p = dir.join(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}

fn setup() -> Env {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = t.path().join("r");
    std::fs::create_dir_all(&repo).unwrap();
    let script = t.path().join("fake.sh");
    std::fs::write(&script, "cat > \"$REQ_FILE\"\ncat \"$RESP_FILE\"\n").unwrap();
    let e = Env {
        req: t.path().join("req.json"),
        resp: t.path().join("resp.json"),
        script,
        home,
        repo: repo.clone(),
        _t: t,
    };
    ok(&e, &["init", "."]);
    write(&repo, "svc/go.mod", "module ex.com/svc\n");
    for f in ["main.go", "util.go", "x.go"] {
        write(&repo, &format!("svc/{f}"), "package svc // SECRET_BODY_TEXT\n");
    }
    for f in ["index.js", "app.js", "ui/a.js", "ui/b.js", "ui/c.js"] {
        write(&repo, &format!("web/{f}"), "// SECRET_BODY_TEXT\n");
    }
    for f in ["a.md", "b.md", "c.md"] {
        write(&repo, &format!("docs/{f}"), "# SECRET_BODY_TEXT\n");
    }
    write(&repo, "README.md", "# top SECRET_BODY_TEXT\n");
    write(&repo, "CODEOWNERS", "*.js @alice\n");
    ok(&e, &["add", "."]);
    ok(&e, &["commit", "-m", "init"]);
    e
}

fn cmd(e: &Env) -> String {
    format!("sh {}", e.script.display())
}

const GOOD: &str = r#"{"zones":[
 {"name":"web-ui","path":"web/","rationale":"front end owned by @alice","confidence":0.7},
 {"name":"service","path":"svc","rationale":"the go module","confidence":0.9},
 {"name":"inner","path":"web/ui","rationale":"overlaps web-ui","confidence":0.5},
 {"name":"bad","path":"../etc","rationale":"escape","confidence":0.5},
 {"name":"ghost","path":"nothere","rationale":"missing","confidence":0.5}]}"#;

#[test]
fn off_by_default_and_read_only_with_assist() {
    let e = setup();
    std::fs::write(&e.resp, GOOD).unwrap();
    // plain suggest never runs an assistant
    ok(&e, &["zone", "suggest"]);
    assert!(!e.req.exists());
    let zones_before = ok(&e, &["zone", "list"]);

    let v = json(&e, &["zone", "suggest", "--assist", &cmd(&e)]);
    let a = &v["assist"];
    assert_eq!(a["ok"], true, "{a}");
    assert_eq!(a["command_from"], "flag");
    let props = a["proposals"].as_array().unwrap();
    let by = |n: &str| props.iter().find(|p| p["name"] == n).unwrap_or_else(|| panic!("no {n}: {a}"));
    assert_eq!(by("web-ui")["path"], "web");
    assert_eq!(by("web-ui")["source"], "assist");
    assert_eq!(by("web-ui")["files"], 5);
    assert_eq!(by("service")["source"], "both"); // same path as the go.mod heuristic proposal
    assert_eq!(props.len(), 2, "{a}");
    let rej: Vec<String> = a["rejected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["reason"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(rej.len(), 3, "{rej:?}");
    assert!(rej.iter().any(|r| r.contains("overlaps proposal")));
    assert!(rej.iter().any(|r| r.contains("`..`")));
    assert!(rej.iter().any(|r| r.contains("no committed files")));
    // heuristic list is unchanged and labelled
    assert!(v["zones"].as_array().unwrap().iter().all(|z| z["source"] == "heuristic"));
    // nothing was created
    assert_eq!(ok(&e, &["zone", "list"]), zones_before);

    // the request: directory paths and metadata, no contents, no individual file names
    let req = std::fs::read_to_string(&e.req).unwrap();
    let r: serde_json::Value = serde_json::from_str(&req).unwrap();
    assert_eq!((r["version"].as_u64(), r["kind"].as_str()), (Some(1), Some("zone-suggest")));
    assert!(r["tree"]["dirs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["path"] == "web" && d["files"] == 5));
    assert!(r["manifests"].as_array().unwrap().iter().any(|m| m == "svc/go.mod"));
    assert_eq!(r["codeowners"][0]["pattern"], "*.js");
    assert!(r["heuristic"].as_array().unwrap().iter().any(|h| h["path"] == "svc"));
    assert!(
        !req.contains("SECRET_BODY_TEXT") && !req.contains("index.js") && !req.contains("util.go"),
        "{req}"
    );

    // text output
    let t = ok(&e, &["zone", "suggest", "--assist", &cmd(&e)]);
    assert!(
        t.contains("proposals only") && t.contains("source assist") && t.contains("source both"),
        "{t}"
    );
    assert!(t.contains("rejected \"bad\""), "{t}");
    assert_eq!(ok(&e, &["zone", "list"]), zones_before);
}

#[test]
fn assistant_failures_keep_the_heuristic_output() {
    let e = setup();
    for bad in ["not json", "{}", "{\"version\":9,\"zones\":[]}", ""] {
        std::fs::write(&e.resp, bad).unwrap();
        let v = json(&e, &["zone", "suggest", "--assist", &cmd(&e)]);
        assert_eq!(v["assist"]["ok"], false, "{bad}");
        assert!(v["assist"]["error"].as_str().unwrap().contains("ignored"));
        assert!(!v["zones"].as_array().unwrap().is_empty());
    }
    let v = json(&e, &["zone", "suggest", "--assist", "exit 3"]);
    assert_eq!(v["assist"]["ok"], false);
    assert!(v["assist"]["error"].as_str().unwrap().contains("failed"));
    let v = json(
        &e,
        &["zone", "suggest", "--assist", "cat >/dev/null; sleep 5", "--assist-timeout", "1"],
    );
    assert!(v["assist"]["error"].as_str().unwrap().contains("timed out"));
}

#[test]
fn setting_is_user_only_opt_in() {
    let e = setup();
    std::fs::write(&e.resp, GOOD).unwrap();
    let c = cmd(&e);
    // unset: nothing runs
    ok(&e, &["zone", "suggest"]);
    assert!(!e.req.exists());
    // a repository config cannot set it (through the command or by hand)
    let o = run(&e, &["config", "set", "zone.assist", &c]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--user"));
    write(&e.repo, ".vcs/config", &format!("zone.assist = {c}\n"));
    let o = run(&e, &["zone", "suggest"]);
    assert!(o.status.success());
    assert!(!e.req.exists(), "a repository config named the assistant");
    assert!(String::from_utf8_lossy(&o.stderr).contains("ignoring zone.assist from the repository config"));
    assert!(!String::from_utf8_lossy(&o.stdout).contains("zone.assist is set"));
    std::fs::remove_file(e.repo.join(".vcs/config")).unwrap();
    // user config: runs and says so
    ok(&e, &["config", "set", "--user", "zone.assist", &c]);
    let out = ok(&e, &["zone", "suggest"]);
    assert!(out.contains("zone.assist is set") && out.contains("source assist"), "{out}");
    assert!(e.req.exists());
    // --json keeps stdout one document; the line goes to stderr
    let o = run(&e, &["--json", "zone", "suggest"]);
    let so = String::from_utf8_lossy(&o.stdout);
    assert!(serde_json::from_str::<serde_json::Value>(so.trim()).unwrap()["assist"]["command_from"] == "setting");
    assert!(String::from_utf8_lossy(&o.stderr).contains("zone.assist is set"));
    // --no-assist wins, `off` disables
    std::fs::remove_file(&e.req).unwrap();
    let out = ok(&e, &["zone", "suggest", "--no-assist"]);
    assert!(!out.contains("zone.assist is set") && !e.req.exists(), "{out}");
    ok(&e, &["config", "set", "--user", "zone.assist", "off"]);
    ok(&e, &["zone", "suggest"]);
    assert!(!e.req.exists());
    // the environment variable counts as the user's own
    let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(["zone", "suggest"])
        .current_dir(&e.repo)
        .env("VCS_CONFIG_HOME", &e.home)
        .env("VCS_ZONE_ASSIST", &c)
        .env("REQ_FILE", &e.req)
        .env("RESP_FILE", &e.resp)
        .output()
        .unwrap();
    assert!(o.status.success() && e.req.exists());
}

#[test]
fn proposals_are_adopted_only_by_apply_dir() {
    let e = setup();
    std::fs::write(&e.resp, GOOD).unwrap();
    // `apply <target>` still adopts only current heuristic suggestions; web/ui is not one
    let o = run(&e, &["zone", "apply", "web/ui", "--yes"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("not a current suggestion"));
    // an explicit directory: plan first
    let plan = ok(&e, &["zone", "apply", "--dir", "web/ui", "--name", "web-ui"]);
    assert!(plan.contains("plan (nothing changed)"), "{plan}");
    assert!(!ok(&e, &["zone", "list"]).contains("web-ui"));
    // refusals
    for bad in ["../x", "/abs", "a/./b", "web/../svc", "web//ui", "node_modules/x", ".hidden"] {
        let o = run(&e, &["zone", "apply", "--dir", bad, "--name", "z"]);
        assert!(!o.status.success(), "{bad}");
    }
    assert!(!run(&e, &["zone", "apply", "--dir", "web/ui"]).status.success()); // --name required
    assert!(!run(&e, &["zone", "apply", "--dir", "nothere", "--name", "z"]).status.success());
    // then really
    let out = ok(&e, &["zone", "apply", "--dir", "web/ui", "--name", "web-ui", "--yes"]);
    assert!(out.contains("created zone web-ui"), "{out}");
    assert!(ok(&e, &["zone", "list"]).contains("web-ui"));
    // and a later assist run now rejects the same path (it belongs to an existing zone)
    let v = json(&e, &["zone", "suggest", "--assist", &cmd(&e)]);
    let rej = v["assist"]["rejected"].to_string();
    assert!(rej.contains("overlaps existing zone"), "{rej}");
}
