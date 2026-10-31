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

fn fast_import(g: &std::path::Path, stream: &[u8], marks: &std::path::Path) {
    let mut c = Command::new("git")
        .current_dir(g)
        .args(["fast-import", "--quiet"])
        .arg(format!("--export-marks={}", marks.display()))
        .arg(format!("--import-marks-if-exists={}", marks.display()))
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(stream).unwrap();
    let o = c.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

fn inc(r: &Repo) -> (Vec<u8>, vcs_repo::ExportReport) {
    let mut buf = Vec::new();
    let opts = vcs_repo::ExportOptions {
        incremental: true,
        onto_origin: false,
        record: true,
    };
    let rep = r.export_git_with("root", "main", &mut buf, opts).unwrap();
    (buf, rep)
}

#[cfg(unix)]
#[test]
fn symlinks_and_tags_are_exported_and_export_is_incremental() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    write(&r, "a.txt", b"one\n");
    std::os::unix::fs::symlink("a.txt", r.worktree().join("ln")).unwrap();
    commit(&r, "first", 1_700_000_000);
    r.tag_create("root", "light", None, None).unwrap();
    r.tag_create("root", "v1", None, Some(("release one\n", sig(1_700_000_050), vec![])))
        .unwrap();

    let (s1, rep1) = inc(&r);
    let t1 = String::from_utf8_lossy(&s1).into_owned();
    assert_eq!((rep1.commits, rep1.commits_known, rep1.tags, rep1.symlinks), (1, 0, 2, 1));
    assert!(t1.contains("M 120000 :"), "{t1}");
    assert!(t1.contains("reset refs/tags/light\nfrom :"));
    assert!(t1.contains("tag v1\nfrom :"));
    assert!(t1.contains("tagger Ann <ann@x> 1700000050 +0000\ndata 12\nrelease one\n"));

    // second run with nothing new: no commit commands
    let (s0, rep0) = inc(&r);
    assert_eq!((rep0.commits, rep0.commits_known), (0, 1));
    assert!(!String::from_utf8_lossy(&s0).contains("commit refs/"));

    write(&r, "b.txt", b"two\n");
    commit(&r, "second", 1_700_000_100);
    let (s2, rep2) = inc(&r);
    let t2 = String::from_utf8_lossy(&s2).into_owned();
    assert_eq!((rep2.commits, rep2.commits_known), (1, 1));
    assert_eq!(t2.matches("commit refs/heads/main\n").count(), 1);
    assert!(t2.contains("\nfrom :3\nM 100644 :4 b.txt"), "{t2}"); // parent by mark, marks continue
    assert!(!t2.contains("a.txt"), "old files must not be re-sent");

    if !have_git() {
        eprintln!("git not installed; skipping the git half");
        return;
    }
    let g = d.path().join("g");
    git(d.path(), &["init", "-q", "--bare", "g"]);
    let marks = d.path().join("marks");
    fast_import(&g, &s1, &marks);
    let first = git(&g, &["rev-parse", "refs/heads/main"]);
    assert_eq!(git(&g, &["ls-tree", "refs/heads/main"]).matches("120000 blob").count(), 1);
    assert_eq!(git(&g, &["cat-file", "-t", "refs/tags/v1"]).trim(), "tag");
    assert_eq!(git(&g, &["cat-file", "-t", "refs/tags/light"]).trim(), "commit");
    assert!(git(&g, &["cat-file", "-p", "refs/tags/v1"]).contains("release one"));
    fast_import(&g, &s2, &marks);
    git(&g, &["fsck", "--strict"]);
    assert_eq!(
        git(&g, &["rev-parse", "refs/heads/main~1"]),
        first,
        "old commit SHA must be unchanged"
    );
    assert_eq!(git(&g, &["rev-list", "--count", "refs/heads/main"]).trim(), "2");
}

#[test]
fn full_export_without_options_ignores_the_table() {
    let (_d, r) = build();
    let (a, _) = export(&r, "main");
    let _ = inc(&r); // writes the table; a plain export must ignore it
    let (b, rep) = export(&r, "main");
    assert_eq!(a, b);
    assert_eq!(rep.commits_known, 0);
}
