mod common;
use common::*;
use std::path::Path;

fn fixtures() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/eval")
}

#[test]
fn offline_eval_scores_recorded_responses_deterministically() {
    let o = bin().args(["eval", fixtures()]).output().unwrap();
    assert!(o.status.success(), "{}", all_output(&o));
    let t = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(t.contains("good") && t.contains("exact"), "{t}");
    assert!(t.contains("cases: 5"), "{t}");
    assert!(t.contains("exact-match: 20.0% (1/5)"), "{t}");
    // Without a parser (default build) the unparsable answer is a valid proposal that merely differs.
    #[cfg(feature = "structural")]
    assert!(
        t.contains("proposals passing validation: 40.0% (2/5)") && t.contains("refused: 1, invalid: 1, no response: 1"),
        "{t}"
    );
    #[cfg(not(feature = "structural"))]
    assert!(
        t.contains("proposals passing validation: 60.0% (3/5)") && t.contains("refused: 1, invalid: 0, no response: 1"),
        "{t}"
    );
    // Same input, same output.
    let o2 = bin().args(["eval", fixtures()]).output().unwrap();
    assert_eq!(o.stdout, o2.stdout);
}

#[cfg(feature = "structural")]
#[test]
fn parse_validity_rate_counts_rejected_unparsable_proposals() {
    let o = bin().args(["eval", fixtures(), "--json"]).output().unwrap();
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["cases"], 5);
    assert_eq!(j["exact"], 1);
    assert_eq!(j["different"], 1);
    // good, wrong parse; badparse is a checkable miss.
    assert_eq!(j["parse_checkable"], 3);
    assert_eq!(j["parse_valid"], 2);
}

#[test]
fn eval_needs_no_key_and_makes_no_network_call() {
    // No ANTHROPIC_API_KEY, URL points nowhere: offline eval must not care.
    let o = bin()
        .env("VCS_ASSIST_API_URL", "http://127.0.0.1:1/x")
        .args(["eval", fixtures()])
        .output()
        .unwrap();
    assert!(o.status.success());
}

#[test]
fn live_eval_uses_the_api_and_scores() {
    let m = Mock::start(vec![ok_text(&resolved(GOOD))]);
    let d = tempfile::tempdir().unwrap();
    let c = d.path().join("case1");
    std::fs::create_dir(&c).unwrap();
    for (f, v) in [
        ("base", BASE),
        ("ours", OURS),
        ("theirs", THEIRS),
        ("expected", GOOD),
        ("path", "src/a.rs\n"),
    ] {
        std::fs::write(c.join(f), v).unwrap();
    }
    let o = bin()
        .env("ANTHROPIC_API_KEY", KEY)
        .env("VCS_ASSIST_API_URL", &m.url)
        .args(["eval", d.path().to_str().unwrap(), "--live"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", all_output(&o));
    assert!(String::from_utf8_lossy(&o.stdout).contains("exact-match: 100.0% (1/1)"));
    assert_eq!(m.requests().len(), 1);
    assert!(!all_output(&o).contains(KEY));
}

#[test]
fn eval_errors_are_clean() {
    let o = bin().args(["eval", "/nonexistent-dir-xyz"]).output().unwrap();
    assert!(!o.status.success());
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("c")).unwrap();
    let o = bin().args(["eval", d.path().to_str().unwrap()]).output().unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("missing file"));
    assert!(!Path::new("x").exists());
    let o = bin().args(["eval"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}
