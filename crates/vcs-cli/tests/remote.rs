//! End-to-end remote workflow with the real `vcs` binary and a local directory remote.
use std::path::Path;
use std::process::{Command, Output};

fn vcs(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .output()
        .unwrap()
}
fn ok(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(o.status.success(), "vcs {args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn fail(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(!o.status.success(), "vcs {args:?} unexpectedly succeeded");
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn write(dir: &Path, f: &str, c: &str) {
    let p = dir.join(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}

#[test]
fn multi_zone_remote_workflow() {
    let t = tempfile::tempdir().unwrap();
    let remote = t.path().join("remote");
    let r = remote.to_str().unwrap();
    workflow(t.path(), r, &format!("file://{r}"), r);
}

/// Publish/clone/fetch/sync/diverge workflow against any remote location.
/// `set_url` goes to `remote set`; the clones use `sparse_url` and `full_url`.
fn workflow(root: &Path, set_url: &str, sparse_url: &str, full_url: &str) {
    let a = root.join("a");
    ok(root, &["init", "a"]);
    ok(&a, &["zone", "create", "engine", "--path", "engine"]);
    ok(&a, &["zone", "create", "assets", "--path", "assets"]);
    write(&a, "engine/lib.rs", "1\n");
    write(&a, "assets/logo.txt", "logo\n");
    write(&a, "README", "hi\n");
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "init"]);

    assert!(ok(&a, &["remote", "show"]).contains("no remote configured"));
    assert!(fail(&a, &["publish"]).contains("vcs remote set"));
    ok(&a, &["remote", "set", set_url]);
    assert!(ok(&a, &["remote", "show"]).contains(set_url));
    let out = ok(&a, &["publish"]);
    assert!(out.contains("engine/main (none) ->"), "{out}");
    assert!(out.contains("assets/main (none) ->"), "{out}");
    assert!(out.contains("published atomically"), "{out}");
    assert!(ok(&a, &["publish"]).contains("nothing to publish"));

    // sparse clone
    let out = ok(root, &["clone", sparse_url, "b", "--zone", "engine"]);
    assert!(out.contains("downloaded zones: engine"), "{out}");
    assert!(out.contains("skipped zones (not subscribed): "), "{out}");
    assert!(out.contains("assets"), "{out}");
    let b = root.join("b");
    assert!(b.join("engine/lib.rs").exists());
    assert!(!b.join("assets").exists());
    assert!(ok(&b, &["status"]).contains("not subscribed"));
    assert!(ok(&b, &["remote", "show"]).contains("engine/main"));

    // full clone
    ok(root, &["clone", full_url, "c"]);
    let c = root.join("c");
    assert!(c.join("assets/logo.txt").exists());

    // disjoint publishes
    write(&b, "engine/lib.rs", "2\n");
    ok(&b, &["add", "."]);
    ok(&b, &["commit", "-m", "engine 2"]);
    let out = ok(&b, &["publish"]);
    assert!(out.contains("engine/main"), "{out}");
    write(&c, "assets/logo.txt", "logo2\n");
    ok(&c, &["add", "."]);
    ok(&c, &["commit", "-m", "assets 2"]);
    let out = ok(&c, &["publish"]);
    assert!(out.contains("assets/main"), "{out}");

    let out = ok(&c, &["sync"]);
    assert!(out.contains("engine/main: fast-forwarded"), "{out}");
    assert!(out.contains("assets/main: up to date"), "{out}");
    assert_eq!(std::fs::read_to_string(c.join("engine/lib.rs")).unwrap(), "2\n");
    let out = ok(&b, &["sync"]);
    assert!(out.contains("engine/main: up to date"), "{out}");
    assert!(!b.join("assets").exists());
    ok(&a, &["sync"]);
    assert_eq!(std::fs::read_to_string(a.join("assets/logo.txt")).unwrap(), "logo2\n");

    // non-fast-forward refusal: both change engine, B publishes first
    write(&b, "engine/lib.rs", "3b\n");
    ok(&b, &["add", "."]);
    ok(&b, &["commit", "-m", "b"]);
    write(&c, "engine/lib.rs", "3c\n");
    ok(&c, &["add", "."]);
    ok(&c, &["commit", "-m", "c"]);
    ok(&b, &["publish"]);
    let err = fail(&c, &["publish"]);
    assert!(err.contains("Cannot publish engine/main"), "{err}");
    assert!(err.contains("Options:\n  vcs sync"), "{err}");
    let out = ok(&c, &["sync"]);
    assert!(
        out.contains("engine/main: diverged; merge with `vcs merge --remote main --zone engine`"),
        "{out}"
    );
}

#[test]
fn unknown_scheme_is_rejected() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let r = t.path().join("r");
    let err = fail(t.path(), &["clone", "gs://bucket/x", "d"]);
    assert!(err.contains("not supported in this build"), "{err}");
    ok(&r, &["remote", "set", "gs://bucket/x"]);
    let err = fail(&r, &["fetch"]);
    assert!(err.contains("not supported in this build"), "{err}");
}

