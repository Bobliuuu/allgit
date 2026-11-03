//! #129: the "sole manifest writer" deployment. A client credential must not be able to write the
//! manifest (single PUT or multipart); `probe_manifest_write` is the startup self-check behind
//! `vcs-server --check-client-cannot-write-manifest`.
//! The first test needs no server; the second is gated on `VCS_TEST_S3_ENDPOINT` (RustFS / MinIO).

use std::io::{Read, Write};
use std::net::TcpListener;
use vcs_store_s3::{ManifestWriteProbe, S3Backend, S3Config};

/// Answers every request with the status chosen by `route(method_and_path)`.
fn server(route: fn(&str) -> &'static str) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            std::thread::spawn(move || {
                let mut data = Vec::new();
                let mut buf = [0u8; 8192];
                loop {
                    let Ok(n) = s.read(&mut buf) else { return };
                    data.extend_from_slice(&buf[..n]);
                    if n == 0 || String::from_utf8_lossy(&data).contains("\r\n\r\n") {
                        break;
                    }
                }
                let line = String::from_utf8_lossy(&data).lines().next().unwrap_or("").to_owned();
                let status = route(&line);
                let body = if status.starts_with("200") && line.starts_with("POST") {
                    "<InitiateMultipartUploadResult><UploadId>u1</UploadId></InitiateMultipartUploadResult>"
                } else {
                    ""
                };
                let r = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = s.write_all(r.as_bytes());
            });
        }
    });
    format!("http://{addr}")
}

fn client(ep: &str) -> S3Backend {
    let mut c = S3Config::new(ep, "b", "k", "s");
    c.timeout = std::time::Duration::from_secs(2);
    S3Backend::new_unchecked(c).unwrap()
}

#[test]
fn probe_reports_denied_allowed_and_inconclusive() {
    // a policy that refuses both routes with AccessDenied
    let ep = server(|_| "403 Forbidden");
    assert_eq!(client(&ep).probe_manifest_write().unwrap(), ManifestWriteProbe::Denied);
    // PUT refused but multipart initiation accepted: the credential can still replace the manifest
    let ep = server(|l| if l.starts_with("PUT") { "403 Forbidden" } else { "200 OK" });
    match client(&ep).probe_manifest_write().unwrap() {
        ManifestWriteProbe::Allowed(how) => assert!(how.contains("Multipart"), "{how}"),
        other => panic!("{other:?}"),
    }
    // an authorised writer sees a failed precondition (412) and nothing is written
    let ep = server(|l| if l.starts_with("PUT") { "412 Precondition Failed" } else { "200 OK" });
    assert!(matches!(client(&ep).probe_manifest_write().unwrap(), ManifestWriteProbe::Allowed(_)));
    // anything else (a missing bucket, say) is inconclusive and must not pass as "denied"
    let ep = server(|_| "404 Not Found");
    let e = client(&ep).probe_manifest_write().err().expect("404 on the multipart route is inconclusive");
    assert!(e.to_string().contains("inconclusive"), "{e}");
}

#[test]
fn bucket_policy_denies_manifest_writes_on_a_real_s3_server() {
    let Ok(ep) = std::env::var("VCS_TEST_S3_ENDPOINT") else {
        eprintln!("skipping: VCS_TEST_S3_ENDPOINT not set");
        return;
    };
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
    let (ak, sk) = (var("VCS_TEST_S3_ACCESS_KEY", "minioadmin"), var("VCS_TEST_S3_SECRET_KEY", "minioadmin"));
    let bucket = format!("manifest-deny-{}", std::process::id());
    let mut c = S3Config::new(&ep, &bucket, &ak, &sk);
    c.prefix = "repo/".into();
    let b = S3Backend::new_unchecked(c).unwrap();
    b.ensure_bucket().unwrap();
    // without a policy the credential can write the manifest: the self-check must say so
    assert!(matches!(b.probe_manifest_write().unwrap(), ManifestWriteProbe::Allowed(_)));
    // the documented policy (docs/deploy.md): nobody but the server's role may write the manifest
    let policy = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Deny","Principal":"*","Action":["s3:PutObject","s3:DeleteObject","s3:AbortMultipartUpload"],"Resource":["arn:aws:s3:::{bucket}/repo/manifest"]}}]}}"#
    );
    b.put_bucket_policy(&policy).unwrap();
    assert_eq!(b.probe_manifest_write().unwrap(), ManifestWriteProbe::Denied);
    // other keys stay writable (publishing objects does not need the manifest)
    use vcs_remote::Backend;
    assert!(b.put_if_absent("objects/aa/bb", b"x").unwrap());
    // and the manifest itself cannot be created or replaced through the normal API either
    assert!(b.put_if_absent(vcs_remote::manifest::KEY, b"evil").is_err());
    assert!(b.put_if_match(vcs_remote::manifest::KEY, b"evil", "\"0\"").is_err());
    b.delete_bucket_policy().unwrap();
    assert!(matches!(b.probe_manifest_write().unwrap(), ManifestWriteProbe::Allowed(_)));
}
