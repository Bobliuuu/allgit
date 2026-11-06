//! #566: a 403 for expired static credentials raises the process-wide flag that fails /readyz; the next
//! successful request clears it. One test per binary because the flag is global.

mod common;
use common::*;
use vcs_remote::Backend;
use vcs_store_s3::{credentials_rejected, S3Backend};

#[test]
fn expired_static_credentials_set_the_flag_until_a_request_succeeds() {
    let expired = Step::Reply("403 Forbidden", "", "<Error><Code>ExpiredToken</Code></Error>");
    let s3 = scripted(vec![
        expired.clone(),
        expired,
        Step::Reply("404 Not Found", "", ""),
        Step::Reply("200 OK", "ETag: \"e\"\r\n", "x"),
    ]);
    let b = S3Backend::new_unchecked(cfg(&s3.url)).unwrap();
    assert!(!credentials_rejected());
    assert!(b.exists("k").is_err());
    assert!(credentials_rejected(), "a 403 on the HEAD probe must fail readiness");
    assert!(b.exists("k").is_err());
    assert!(credentials_rejected());
    // any answer other than 403 shows the credentials were accepted: flag clears
    assert!(!b.exists("k").unwrap());
    assert!(!credentials_rejected());
    assert!(b.exists("k").unwrap());
    assert!(!credentials_rejected());
}
