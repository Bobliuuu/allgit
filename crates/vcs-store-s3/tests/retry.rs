//! #569: S3 client retry with jittered backoff on SlowDown / 5xx / timeouts, and a configurable timeout.
//! A scripted fault-injecting HTTP server answers each request from a queue and records what it saw.

use std::sync::atomic::Ordering::SeqCst;
use std::time::{Duration, Instant};
use vcs_remote::{Backend, BackendError};
use vcs_store_s3::{S3Backend, S3Config};

mod common;
use common::*;

const SLOWDOWN: Step = Step::Reply(
    "503 Slow Down",
    "",
    "<Error><Code>SlowDown</Code><Message>Please reduce your request rate.</Message></Error>",
);
const OK: Step = Step::Reply("200 OK", "ETag: \"e1\"\r\n", "hello");

#[test]
fn get_survives_a_slowdown_ramp_and_counts_it() {
    let before = vcs_store_s3::stats();
    let sc = scripted(vec![SLOWDOWN, SLOWDOWN, Step::Reply("500 Internal Server Error", "", ""), OK]);
    let b = S3Backend::new_unchecked(cfg(&sc.url)).unwrap();
    let got = b.get("k").unwrap().expect("object");
    assert_eq!(got.data, b"hello");
    assert_eq!(sc.hits.load(SeqCst), 4);
    let after = vcs_store_s3::stats();
    assert!(after.slowdown >= before.slowdown + 2, "{after:?}");
    assert!(after.retries >= before.retries + 3);
    assert!(after.requests[0] >= before.requests[0] + 4);
}

#[test]
fn retry_waits_are_jittered_and_honour_retry_after() {
    let sc = scripted(vec![
        Step::Reply("503 Slow Down", "Retry-After: 1\r\n", "<Code>SlowDown</Code>"),
        OK,
    ]);
    let b = S3Backend::new_unchecked(cfg(&sc.url)).unwrap();
    let t = Instant::now();
    b.get("k").unwrap();
    assert!(t.elapsed() >= Duration::from_millis(1000), "waited only {:?}", t.elapsed());
}

#[test]
fn exhausted_slowdown_is_unavailable_and_tagged_so_callers_can_tell() {
    let sc = scripted(vec![SLOWDOWN]);
    let mut c = cfg(&sc.url);
    c.retries = 2;
    let b = S3Backend::new_unchecked(c).unwrap();
    let e = b.get("k").unwrap_err();
    assert!(matches!(&e, BackendError::Unavailable(m) if m.contains("SlowDown")), "{e}");
    assert_eq!(sc.hits.load(SeqCst), 3, "1 try + 2 retries");
}

#[test]
fn retries_zero_disables_retry() {
    let sc = scripted(vec![SLOWDOWN, OK]);
    let mut c = cfg(&sc.url);
    c.retries = 0;
    let b = S3Backend::new_unchecked(c).unwrap();
    assert!(b.get("k").is_err());
    assert_eq!(sc.hits.load(SeqCst), 1);
}

#[test]
fn writes_retry_503_but_an_ambiguous_500_surfaces() {
    let sc = scripted(vec![SLOWDOWN, OK]);
    let b = S3Backend::new_unchecked(cfg(&sc.url)).unwrap();
    assert!(b.put_if_absent("k", b"v").unwrap());
    assert_eq!(sc.hits.load(SeqCst), 2);

    let sc = scripted(vec![Step::Reply("500 Internal Server Error", "", ""), OK]);
    let b = S3Backend::new_unchecked(cfg(&sc.url)).unwrap();
    assert!(matches!(b.put_if_absent("k", b"v"), Err(BackendError::Unavailable(_))));
    assert_eq!(
        sc.hits.load(SeqCst),
        1,
        "a 500 on a conditional write may have applied: never replayed blind"
    );
}

#[test]
fn a_stalled_request_times_out_at_the_configured_timeout_and_is_retried() {
    let sc = scripted(vec![Step::Stall(Duration::from_secs(3)), OK]);
    let mut c = cfg(&sc.url);
    c.timeout = Duration::from_millis(300);
    let b = S3Backend::new_unchecked(c).unwrap();
    let t = Instant::now();
    assert!(b.exists("k").unwrap());
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    assert_eq!(sc.hits.load(SeqCst), 2);
}

#[test]
fn timeout_is_configurable_from_the_environment_table() {
    let mut c = S3Config::new("http://127.0.0.1:1", "b", "k", "s");
    let env = |k: &str| match k {
        "VCS_S3_TIMEOUT_SECS" => Some("120".to_owned()),
        "VCS_S3_RETRIES" => Some("7".to_owned()),
        "VCS_S3_RETRY_BASE_MS" => Some("250".to_owned()),
        _ => None,
    };
    c.apply_tuning_env(&env);
    assert_eq!(
        (c.timeout, c.retries, c.retry_base),
        (Duration::from_secs(120), 7, Duration::from_millis(250))
    );
}

#[test]
fn conflicts_and_cas_losses_are_counted() {
    let before = vcs_store_s3::stats();
    let sc = scripted(vec![Step::Reply("409 Conflict", "", "<Code>ConditionalRequestConflict</Code>")]);
    let b = S3Backend::new_unchecked(cfg(&sc.url)).unwrap();
    assert_eq!(b.put_if_match("manifest", b"x", "\"t\"").unwrap(), None);
    let sc = scripted(vec![Step::Reply("412 Precondition Failed", "", "")]);
    let b = S3Backend::new_unchecked(cfg(&sc.url)).unwrap();
    assert!(!b.put_if_absent("k", b"x").unwrap());
    let after = vcs_store_s3::stats();
    assert!(after.conflict_409 > before.conflict_409);
    assert!(after.cas_losses >= before.cas_losses + 2);
}