#[test]
fn remote_set_stores_absolute_path_and_publish_works_from_subdir() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    ok(&root, &["init", "demo"]);
    let demo = root.join("demo");
    write(&demo, "sub/f.txt", "1\n");
    ok(&demo, &["add", "."]);
    ok(&demo, &["commit", "-m", "one"]);
    let sub = demo.join("sub");
    ok(&sub, &["remote", "set", "../../shared.vcs"]);
    let saved = std::fs::read_to_string(demo.join(".vcs/remote")).unwrap();
    let saved = saved.trim();
    assert!(Path::new(saved).is_absolute(), "not absolute: {saved}");
    assert!(!saved.contains(".."), "not cleaned: {saved}");
    assert_eq!(Path::new(saved), root.join("shared.vcs"));
    ok(&sub, &["publish"]);
    ok(&demo, &["publish"]);
}

#[test]
fn dash_c_runs_in_other_directory() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    ok(root, &["init", "demo"]);
    let demo = root.join("demo");
    write(&demo, "a.txt", "x\n");
    let inside = ok(&demo, &["status"]);
    let outside = ok(root, &["-C", "demo", "status"]);
    assert_eq!(inside, outside);
    let err = fail(root, &["-C", "nope", "status"]);
    assert!(err.contains("cannot change to directory"), "{err}");
}

#[test]
fn deleted_cached_object_is_refetched_by_the_next_command() {
    let t = tempfile::tempdir().unwrap();
    let remote = t.path().join("remote");
    ok(t.path(), &["init", "a"]);
    let a = t.path().join("a");
    std::fs::write(a.join("f.txt"), "cached content\n").unwrap();
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "init"]);
    ok(&a, &["remote", "set", remote.to_str().unwrap()]);
    ok(&a, &["publish"]);
    ok(t.path(), &["clone", remote.to_str().unwrap(), "b"]);
    let b = t.path().join("b");
    // Evict every blob-sized loose object that is the file's content.
    let id = vcs_objects::Object::Blob(b"cached content\n".to_vec()).id();
    let h = id.to_hex();
    let p = b.join(".vcs/objects").join(&h[..2]).join(&h[2..]);
    assert!(p.exists());
    std::fs::remove_file(&p).unwrap();
    assert!(ok(&b, &["fsck"]).contains("refetched on demand"));
    // `diff` must read the committed blob to compare against the edited file.
    std::fs::write(b.join("f.txt"), "cached content\nmore\n").unwrap();
    let diff = ok(&b, &["diff"]);
    assert!(diff.contains("+more"), "{diff}");
    assert!(p.exists(), "object was refetched into the store");
}

#[test]
fn s3_remote_without_credentials_is_a_clear_error() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let r = t.path().join("r");
    ok(&r, &["remote", "set", "s3://bkt/p?endpoint=http://127.0.0.1:1"]);
    let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(["fetch"])
        .current_dir(&r)
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .output()
        .unwrap();
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("AWS_ACCESS_KEY_ID is not set"), "{err}");
    assert!(!err.contains("panicked"), "{err}");
}

#[test]
fn s3_remote_rejects_secrets_in_url() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let r = t.path().join("r");
    ok(&r, &["remote", "set", "s3://bkt?endpoint=http://127.0.0.1:1&secret_key=hunter2"]);
    let err = fail(&r, &["fetch"]);
    assert!(err.contains("unknown option"), "{err}");
    assert!(!err.contains("hunter2"), "{err}");
}

/// Gated on `VCS_TEST_S3_ENDPOINT` (see docs/s3.md); skips silently otherwise.
#[test]
fn s3_minio_remote_workflow() {
    let Ok(endpoint) = std::env::var("VCS_TEST_S3_ENDPOINT") else {
        eprintln!("skipping: VCS_TEST_S3_ENDPOINT not set");
        return;
    };
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
    let bucket = var("VCS_TEST_S3_BUCKET", "vcs-test");
    let (ak, sk) = (
        var("VCS_TEST_S3_ACCESS_KEY", "minioadmin"),
        var("VCS_TEST_S3_SECRET_KEY", "minioadmin"),
    );
    vcs_store_s3::S3Backend::new_unchecked(vcs_store_s3::S3Config::new(&endpoint, &bucket, &ak, &sk))
        .unwrap()
        .ensure_bucket()
        .unwrap();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let url = format!("s3://{bucket}/cli-{}-{nonce}?endpoint={endpoint}", std::process::id());
    std::env::set_var("AWS_ACCESS_KEY_ID", &ak);
    std::env::set_var("AWS_SECRET_ACCESS_KEY", &sk);
    let t = tempfile::tempdir().unwrap();
    workflow(t.path(), &url, &url, &url);
    // no secret ever lands on disk
    for repo in ["a", "b", "c"] {
        for e in walk(&t.path().join(repo).join(".vcs")) {
            let bytes = std::fs::read(&e).unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains(&sk) || sk.len() < 8, "secret found in {}", e.display());
        }
    }
    assert!(std::fs::read_to_string(t.path().join("a/.vcs/remote")).unwrap().contains(&bucket));
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p))
        } else {
            out.push(p)
        }
    }
    out
}

#[cfg(not(feature = "tls"))]
#[test]
fn s3_https_endpoint_without_tls_feature_names_the_feature() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let r = t.path().join("r");
    ok(&r, &["remote", "set", "s3://bkt?endpoint=https://127.0.0.1:1"]);
    let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(["fetch"])
        .current_dir(&r)
        .env("AWS_ACCESS_KEY_ID", "k")
        .env("AWS_SECRET_ACCESS_KEY", "s")
        .output()
        .unwrap();
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("tls"), "{err}");
}
