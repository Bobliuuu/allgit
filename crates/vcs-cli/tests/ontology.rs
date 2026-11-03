//! `vcs zone describe` and `vcs zone graph`: the zone ontology is derived from real state and
//! matches docs/zone-ontology.schema.json (checked with a small structural validator below).
use serde_json::Value;
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
fn json(dir: &Path, args: &[&str]) -> Value {
    let mut a = args.to_vec();
    a.extend(["--format", "full"]);
    a.push("--json");
    let out = ok(dir, &a);
    assert_eq!(out.matches('\n').count(), 1, "one line: {out:?}");
    serde_json::from_str(out.trim()).unwrap()
}
/// Default (compact) format.
fn jsonc(dir: &Path, args: &[&str]) -> (Value, usize) {
    let mut a = args.to_vec();
    a.push("--json");
    let out = ok(dir, &a);
    assert_eq!(out.matches('\n').count(), 1, "one line: {out:?}");
    (serde_json::from_str(out.trim()).unwrap(), out.len())
}
fn write(dir: &Path, f: &str, c: &str) {
    let p = dir.join(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}

/// Minimal JSON Schema subset check: type (string or list), required, properties, items, enum.
fn check(schema: &Value, v: &Value, at: &str, root: &Value) {
    if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
        let mut t = root;
        for part in r.trim_start_matches("#/").split('/') {
            t = &t[part];
        }
        return check(t, v, at, root);
    }
    if let Some(t) = schema.get("type") {
        let types: Vec<&str> = match t {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().map(|x| x.as_str().unwrap()).collect(),
            _ => panic!("bad type"),
        };
        let good = types.iter().any(|t| match *t {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "integer" => v.is_u64() || v.is_i64(),
            "boolean" => v.is_boolean(),
            "null" => v.is_null(),
            o => panic!("type {o}"),
        });
        assert!(good, "{at}: {v} is not {types:?}");
    }
    if let Some(e) = schema.get("enum") {
        assert!(e.as_array().unwrap().contains(v), "{at}: {v} not in {e}");
    }
    if let (Some(req), Some(o)) = (schema.get("required"), v.as_object()) {
        for k in req.as_array().unwrap() {
            assert!(o.contains_key(k.as_str().unwrap()), "{at}: missing required {k}");
        }
    }
    if let (Some(props), Some(o)) = (schema.get("properties"), v.as_object()) {
        for (k, s) in props.as_object().unwrap() {
            if let Some(x) = o.get(k) {
                check(s, x, &format!("{at}.{k}"), root);
            }
        }
    }
    if let (Some(items), Some(a)) = (schema.get("items"), v.as_array()) {
        for (i, x) in a.iter().enumerate() {
            check(items, x, &format!("{at}[{i}]"), root);
        }
    }
}

fn schema() -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/zone-ontology.schema.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}
fn validate(def: &str, v: &Value) {
    let s = schema();
    check(&s["$defs"][def], v, def, &s);
}

fn cargo(a: &Path, dir: &str, name: &str, desc: Option<&str>, deps: &[&str]) {
    let mut t = format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n");
    if let Some(d) = desc {
        t.push_str(&format!("description = \"{d}\"\n"));
    }
    t.push_str("\n[dependencies]\n");
    for d in deps {
        t.push_str(&format!("{d} = {{ path = \"../{d}\" }}\n"));
    }
    write(a, &format!("{dir}/Cargo.toml"), &t);
    write(a, &format!("{dir}/src/lib.rs"), "pub fn f() {}\n");
}

/// zones: core <- api <- app (path deps), docs (README purpose only), root with CODEOWNERS.
fn fixture(root: &Path) -> std::path::PathBuf {
    ok(root, &["init", "r"]);
    let a = root.join("r");
    for (n, p) in [
        ("core", "crates/core"),
        ("api", "crates/api"),
        ("app", "crates/app"),
        ("docs", "docs"),
    ] {
        ok(&a, &["zone", "create", n, "--path", p]);
    }
    cargo(&a, "crates/core", "core", Some("Core ledger primitives"), &[]);
    cargo(&a, "crates/api", "api", None, &["core"]);
    cargo(&a, "crates/app", "app", Some("The user-facing app"), &["api"]);
    write(&a, "docs/README.md", "# Docs\n\nHow to operate the system.\n");
    write(&a, "CODEOWNERS", "/crates/core/ @alice @bob\n/docs/ @carol\n");
    write(&a, "top.txt", "t\n");
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "initial import"]);
    // a second commit touching two zones at once
    write(&a, "crates/core/src/lib.rs", "pub fn f() { /* v2 */ }\n");
    write(&a, "crates/api/src/new.rs", "pub fn g() {}\n");
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "touch core and api"]);
    a
}

