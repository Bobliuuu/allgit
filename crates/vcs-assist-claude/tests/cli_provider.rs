#![cfg(unix)]
mod common;
use common::*;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

/// A fake `claude` executable: logs its arguments and stdin, prints a canned `--output-format json` result.
fn fake_claude(dir: &std::path::Path, result_text: &str) -> std::path::PathBuf {
    let reply = serde_json::json!({
        "type": "result", "subtype": "success", "is_error": false, "stop_reason": "end_turn",
        "duration_ms": 1234, "total_cost_usd": 0.0123,
        "usage": {"input_tokens": 7, "output_tokens": 55, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 200},
        "result": result_text
    });
    std::fs::write(dir.join("reply.json"), reply.to_string()).unwrap();
    let p = dir.join("claude");
    let mut f = std::fs::File::create(&p).unwrap();
    writeln!(
        f,
        "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{d}/argv.txt'\npwd > '{d}/cwd.txt'\ncat > '{d}/stdin.txt'\ncat '{d}/reply.json'",
        d = dir.display()
    )
    .unwrap();
    drop(f);
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn hunk_answer() -> String {
    "<explanation>sum</explanation>\n<hunk id=\"1\">\n    21\n</hunk>".into()
}

#[test]
fn cli_provider_runs_headless_without_tools_and_scores_hunk_answers() {
    let d = tempfile::tempdir().unwrap();
    let fake = fake_claude(d.path(), &hunk_answer());
    let o = {
        let mut c = bin();
        c.env("VCS_ASSIST_PROVIDER", "cli")
            .env("VCS_ASSIST_CLAUDE_BIN", &fake)
            .env("VCS_ASSIST_MODEL", "haiku")
            .env_remove("ANTHROPIC_API_KEY")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut ch = c.spawn().unwrap();
        ch.stdin.take().unwrap().write_all(good_request().as_bytes()).unwrap();
        ch.wait_with_output().unwrap()
    };
    assert!(o.status.success(), "{}", all_output(&o));
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["resolution"], GOOD);
    let argv = std::fs::read_to_string(d.path().join("argv.txt")).unwrap();
    let lines: Vec<&str> = argv.lines().collect();
    assert!(
        lines.contains(&"-p") && lines.contains(&"json") && lines.contains(&"haiku"),
        "{argv}"
    );
    // tools disabled: `--tools` followed by an empty argument
    let i = lines.iter().position(|l| *l == "--tools").unwrap();
    assert_eq!(lines[i + 1], "");
    assert!(lines.contains(&"--no-session-persistence") && lines.contains(&"--strict-mcp-config"));
    // the conflict goes in on stdin, not on the command line
    let stdin = std::fs::read_to_string(d.path().join("stdin.txt")).unwrap();
    assert!(stdin.contains("CONFLICT 1") && stdin.contains("fn a()"));
    assert!(!argv.contains("fn a()"));
    // runs in a scratch dir, not the caller's directory
    let cwd = std::fs::read_to_string(d.path().join("cwd.txt")).unwrap();
    assert!(cwd.contains("vcs-assist-cli-"), "{cwd}");
}

#[test]
fn cli_eval_records_responses_and_metrics_for_offline_rescoring() {
    let d = tempfile::tempdir().unwrap();
    let fake = fake_claude(d.path(), &hunk_answer());
    let corpus = d.path().join("corpus");
    let c = corpus.join("case1");
    std::fs::create_dir_all(&c).unwrap();
    for (f, v) in [
        ("base", BASE),
        ("ours", OURS),
        ("theirs", THEIRS),
        ("expected", GOOD),
        ("path", "src/a.rs\n"),
    ] {
        std::fs::write(c.join(f), v).unwrap();
    }
    let dir = corpus.to_str().unwrap();
    let o = bin()
        .env("VCS_ASSIST_CLAUDE_BIN", &fake)
        .env_remove("ANTHROPIC_API_KEY")
        .args(["eval", dir, "--live", "--provider", "cli", "--record"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", all_output(&o));
    let t = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(t.contains("exact-match: 100.0% (1/1)") && t.contains("cost: $0.0123"), "{t}");
    assert!(std::fs::read_to_string(c.join("response.txt")).unwrap().contains("<hunk id=\"1\">"));
    let m: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(c.join("metrics.json")).unwrap()).unwrap();
    assert_eq!(m["output_tokens"], 55);
    // offline re-score reads the recording and the metrics, no CLI involved
    let o = bin()
        .env("VCS_ASSIST_CLAUDE_BIN", "/nonexistent")
        .args(["eval", dir, "--json"])
        .output()
        .unwrap();
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["exact"], 1);
    assert_eq!(j["output_tokens"], 55);
}

#[test]
fn cli_failures_are_clean() {
    let d = tempfile::tempdir().unwrap();
    let o = bin()
        .env("VCS_ASSIST_PROVIDER", "cli")
        .env("VCS_ASSIST_CLAUDE_BIN", d.path().join("missing"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut ch| {
            ch.stdin.take().unwrap().write_all(good_request().as_bytes())?;
            ch.wait_with_output()
        })
        .unwrap();
    assert!(!o.status.success() && o.stdout.is_empty());
    assert!(String::from_utf8_lossy(&o.stderr).contains("cannot run"));
}

#[test]
fn split_is_deterministic_and_partitions_the_corpus() {
    let d = tempfile::tempdir().unwrap();
    for i in 0..50 {
        let c = d.path().join(format!("case{i:03}"));
        std::fs::create_dir(&c).unwrap();
        for (f, v) in [
            ("base", BASE),
            ("ours", OURS),
            ("theirs", THEIRS),
            ("expected", GOOD),
            ("path", "a.rs\n"),
        ] {
            std::fs::write(c.join(f), v).unwrap();
        }
    }
    let n = |s: &str| {
        let o = bin()
            .args(["eval", d.path().to_str().unwrap(), "--mock", "refuse", "--split", s, "--json"])
            .output()
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["cases"]
            .as_u64()
            .unwrap()
    };
    let (dev, test, all) = (n("dev"), n("test"), n("all"));
    assert_eq!(all, 50);
    assert_eq!(dev + test, 50);
    assert!(test >= 3 && test <= 20, "{test}");
}
