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
    let root = t.path();
    let remote = root.join("remote");
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
    ok(&a, &["remote", "set", remote.to_str().unwrap()]);
    assert!(ok(&a, &["remote", "show"]).contains(remote.to_str().unwrap()));
    let out = ok(&a, &["publish"]);
    assert!(out.contains("engine/main (none) ->"), "{out}");
    assert!(out.contains("assets/main (none) ->"), "{out}");
    assert!(out.contains("published atomically"), "{out}");
    assert!(ok(&a, &["publish"]).contains("nothing to publish"));

    // sparse clone
    let url = format!("file://{}", remote.display());
    let out = ok(root, &["clone", &url, "b", "--zone", "engine"]);
    assert!(out.contains("downloaded zones: engine"), "{out}");
    assert!(out.contains("skipped zones (not subscribed): "), "{out}");
    assert!(out.contains("assets"), "{out}");
    let b = root.join("b");
    assert!(b.join("engine/lib.rs").exists());
    assert!(!b.join("assets").exists());
    assert!(ok(&b, &["status"]).contains("not subscribed"));
    assert!(ok(&b, &["remote", "show"]).contains("engine/main"));

    // full clone
    ok(root, &["clone", remote.to_str().unwrap(), "c"]);
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
    let err = fail(t.path(), &["clone", "s3://bucket/x", "d"]);
    assert!(err.contains("not supported in this build"), "{err}");
    ok(&r, &["remote", "set", "s3://bucket/x"]);
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
