//! `commit -S`, `tag -s`, `verify-commit`, `verify-tag`, `log --show-signature` with fixed keys.
use std::path::Path;
use std::process::{Command, Output};
use vcs_objects::signing::fixtures::{KEY1, KEY1_PUB, KEY2, KEY2_PUB, KEY_ENCRYPTED};

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Ann")
        .env("VCS_AUTHOR_EMAIL", "ann@example.com")
        .env("VCS_CONFIG_HOME", dir.join("nocfg"))
        .output()
        .unwrap()
}
fn ok(dir: &Path, args: &[&str]) -> String {
    let o = run(dir, args);
    assert!(
        o.status.success(),
        "vcs {args:?}: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn fail(dir: &Path, args: &[&str]) -> String {
    let o = run(dir, args);
    assert!(!o.status.success(), "vcs {args:?} should fail");
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}
fn commit(d: &Path, f: &str, extra: &[&str]) {
    std::fs::write(d.join(f), f).unwrap();
    ok(d, &["add", f]);
    let mut a = vec!["commit", "-m", f];
    a.extend_from_slice(extra);
    ok(d, &a);
}

fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("r");
    ok(t.path(), &["init", "r"]);
    for (n, k) in [("key1", KEY1), ("key2", KEY2), ("enc", KEY_ENCRYPTED)] {
        std::fs::write(t.path().join(n), k).unwrap();
    }
    let pk = |p: &str| p.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    std::fs::write(t.path().join("allowed"), format!("alice {}\n", pk(KEY1_PUB))).unwrap();
    std::fs::write(t.path().join("allowed2"), format!("alice {}\nbob {}\n", pk(KEY1_PUB), pk(KEY2_PUB))).unwrap();
    std::fs::write(t.path().join("revoked"), format!("{}\n", pk(KEY1_PUB))).unwrap();
    ok(&d, &["config", "set", "user.signingkey", t.path().join("key1").to_str().unwrap()]);
    ok(
        &d,
        &[
            "config",
            "set",
            "signing.allowedSigners",
            t.path().join("allowed").to_str().unwrap(),
        ],
    );
    (t, d)
}

#[test]
fn signed_commit_verifies_and_unsigned_and_unknown_do_not() {
    let (t, d) = setup();
    commit(&d, "a.txt", &["-S"]);
    assert!(ok(&d, &["verify-commit"]).contains("good signature from alice"));
    // the signature trailer is not shown in log/show
    let log = ok(&d, &["log", "--show-signature"]);
    assert!(log.contains("Signature: good signature from alice"), "{log}");
    assert!(!log.contains("ALLGIT SIGNATURE"), "{log}");
    assert!(!ok(&d, &["show"]).contains("ALLGIT SIGNATURE"));

    commit(&d, "b.txt", &[]);
    assert!(fail(&d, &["verify-commit"]).contains("no signature"));

    // signed by key2, which is not allowed
    commit(&d, "c.txt", &["-S", t.path().join("key2").to_str().unwrap()]);
    assert!(fail(&d, &["verify-commit"]).contains("unknown key"));
    // allow it
    ok(
        &d,
        &[
            "config",
            "set",
            "signing.allowedSigners",
            t.path().join("allowed2").to_str().unwrap(),
        ],
    );
    assert!(ok(&d, &["verify-commit"]).contains("good signature from bob"));
    // revoke key1: earlier commit signed by it is reported revoked
    ok(
        &d,
        &["config", "set", "signing.revoked", t.path().join("revoked").to_str().unwrap()],
    );
    assert!(fail(&d, &["verify-commit", "HEAD~2"]).contains("REVOKED"));
    assert!(ok(&d, &["verify-commit"]).contains("bob"));
}

#[test]
fn config_commit_sign_and_no_sign() {
    let (_t, d) = setup();
    ok(&d, &["config", "set", "commit.sign", "true"]);
    commit(&d, "a.txt", &[]);
    assert!(ok(&d, &["verify-commit"]).contains("good"));
    commit(&d, "b.txt", &["--no-sign"]);
    assert!(fail(&d, &["verify-commit"]).contains("no signature"));
}

#[test]
fn encrypted_key_is_refused() {
    let (t, d) = setup();
    std::fs::write(d.join("a"), "a").unwrap();
    ok(&d, &["add", "a"]);
    let out = fail(&d, &["commit", "-m", "x", "-S", t.path().join("enc").to_str().unwrap()]);
    assert!(out.contains("passphrase-protected"), "{out}");
    // nothing was committed
    assert!(!fail(&d, &["verify-commit"]).is_empty());
}

#[test]
fn signed_tag_verifies_and_tamper_is_bad() {
    let (_t, d) = setup();
    commit(&d, "a.txt", &[]);
    ok(&d, &["tag", "v1", "-m", "release", "-s"]);
    assert!(ok(&d, &["verify-tag", "v1"]).contains("good signature from alice"));
    assert!(ok(&d, &["show", "v1"]).contains("Signature: good signature from alice"));
    // an unsigned annotated tag and a legacy opaque one do not verify
    ok(&d, &["tag", "v2", "-m", "x"]);
    assert!(fail(&d, &["verify-tag", "v2"]).contains("no signature"));
    ok(&d, &["tag", "v3", "-m", "x", "--signature", "opaque"]);
    assert!(fail(&d, &["verify-tag", "v3"]).contains("BAD"));
    ok(&d, &["tag", "light"]);
    assert!(fail(&d, &["verify-tag", "light"]).contains("lightweight"));
    assert!(fail(&d, &["tag", "v4", "-s"]).contains("--sign needs"));
}
