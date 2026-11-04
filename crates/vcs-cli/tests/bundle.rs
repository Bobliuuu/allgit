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
    let n = bytes.len() - 32 - 40; // inside the pack, past the header
    bytes[n] ^= 1;
    std::fs::write(&b2, bytes).unwrap();
    let b2s = b2.to_str().unwrap();
    assert!(fail(root, &["bundle", "verify", "--full", b2s]).contains("hash mismatch"));
    // the default verify is the cheap one (documented gap: it does not read the pack body) ...
    let out = ok(root, &["bundle", "verify", b2s]);
    assert!(out.contains("quick") && out.contains("--full"), "{out}");
    assert!(ok(root, &["bundle", "verify", "--quick", b2s]).contains("quick"));
    // ... but the receiver always hashes everything, so the corrupt bundle is never applied
    assert!(fail(&c, &["fetch", b2s]).contains("hash mismatch"));
}

#[test]
fn full_and_quick_agree_on_a_good_bundle_and_conflict_as_flags() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    ok(root, &["init", "a"]);
    let a = root.join("a");
    std::fs::write(a.join("f"), "x").unwrap();
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "one"]);
    let b = root.join("g.bundle");
    let bs = b.to_str().unwrap();
    ok(&a, &["bundle", "create", bs]);
    let q = ok(root, &["bundle", "verify", bs]);
    let f = ok(root, &["bundle", "verify", "--full", bs]);
    let head = |s: &str| s.lines().take_while(|l| !l.starts_with("ok")).collect::<Vec<_>>().join("\n");
    assert_eq!(head(&q), head(&f));
    assert!(f.trim_end().ends_with("ok"), "{f}");
    fail(root, &["bundle", "verify", "--full", "--quick", bs]);
}
