//! `vcs bundle`, `vcs clone <bundle>` and `vcs fetch <bundle>` with the real binary.
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

#[test]
fn bundle_create_verify_clone_and_incremental_fetch() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    ok(root, &["init", "a"]);
    let a = root.join("a");
    ok(&a, &["zone", "create", "engine", "--path", "engine"]);
    std::fs::create_dir_all(a.join("engine")).unwrap();
    std::fs::write(a.join("engine/core.rs"), "v1").unwrap();
    std::fs::write(a.join("README"), "hi").unwrap();
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "one"]);
    let b1 = root.join("one.bundle");
    ok(&a, &["bundle", "create", b1.to_str().unwrap()]);
    let out = ok(root, &["bundle", "verify", b1.to_str().unwrap()]);
    assert!(out.contains("ok"), "{out}");
    ok(root, &["clone", b1.to_str().unwrap(), "c"]);
    let c = root.join("c");
    assert_eq!(std::fs::read_to_string(c.join("engine/core.rs")).unwrap(), "v1");
    assert!(ok(&c, &["fsck"]).contains("ok"));
    // incremental
    std::fs::write(a.join("engine/core.rs"), "v2").unwrap();
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "two"]);
    let b2 = root.join("two.bundle");
    let out = ok(&a, &["bundle", "create", b2.to_str().unwrap(), "engine", "--since", "HEAD~1"]);
    assert!(out.contains("requires commit"), "{out}");
    let out = ok(&c, &["fetch", b2.to_str().unwrap()]);
    assert!(out.contains("fetched engine/main"), "{out}");
    assert!(fail(root, &["clone", b2.to_str().unwrap(), "d"]).contains("incremental"));
    // a corrupted file is refused
    let mut bytes = std::fs::read(&b2).unwrap();
    let n = bytes.len() / 2;
    bytes[n] ^= 1;
    std::fs::write(&b2, bytes).unwrap();
    assert!(fail(root, &["bundle", "verify", b2.to_str().unwrap()]).contains("hash mismatch"));
}

#[test]
fn clone_bundle_for_path_takes_only_the_selected_zone() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    ok(root, &["init", "a"]);
    let a = root.join("a");
    ok(&a, &["zone", "create", "engine", "--path", "engine"]);
    ok(&a, &["zone", "create", "docs", "--path", "docs"]);
    for (f, c) in [("engine/core.rs", "e"), ("docs/guide.md", "d"), ("README", "r")] {
        std::fs::create_dir_all(a.join(f).parent().unwrap()).unwrap();
        std::fs::write(a.join(f), c).unwrap();
    }
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "one"]);
    let b = root.join("x.bundle");
    ok(&a, &["bundle", "create", b.to_str().unwrap()]);
    // dry run writes nothing
    ok(root, &["clone", b.to_str().unwrap(), "dry", "--for", "engine", "--dry-run"]);
    assert!(!root.join("dry").exists());
    ok(root, &["clone", b.to_str().unwrap(), "c", "--for", "engine/core.rs"]);
    let c = root.join("c");
    assert_eq!(std::fs::read_to_string(c.join("engine/core.rs")).unwrap(), "e");
    assert!(!c.join("docs").exists(), "unselected zone must not be checked out");
    assert!(ok(&c, &["fsck"]).contains("ok"));
    // the rest can be added later
    ok(&c, &["zone", "add", "docs"]);
    assert_eq!(std::fs::read_to_string(c.join("docs/guide.md")).unwrap(), "d");
    // unresolvable query is refused and leaves no destination
    let e = fail(root, &["clone", b.to_str().unwrap(), "bad", "--for", "nosuchthing"]);
    assert!(e.contains("could not resolve") || e.contains("nothing"), "{e}");
    assert!(!root.join("bad").exists());
}
