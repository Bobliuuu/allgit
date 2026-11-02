//! #129: `vcs-server --check-client-cannot-write-manifest` through the real binary against a real
//! S3-compatible server (gated on `VCS_TEST_S3_ENDPOINT`, docs/deploy.md): the server refuses to
//! start while the client credential can write the manifest, and starts once the bucket policy denies it.
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn server_bin() -> PathBuf {
    let vcs = PathBuf::from(env!("CARGO_BIN_EXE_vcs"));
    let p = vcs.with_file_name("vcs-server");
    if !p.exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let st = Command::new(cargo).args(["build", "-p", "vcs-server", "--bin", "vcs-server"]).status().unwrap();
        assert!(st.success() && p.exists(), "could not build vcs-server");
    }
    p
}

#[test]
fn server_refuses_to_start_while_the_client_credential_can_write_the_manifest() {
    let Ok(ep) = std::env::var("VCS_TEST_S3_ENDPOINT") else {
        eprintln!("skipping: VCS_TEST_S3_ENDPOINT not set");
        return;
    };
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
    let (ak, sk) = (var("VCS_TEST_S3_ACCESS_KEY", "minioadmin"), var("VCS_TEST_S3_SECRET_KEY", "minioadmin"));
    let bucket = format!("selfcheck-{}", std::process::id());
    let admin = vcs_store_s3::S3Backend::new_unchecked(vcs_store_s3::S3Config::new(&ep, &bucket, &ak, &sk)).unwrap();
    admin.ensure_bucket().unwrap();
    let t = tempfile::tempdir().unwrap();
    let hash = Command::new(server_bin()).args(["hash-token", "tok"]).output().unwrap();
    let tokens = t.path().join("tokens");
    std::fs::write(&tokens, format!("alice write * {}\n", String::from_utf8(hash.stdout).unwrap().trim())).unwrap();
    let url = format!("s3://{bucket}/repo?endpoint={ep}");
    let run = |check: bool| {
        let mut c = Command::new(server_bin());
        c.args(["--backend", &url, "--tokens", tokens.to_str().unwrap(), "--listen", "127.0.0.1:0"]);
        if check {
            c.arg("--check-client-cannot-write-manifest");
        }
        c.env("AWS_ACCESS_KEY_ID", &ak)
            .env("AWS_SECRET_ACCESS_KEY", &sk)
            .env("VCS_CLIENT_ACCESS_KEY_ID", &ak)
            .env("VCS_CLIENT_SECRET_ACCESS_KEY", &sk)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        c.spawn().unwrap()
    };
    // no policy: refuses, exit status 2, says why
    let mut child = run(true);
    let mut err = String::new();
    BufReader::new(child.stderr.take().unwrap()).read_line(&mut err).unwrap();
    let st = child.wait().unwrap();
    assert_eq!(st.code(), Some(2), "{err}");
    assert!(err.contains("client credential can write the manifest"), "{err}");
    // policy in place: starts and reports its address
    let policy = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Deny","Principal":"*","Action":["s3:PutObject","s3:DeleteObject","s3:AbortMultipartUpload"],"Resource":["arn:aws:s3:::{bucket}/repo/manifest"]}}]}}"#
    );
    admin.put_bucket_policy(&policy).unwrap();
    let mut child = run(true);
    let mut line = String::new();
    BufReader::new(child.stderr.take().unwrap()).read_line(&mut line).unwrap();
    let _ = child.kill();
    let _ = child.wait();
    assert!(line.contains("http://"), "server did not start: {line:?}");
    admin.delete_bucket_policy().unwrap();
}
