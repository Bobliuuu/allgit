//! Export to a fast-import stream. The stream is checked structurally; if a `git` binary exists the test
//! also feeds it to `git fast-import` (test-only: the core crates never call git).
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use vcs_objects::Signature;
use vcs_repo::Repo;

fn sig(ts: u64) -> Signature {
    Signature {
        name: "Ann".into(),
        email: "ann@x".into(),
        timestamp: ts,
    }
}
fn write(r: &Repo, p: &str, c: &[u8]) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
}
fn commit(r: &Repo, m: &str, ts: u64) {
    r.add(&["".to_string()]).unwrap();
    r.commit(m, sig(ts)).unwrap();
}
fn have_git() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}
fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let o = Command::new("git").current_dir(dir).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

/// base on main; feature adds/edits/deletes; main moves; merge-free two-branch history plus a zone.
fn build() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    write(&r, "a.txt", b"one\n");
    write(&r, "dir/b.bin", &[0u8, 1, 2, 255]);
    write(&r, "dir/dup.bin", &[0u8, 1, 2, 255]);
    write(&r, "has space/c.txt", b"c\n");
    commit(&r, "base\n\nbody line", 1_700_000_000);
    r.create_branch("root", "feature").unwrap();
    r.checkout("root", "feature").unwrap();
    write(&r, "a.txt", b"two\n");
    fs::remove_file(r.worktree().join("dir/dup.bin")).unwrap();
    commit(&r, "feature edit", 1_700_000_100);
    r.checkout("root", "main").unwrap();
    write(&r, "m.txt", b"main only\n");
    commit(&r, "main work", 1_700_000_200);
    (d, r)
}

fn export(r: &Repo, b: &str) -> (Vec<u8>, vcs_repo::ExportReport) {
    let mut buf = Vec::new();
    let rep = r.export_git("root", b, &mut buf).unwrap();
    (buf, rep)
}

#[test]
fn stream_is_deterministic_and_dedupes_blobs() {
    let (_d, r) = build();
    let (a, rep) = export(&r, "feature");
    let (b, _) = export(&r, "feature");
    assert_eq!(a, b);
    assert_eq!(rep.commits, 2);
    // base: a.txt, b.bin(=dup.bin), c.txt -> 3 distinct blobs; feature adds "two\n" -> 4
    assert_eq!(rep.blobs, 4);
    let t = String::from_utf8_lossy(&a);
    assert!(t.contains("commit refs/heads/feature\n"));
    assert!(t.contains("D dir/dup.bin\n"));
    assert!(t.contains("Vcs-Change-Id: "));
    assert!(t.ends_with("done\n"));
}

#[test]
fn bad_inputs_are_errors() {
    let (_d, r) = build();
    assert!(r.export_git("root", "nope", &mut Vec::new()).is_err());
    assert!(r.export_git("nosuch", "main", &mut Vec::new()).is_err());
}

#[test]
fn git_can_read_the_stream() {
    if !have_git() {
        eprintln!("git not installed; skipping");
        return;
    }
    let (d, r) = build();
    let (stream, _) = export(&r, "feature");
    let g = d.path().join("g");
    git(d.path(), &["init", "-q", "--bare", "g"]);
    let mut c = Command::new("git")
        .current_dir(&g)
        .args(["fast-import", "--quiet"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(&stream).unwrap();
    let o = c.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    git(&g, &["fsck", "--strict"]);
    let log = git(&g, &["log", "--format=%an|%ae|%at|%s", "refs/heads/feature"]);
    assert_eq!(log, "Ann|ann@x|1700000100|feature edit\nAnn|ann@x|1700000000|base\n");
    assert_eq!(git(&g, &["show", "refs/heads/feature:a.txt"]), "two\n");
    assert_eq!(git(&g, &["show", "refs/heads/feature:has space/c.txt"]), "c\n");
    let tree = git(&g, &["ls-tree", "-r", "--name-only", "refs/heads/feature"]);
    assert_eq!(tree, "a.txt\ndir/b.bin\nhas space/c.txt\n");
    let body = git(&g, &["log", "-1", "--format=%B", "refs/heads/feature~1"]);
    assert!(body.starts_with("base\n\nbody line\n"), "{body}");
}
