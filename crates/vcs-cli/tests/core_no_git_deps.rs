//! Guard: the core crates must never depend (directly or transitively) on git, libgit2, gitoxide or
//! Perforce libraries. Git/p4 interop lives in `vcs-cli` and shells out to the external tools.
//! Uses `cargo metadata`, so it needs a registry index (cargo fetches it if missing).

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

const CORE: &[&str] = &[
    "vcs-objects",
    "vcs-store",
    "vcs-merge",
    "vcs-remote",
    "vcs-repo",
    "vcs-store-s3",
    "vcs-server",
];

fn forbidden(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "git2"
        || n == "gix"
        || n.starts_with("gix-")
        || n.starts_with("libgit2")
        || n.starts_with("git2-")
        || n.contains("gitoxide")
        || n == "git"
        || n.starts_with("git-")
        || n == "p4"
        || n.starts_with("p4-")
        || n.contains("perforce")
}

#[test]
fn core_crates_have_no_git_or_p4_dependencies() {
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["metadata", "--format-version", "1"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run cargo metadata");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let meta: Value = serde_json::from_slice(&out.stdout).expect("metadata json");

    let names: BTreeMap<&str, &str> = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p["name"].as_str().unwrap()))
        .collect();
    let edges: BTreeMap<&str, Vec<&str>> = meta["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            let deps = n["dependencies"].as_array().unwrap().iter().map(|d| d.as_str().unwrap()).collect();
            (n["id"].as_str().unwrap(), deps)
        })
        .collect();

    let mut bad = Vec::new();
    let mut seen_core = 0;
    for (id, name) in &names {
        if !CORE.contains(name) {
            continue;
        }
        seen_core += 1;
        // Transitive closure over normal, build and dev dependencies.
        let mut stack = vec![*id];
        let mut seen = BTreeSet::new();
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            let n = names[cur];
            if forbidden(n) {
                bad.push(format!("{name} -> {n}"));
            }
            stack.extend(edges.get(cur).into_iter().flatten().copied());
        }
    }
    assert_eq!(seen_core, CORE.len(), "core crate list out of date");
    assert!(bad.is_empty(), "core crates must not depend on git/p4 libraries: {bad:?}");
}

#[test]
fn forbidden_name_matcher_catches_known_crates() {
    for n in [
        "git2",
        "libgit2-sys",
        "gix",
        "gix-object",
        "gitoxide-core",
        "p4-sys",
        "perforce-api",
    ] {
        assert!(forbidden(n), "{n}");
    }
    for n in ["blake3", "gimli", "digest", "vcs-repo", "tempfile"] {
        assert!(!forbidden(n), "{n}");
    }
}
