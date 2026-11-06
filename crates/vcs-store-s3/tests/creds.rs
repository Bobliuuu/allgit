//! #566: credential provider chain (container endpoint, IMDSv2), refresh before expiry, refresh on an
//! expired-token 403, and the rejected-credentials flag that fails /readyz.

mod common;
use common::*;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::Arc;
use std::time::Duration;
use vcs_remote::Backend;
use vcs_store_s3::{CredentialProvider, S3Backend};

fn creds_json(ak: &'static str, expiration: &'static str) -> Step {
    match (ak, expiration) {
        ("AKFIRST", "2099") => Step::Reply(
            "200 OK",
            "",
            r#"{"AccessKeyId":"AKFIRST","SecretAccessKey":"s1","Token":"T1","Expiration":"2099-01-01T00:00:00Z"}"#,
        ),
        ("AKSECOND", "2099") => Step::Reply(
            "200 OK",
            "",
            r#"{"AccessKeyId":"AKSECOND","SecretAccessKey":"s2","Token":"T2","Expiration":"2099-01-01T00:00:00Z"}"#,
        ),
        ("AKFIRST", _) => Step::Reply(
            "200 OK",
            "",
            r#"{"AccessKeyId":"AKFIRST","SecretAccessKey":"s1","Token":"T1","Expiration":"2000-01-01T00:00:00Z"}"#,
        ),
        _ => unreachable!(),
    }
}

const OK: Step = Step::Reply("200 OK", "ETag: \"e1\"\r\n", "hello");

fn credential_of(req: &str) -> String {
    let i = req.find("Credential=").or_else(|| req.find("credential=")).expect("signed request");
    req[i + 11..].split('/').next().unwrap().to_owned()
}

fn backend(url: &str, provider: CredentialProvider) -> S3Backend {
    let mut c = cfg(url);
    c.credentials = Some(Arc::new(provider));
    S3Backend::new_unchecked(c).unwrap()
}

#[test]
fn container_credentials_are_refreshed_before_they_expire() {
    let cred = scripted(vec![creds_json("AKFIRST", "2099"), creds_json("AKSECOND", "2099")]);
    let s3 = scripted(vec![OK]);
    // margin of a century: every call counts as "about to expire", so each request refetches
    let p = CredentialProvider::container(&format!("{}/creds", cred.url), Some("Bearer pod-token".into()))
        .with_margin(Duration::from_secs(100 * 365 * 86400));
    let b = backend(&s3.url, p);
    b.get("k").unwrap();
    b.get("k").unwrap();
    let seen = s3.seen.lock().unwrap();
    assert_eq!(credential_of(&seen[0]), "AKFIRST");
    assert_eq!(credential_of(&seen[1]), "AKSECOND");
    assert!(seen[0].to_lowercase().contains("x-amz-security-token: t1"));
    assert!(seen[1].to_lowercase().contains("x-amz-security-token: t2"));
    let c = cred.seen.lock().unwrap();
    assert!(c[0].to_lowercase().contains("authorization: bearer pod-token"), "{}", c[0]);
}

#[test]
fn credentials_are_cached_until_the_margin() {
    let cred = scripted(vec![creds_json("AKFIRST", "2099"), creds_json("AKSECOND", "2099")]);
    let s3 = scripted(vec![OK]);
    let b = backend(&s3.url, CredentialProvider::container(&format!("{}/creds", cred.url), None));
    for _ in 0..3 {
        b.get("k").unwrap();
    }
    assert_eq!(cred.hits.load(SeqCst), 1, "one fetch serves all three requests");
}

#[test]
fn a_failed_refresh_keeps_using_credentials_that_have_not_expired() {
    let cred = scripted(vec![
        creds_json("AKFIRST", "2099"),
        Step::Reply("500 Internal Server Error", "", ""),
    ]);
    let s3 = scripted(vec![OK]);
    let p = CredentialProvider::container(&format!("{}/creds", cred.url), None).with_margin(Duration::from_secs(100 * 365 * 86400));
    let b = backend(&s3.url, p);
    b.get("k").unwrap();
    b.get("k").unwrap(); // refresh fails (500), cached keys are still valid
    let seen = s3.seen.lock().unwrap();
    assert_eq!(
        (credential_of(&seen[0]), credential_of(&seen[1])),
        ("AKFIRST".into(), "AKFIRST".into())
    );
}

#[test]
fn expired_credentials_that_cannot_be_refreshed_are_an_error_not_a_stale_signature() {
    let cred = scripted(vec![
        creds_json("AKFIRST", "2000"),
        Step::Reply("500 Internal Server Error", "", ""),
    ]);
    let s3 = scripted(vec![OK]);
    let b = backend(&s3.url, CredentialProvider::container(&format!("{}/creds", cred.url), None));
    // first fetch returns already-expired keys; they are used once (nothing better exists) ...
    assert!(b.get("k").is_ok());
    // ... then a refresh is due, fails, and the cached keys are past expiry
    let e = b.get("k").unwrap_err();
    assert!(e.to_string().contains("credential provider"), "{e}");
}

#[test]
fn imds_v2_uses_a_session_token_then_role_then_credentials() {
    let imds = scripted(vec![
        Step::Reply("200 OK", "", "imds-session-token"),
        Step::Reply("200 OK", "", "my-role\n"),
        creds_json("AKFIRST", "2099"),
    ]);
    let s3 = scripted(vec![OK]);
    let b = backend(&s3.url, CredentialProvider::imds(&imds.url));
    b.get("k").unwrap();
    let seen = imds.seen.lock().unwrap();
    assert!(seen[0].starts_with("PUT /latest/api/token"), "{}", seen[0]);
    assert!(seen[0].to_lowercase().contains("x-aws-ec2-metadata-token-ttl-seconds"));
    assert!(
        seen[1].starts_with("GET /latest/meta-data/iam/security-credentials/ "),
        "{}",
        seen[1]
    );
    assert!(seen[1].to_lowercase().contains("x-aws-ec2-metadata-token: imds-session-token"));
    assert!(
        seen[2].starts_with("GET /latest/meta-data/iam/security-credentials/my-role "),
        "{}",
        seen[2]
    );
    assert_eq!(credential_of(&s3.seen.lock().unwrap()[0]), "AKFIRST");
}

#[test]
fn an_expired_token_403_refreshes_the_provider_once_and_retries() {
    let cred = scripted(vec![creds_json("AKFIRST", "2099"), creds_json("AKSECOND", "2099")]);
    let s3 = scripted(vec![
        Step::Reply(
            "403 Forbidden",
            "",
            "<Error><Code>ExpiredToken</Code><Message>The provided token has expired.</Message></Error>",
        ),
        OK,
    ]);
    let b = backend(&s3.url, CredentialProvider::container(&format!("{}/creds", cred.url), None));
    assert_eq!(b.get("k").unwrap().unwrap().data, b"hello");
    let seen = s3.seen.lock().unwrap();
    assert_eq!(
        (credential_of(&seen[0]), credential_of(&seen[1])),
        ("AKFIRST".into(), "AKSECOND".into())
    );
}
