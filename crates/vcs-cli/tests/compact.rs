//! `vcs gc --remote --compact`, the publish hint, and `vcs repack` compaction, end to end.
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
fn packs(remote: &Path) -> usize {
    std::fs::read_dir(remote.join("packs"))
        .map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".idx")).count())
        .unwrap_or(0)
}

#[test]
fn remote_compaction_end_to_end() {
    let t = tempfile::tempdir().unwrap();
    let (a, remote) = (t.path().join("a"), t.path().join("remote"));
    ok(t.path(), &["init", "a"]);
    ok(&a, &["remote", "set", remote.to_str().unwrap()]);
    let mut hint = false;
    for i in 0..34 {
        std::fs::write(a.join("f.txt"), format!("version {i}\n{}", "line of text\n".repeat(50))).unwrap();
        std::fs::write(a.join(format!("g{i}.txt")), format!("file {i}\n")).unwrap();
        ok(&a, &["add", "."]);
        ok(&a, &["commit", "-m", &format!("c{i}")]);
        hint |= ok(&a, &["publish"]).contains("hint: the remote holds");
    }
    assert!(packs(&remote) >= 20, "one pack per publish: {}", packs(&remote));
    assert!(hint, "publish suggests compaction once the remote holds many packs");
    let before = packs(&remote);

    // a clone before compaction, for later comparison
    let b0 = t.path().join("b0");
    ok(t.path(), &["clone", remote.to_str().unwrap(), "b0"]);
    let log0 = ok(&b0, &["log", "--oneline", "-n", "100"]);

    // safety rails
    let e = fail(&a, &["gc", "--remote", "--compact"]);
    assert!(e.contains("--yes"), "{e}");
    assert!(fail(&a, &["gc", "--compact"]).contains("--remote"));
    let dry = ok(&a, &["gc", "--remote", "--compact", "--dry-run"]);
    assert!(dry.contains("nothing was changed") && packs(&remote) == before, "{dry}");

    let out = ok(&a, &["gc", "--remote", "--compact", "--yes"]);
    assert!(out.contains("merged"), "{out}");
    let after = packs(&remote);
    assert!(after < before / 2, "{before} -> {after}\n{out}");

    // everything still readable: fresh clone, fsck, same history; the old clone can fetch too
    let b1 = t.path().join("b1");
    ok(t.path(), &["clone", remote.to_str().unwrap(), "b1"]);
    assert_eq!(ok(&b1, &["log", "--oneline", "-n", "100"]), log0);
    assert!(ok(&b1, &["fsck"]).contains("ok"));
    assert_eq!(std::fs::read_to_string(b1.join("g33.txt")).unwrap(), "file 33\n");
    std::fs::write(a.join("h.txt"), "more\n").unwrap();
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "after compaction"]);
    ok(&a, &["publish"]);
    ok(&b0, &["sync"]);
    assert_eq!(std::fs::read_to_string(b0.join("h.txt")).unwrap(), "more\n");

    // rollback: v2 output, readable the same way
    let out = ok(&a, &["gc", "--remote", "--compact", "--yes", "--pack-format", "v2"]);
    assert!(out.contains("pack(s) on the remote"), "{out}");
    let b2 = t.path().join("b2");
    ok(t.path(), &["clone", remote.to_str().unwrap(), "b2"]);
    assert!(ok(&b2, &["fsck"]).contains("ok"));
    assert!(fail(&a, &["gc", "--remote", "--compact", "--yes", "--pack-format", "v9"]).contains("unknown pack format"));
}

#[test]
fn repack_merges_small_local_packs() {
    let t = tempfile::tempdir().unwrap();
    let a = t.path().join("a");
    ok(t.path(), &["init", "a"]);
    let count = || {
        std::fs::read_dir(a.join(".vcs/objects/pack"))
            .map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".idx")).count())
            .unwrap_or(0)
    };
    for i in 0..9 {
        std::fs::write(a.join(format!("f{i}.txt")), format!("content {i}\n")).unwrap();
        ok(&a, &["add", "."]);
        ok(&a, &["commit", "-m", &format!("c{i}")]);
        ok(&a, &["repack", "--no-compact"]);
    }
    let before = count();
    assert!(before >= 8, "{before}");
    let out = ok(&a, &["repack"]);
    assert!(out.contains("merged"), "{out}");
    assert!(count() < before / 2, "{} -> {}", before, count());
    assert!(ok(&a, &["fsck"]).contains("ok"));
    assert_eq!(std::fs::read_to_string(a.join("f3.txt")).unwrap(), "content 3\n");
}