fn zone<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["zones"]
        .as_array()
        .unwrap()
        .iter()
        .find(|z| z["name"] == name)
        .unwrap_or_else(|| panic!("no zone {name}"))
}

#[test]
fn describe_matches_schema_and_real_state() {
    let t = tempfile::tempdir().unwrap();
    let a = fixture(t.path());
    let v = json(&a, &["zone", "describe", "--path", "crates/api/src/new.rs", "--path", "top.txt"]);
    validate("describe", &v);
    assert_eq!(v["schema"], "vcs.zone-ontology/1");
    assert_eq!(v["zones"].as_array().unwrap().len(), 5);
    let core = zone(&v, "core");
    assert_eq!(core["path"], "crates/core");
    assert_eq!(core["kind"], "cargo");
    assert_eq!(core["purpose"]["text"], "Core ledger primitives");
    assert_eq!(core["purpose"]["source"], "crates/core/Cargo.toml");
    assert_eq!(core["owners"]["codeowners"], serde_json::json!(["@alice", "@bob"]));
    assert_eq!(core["owners"]["top_authors"][0]["name"], "Dev");
    assert_eq!(core["history"]["count"]["commits"], 2);
    assert_eq!(core["history"]["recent"][0]["subject"], "touch core and api");
    assert_eq!(core["history"]["recent"][0]["files"][0]["path"], "src/lib.rs");
    assert_eq!(core["history"]["recent"][0]["files"][0]["kind"], "modified");
    assert_eq!(core["history"]["recent"][1]["files_changed"], 2);
    assert_eq!(core["subscription"]["subscribed"], true);
    // dependencies in both directions
    let api = zone(&v, "api");
    assert_eq!(api["dependencies"]["depends_on"][0]["zone"], "core");
    assert_eq!(api["dependencies"]["depended_on_by"][0]["zone"], "app");
    assert!(
        api["purpose"].is_null(),
        "no description anywhere: purpose must be null, not invented"
    );
    // README fallback, root zone, path ownership
    assert_eq!(zone(&v, "docs")["purpose"]["text"], "How to operate the system.");
    assert_eq!(zone(&v, "docs")["owners"]["codeowners"], serde_json::json!(["@carol"]));
    let root = zone(&v, "root");
    assert_eq!(root["path_ownership"]["owns_rest_of_repo"], true);
    assert_eq!(root["path_ownership"]["excluded_prefixes"].as_array().unwrap().len(), 4);
    assert_eq!(v["owner_of"][0]["zone"], "api");
    assert_eq!(v["owner_of"][0]["relative"], "src/new.rs");
    assert_eq!(v["owner_of"][1]["zone"], "root");
    // the latest operation moved core and api together
    let tx = &v["transactions"][0];
    assert_eq!(tx["current"], true);
    assert_eq!(tx["zones_touched"], serde_json::json!(["api", "core"]));
}

#[test]
fn describe_one_zone_sparse_and_text() {
    let t = tempfile::tempdir().unwrap();
    let a = fixture(t.path());
    ok(&a, &["sparse", "set", "app", "crates/app/src"]);
    let v = json(&a, &["zone", "describe", "app", "--history", "1"]);
    validate("describe", &v);
    assert_eq!(v["zones"].as_array().unwrap().len(), 1);
    assert_eq!(v["zones"][0]["subscription"]["sparse_paths"], serde_json::json!(["crates/app/src"]));
    assert_eq!(v["zones"][0]["history"]["recent"].as_array().unwrap().len(), 1);
    let text = ok(&a, &["zone", "describe"]);
    assert!(text.contains("zone core") && text.contains("Core ledger primitives"), "{text}");
    assert!(!vcs(&a, &["zone", "describe", "nope"]).status.success());
}

