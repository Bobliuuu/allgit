//! Persistent warm cache end to end: a second clone with a populated cache must not need the
//! remote's pack bytes (unix: the remote files are made unreadable), and a corrupted cache entry must be ignored (never trusted).
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn vcs(dir: &Path, cache: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_CACHE_LOCAL", "1")
        .env("VCS_CACHE_DIR", cache)
        .output()
        .unwrap()
}
fn ok(dir: &Path, cache: &Path, args: &[&str]) {
    let o = vcs(dir, cache, args);
    assert!(o.status.success(), "vcs {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}
fn files(p: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(p).unwrap().flatten() {
        if e.path().is_dir() {
            files(&e.path(), out);
        } else {
            out.push(e.path());
        }
    }
}

#[cfg(unix)]
#[test]
fn warm_cache_serves_clone_without_remote_pack_reads_and_rejects_corruption() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    let cache = root.join("cache");
    let remote = root.join("remote");
    let r = remote.to_str().unwrap();
    ok(root, &cache, &["init", "a"]);
    let a = root.join("a");
    for i in 0..20 {
        std::fs::write(a.join(format!("f{i}.txt")), format!("content {i}\n").repeat(50)).unwrap();
    }
    ok(&a, &cache, &["add", "."]);
    ok(&a, &cache, &["commit", "-m", "init"]);
    ok(&a, &cache, &["remote", "set", r]);
    ok(&a, &cache, &["publish"]);

    ok(root, &cache, &["clone", r, "b"]); // cold: fills the cache
    let mut cached = Vec::new();
    files(&cache, &mut cached);
    assert!(!cached.is_empty(), "cold clone populated the cache");

    // Hide every immutable object/pack byte from the remote; the manifest and listing stay.
    let mut hidden = Vec::new();
    for sub in ["packs", "objects"] {
        let d = remote.join(sub);
        if d.exists() {
            files(&d, &mut hidden);
        }
    }
    use std::os::unix::fs::PermissionsExt;
    for f in &hidden {
        std::fs::set_permissions(f, std::fs::Permissions::from_mode(0)).unwrap();
    }
    let o = vcs(root, &cache, &["clone", r, "c"]);
    assert!(o.status.success(), "warm clone must not read remote bytes: {}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        std::fs::read_to_string(root.join("c/f3.txt")).unwrap(),
        std::fs::read_to_string(a.join("f3.txt")).unwrap()
    );
    // restore, corrupt every cache entry, clone again: must succeed with correct data
    for f in &hidden {
        std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    for f in &cached {
        let mut raw = std::fs::read(f).unwrap();
        let n = raw.len() / 2;
        raw[n] ^= 0xff;
        std::fs::write(f, raw).unwrap();
    }
    ok(root, &cache, &["clone", r, "d"]);
    assert_eq!(
        std::fs::read_to_string(root.join("d/f7.txt")).unwrap(),
        std::fs::read_to_string(a.join("f7.txt")).unwrap()
    );
}