#[test]
fn fields_filter_keeps_only_requested_keys() {
    let t = tempfile::tempdir().unwrap();
    let a = fixture(t.path());
    let v = json(&a, &["zone", "describe", "--fields", "purpose,history", "--history", "0"]);
    let z = zone(&v, "core").as_object().unwrap();
    let mut keys: Vec<&str> = z.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["history", "id", "name", "purpose"]);
    assert!(!vcs(&a, &["zone", "describe", "--fields", "bogus"]).status.success());
}

#[test]
fn graph_has_edges_closures_and_path_index() {
    let t = tempfile::tempdir().unwrap();
    let a = fixture(t.path());
    let g = json(&a, &["zone", "graph"]);
    validate("graph", &g);
    let node = |n: &str| g["nodes"].as_array().unwrap().iter().find(|x| x["name"] == n).unwrap().clone();
    assert_eq!(node("app")["depends_on_transitive"], serde_json::json!(["api", "core"]));
    assert_eq!(node("core")["dependents"], serde_json::json!(["api"]));
    assert_eq!(node("core")["dependents_transitive"], serde_json::json!(["api", "app"]));
    assert_eq!(g["edges"].as_array().unwrap().len(), 2);
    assert!(g["edges"][0]["reasons"][0].as_str().unwrap().contains("Cargo"));
    assert_eq!(g["cycles"], serde_json::json!([]));
    assert_eq!(g["path_index"][0]["prefix"], "");
    assert!(ok(&a, &["zone", "graph"]).contains("->"));
}

#[test]
fn read_only() {
    let t = tempfile::tempdir().unwrap();
    let a = fixture(t.path());
    let before = (ok(&a, &["op", "log"]), ok(&a, &["status"]));
    json(&a, &["zone", "describe"]);
    json(&a, &["zone", "graph"]);
    assert_eq!((ok(&a, &["op", "log"]), ok(&a, &["status"])), before);
}

#[test]
fn compact_default_is_smaller_and_keeps_the_answers() {
    let t = tempfile::tempdir().unwrap();
    let a = fixture(t.path());
    let full = ok(&a, &["zone", "describe", "--format", "full", "--json"]).len();
    let (c, clen) = jsonc(&a, &["zone", "describe"]);
    let (b, blen) = jsonc(&a, &["zone", "describe", "--format", "brief"]);
    assert!(clen * 10 < full * 6, "compact {clen} vs full {full}");
    assert!(blen < clen);
    assert_eq!(c["format"], "compact");
    let core = zone(&c, "core");
    assert!(core.get("id").is_none(), "id equal to name is redundant");
    assert!(core["history"]["recent"][0].get("change_id").is_none());
    assert_eq!(core["history"]["recent"][0]["files"][0]["path"], "src/lib.rs");
    assert_eq!(core["history"]["recent"][0]["id"].as_str().unwrap().len(), 12);
    assert!(zone(&b, "core")["history"]["recent"][0].get("files").is_none());
    assert_eq!(zone(&b, "core")["history"]["recent"][0]["subject"], "touch core and api");
    assert!(core.get("locks").is_none() && core["owners"].get("email").is_none());
    let (g, _) = jsonc(&a, &["zone", "graph"]);
    assert!(g.get("edges").is_none());
    assert_eq!(
        g["nodes"].as_array().unwrap().iter().find(|n| n["name"] == "app").unwrap()["depends_on_transitive"],
        serde_json::json!(["api", "core"])
    );
}

#[test]
fn since_and_limit_filter_history() {
    let t = tempfile::tempdir().unwrap();
    let a = fixture(t.path());
    let v = json(&a, &["zone", "describe", "core", "--limit", "1"]);
    assert_eq!(v["zones"][0]["history"]["recent"].as_array().unwrap().len(), 1);
    let v = json(&a, &["zone", "describe", "core", "--since", "2100-01-01"]);
    assert_eq!(v["zones"][0]["history"]["recent"].as_array().unwrap().len(), 0);
}

#[test]
fn explain_answers_ownership_and_impact_in_one_call() {
    let t = tempfile::tempdir().unwrap();
    let a = fixture(t.path());
    let (v, _) = jsonc(&a, &["zone", "explain", "crates/core/src/lib.rs"]);
    assert_eq!(v["zone"], "core");
    assert_eq!(v["relative"], "src/lib.rs");
    assert_eq!(v["exists"], true);
    assert_eq!(v["codeowners"], serde_json::json!(["@alice", "@bob"]));
    assert_eq!(v["dependents_transitive"], serde_json::json!(["api", "app"]));
    assert_eq!(v["impacted"], serde_json::json!(["api", "app", "core"]));
    assert_eq!(v["last_commit"]["subject"], "touch core and api");
    let (v, _) = jsonc(&a, &["zone", "explain", "top.txt"]);
    assert_eq!(v["zone"], "root");
    let (v, _) = jsonc(&a, &["zone", "explain", "crates/api/nope.rs"]);
    assert!(v.get("exists").is_none() || v["exists"] == false);
    assert!(ok(&a, &["zone", "explain", "crates/api/src/lib.rs"]).contains("zone api"));
}

#[test]
fn changes_lists_files_per_zone_between_two_commits() {
    let t = tempfile::tempdir().unwrap();
    let a = fixture(t.path());
    let d = json(&a, &["zone", "describe", "core", "--history", "5"]);
    let first = d["zones"][0]["history"]["recent"][1]["id"].as_str().unwrap().to_owned();
    let last = d["zones"][0]["history"]["recent"][0]["id"].as_str().unwrap().to_owned();
    let v = json(&a, &["zone", "changes", &format!("{first}..{last}")]);
    assert_eq!(v["zones_touched"], serde_json::json!(["api", "core"]));
    let core = v["zones"].as_array().unwrap().iter().find(|z| z["zone"] == "core").unwrap();
    assert_eq!(
        core["files"],
        serde_json::json!([{"path": "crates/core/src/lib.rs", "kind": "modified"}])
    );
    let api = v["zones"].as_array().unwrap().iter().find(|z| z["zone"] == "api").unwrap();
    assert_eq!(api["files"][0]["path"], "crates/api/src/new.rs");
    assert_eq!(api["files"][0]["kind"], "added");
    // open start and a text rendering
    let text = ok(&a, &["zone", "changes", &format!("..{first}")]);
    assert!(text.contains("A crates/core/Cargo.toml"), "{text}");
    assert!(!vcs(&a, &["zone", "changes", "nonsense"]).status.success());
}

#[test]
fn log_all_zones_finds_pickaxe_and_regex_hits_in_one_call() {
    let t = tempfile::tempdir().unwrap();
    let a = fixture(t.path());
    let out = ok(&a, &["log", "--all-zones", "-S", "pub fn g()", "--format", "%z:%s"]);
    assert_eq!(out.trim(), "api:touch core and api");
    let out = ok(&a, &["log", "--all-zones", "-G", "v2|fn g", "--format", "%z:%s"]);
    let mut lines: Vec<&str> = out.lines().collect();
    lines.sort();
    assert_eq!(lines, ["api:touch core and api", "core:touch core and api"]);
    // -G ignores an unchanged line and a non-matching change
    let out = ok(&a, &["log", "--all-zones", "-G", "^pub fn f", "--format", "%z:%s"]);
    assert!(out.lines().any(|l| l == "core:initial import"), "{out}");
    assert!(!vcs(&a, &["log", "-G", "(", "--all-zones"]).status.success());
}

#[test]
fn log_grep_fixed_strings_matches_plus_and_parens_literally() {
    let t = tempfile::tempdir().unwrap();
    let a = fixture(t.path());
    write(&a, "crates/core/src/x.rs", "x\n");
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "net+api: (odd) subject"]);
    let none = ok(&a, &["log", "--all-zones", "--grep", "net+api: (odd)", "--format", "%s"]);
    assert_eq!(none.trim(), "", "as a regex `net+api` does not match `net+api`");
    let some = ok(&a, &["log", "--all-zones", "-F", "--grep", "net+api: (odd)", "--format", "%s"]);
    assert_eq!(some.trim(), "net+api: (odd) subject");
}
