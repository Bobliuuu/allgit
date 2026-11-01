//! End-to-end: drives the real `vcs` binary through the README workflow.
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

#[test]
fn first_demo_workflow() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "demo"]);
    let d = t.path().join("demo");
    std::fs::write(d.join("hello.txt"), "hello\n").unwrap();
    ok(&d, &["add", "hello.txt"]);
    assert!(ok(&d, &["commit", "-m", "Initial commit"]).contains("Initial commit"));
    std::fs::write(d.join("hello.txt"), "hello\nworld\n").unwrap();
    assert!(ok(&d, &["status"]).contains("modified:  hello.txt"));
    assert!(ok(&d, &["diff"]).contains("+world"));
    ok(&d, &["add", "hello.txt"]);
    ok(&d, &["commit", "-m", "Update greeting"]);
    let log = ok(&d, &["log"]);
    assert!(log.find("Update greeting").unwrap() < log.find("Initial commit").unwrap());
    assert!(!d.join(".git").exists());
    assert!(ok(&d, &["fsck"]).contains("ok"));
}

#[test]
fn zone_workflow_and_errors() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    std::fs::create_dir_all(d.join("engine")).unwrap();
    std::fs::write(d.join("engine/a.rs"), "1\n").unwrap();
    ok(&d, &["zone", "create", "engine", "--path", "engine"]);
    ok(&d, &["add", "."]);
    ok(&d, &["commit", "-m", "init"]);
    ok(&d, &["zone", "branch", "engine", "feature"]);
    ok(&d, &["zone", "checkout", "engine", "feature"]);
    std::fs::write(d.join("engine/a.rs"), "2\n").unwrap();
    ok(&d, &["add", "engine"]);
    ok(&d, &["commit", "-m", "feat"]);
    let list = ok(&d, &["zone", "list"]);
    assert!(list.contains("subscribed, on feature"), "{list}");
    assert!(list.contains("subscribed, on main"), "{list}");
    // overlapping zone -> nonzero exit + message
    let o = vcs(&d, &["zone", "create", "x", "--path", "engine/sub"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("overlaps"));
    // outside a repo
    let o = vcs(t.path(), &["status"]);
    assert!(!o.status.success());
    // log from inside the zone dir defaults to that zone
    assert!(ok(&d.join("engine"), &["log"]).contains("zone engine"));
}

#[test]
fn op_log_show_undo_restore_end_to_end() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    std::fs::write(d.join("a.txt"), "1\n").unwrap();
    ok(&d, &["add", "a.txt"]);
    ok(&d, &["commit", "-m", "first"]);
    let first_op = ok(&d, &["op", "log"]);
    let first_id: String = first_op.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned();
    std::fs::write(d.join("a.txt"), "2\n").unwrap();
    ok(&d, &["add", "a.txt"]);
    ok(&d, &["commit", "-m", "second"]);
    let log = ok(&d, &["op", "log"]);
    assert!(
        log.contains("commit: second") && log.contains("commit: first") && log.contains("init repository"),
        "{log}"
    );
    assert!(log.lines().next().unwrap().starts_with('@'));
    let show = ok(&d, &["op", "show", &first_id]);
    assert!(show.contains("commit: first") && show.contains("root/main"), "{show}");

    assert!(ok(&d, &["op", "undo"]).contains("undone"));
    assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "1\n");
    let l = ok(&d, &["log"]);
    assert!(l.contains("first") && !l.contains("second"));
    // dirty => refused, non-zero exit, nothing changed
    std::fs::write(d.join("a.txt"), "dirty\n").unwrap();
    let o = vcs(&d, &["op", "undo"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("uncommitted"));
    assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "dirty\n");
    ok(&d, &["add", "a.txt"]);
    ok(&d, &["commit", "-m", "third"]);
    // restore the first commit op; a bad prefix errors
    ok(&d, &["op", "restore", &first_id]);
    assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "1\n");
    assert!(!vcs(&d, &["op", "restore", "zzzz"]).status.success());
    assert!(ok(&d, &["fsck"]).contains("ok"));
}

#[test]
fn snapshot_checkout_zone_and_branch_management() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    std::fs::create_dir_all(d.join("engine")).unwrap();
    std::fs::write(d.join("engine/a.rs"), "1\n").unwrap();
    ok(&d, &["zone", "create", "engine", "--path", "engine"]);
    ok(&d, &["add", "."]);
    ok(&d, &["commit", "-m", "one"]);
    ok(&d, &["snapshot", "create", "v1"]);
    std::fs::write(d.join("engine/a.rs"), "2\n").unwrap();
    ok(&d, &["add", "."]);
    ok(&d, &["commit", "-m", "two"]);
    // dirty tree: snapshot checkout refuses and changes nothing
    std::fs::write(d.join("engine/a.rs"), "dirty\n").unwrap();
    assert!(!vcs(&d, &["snapshot", "checkout", "v1"]).status.success());
    assert_eq!(std::fs::read_to_string(d.join("engine/a.rs")).unwrap(), "dirty\n");
    std::fs::write(d.join("engine/a.rs"), "2\n").unwrap();
    assert!(ok(&d, &["snapshot", "checkout", "v1"]).contains("snapshot/v1"));
    assert_eq!(std::fs::read_to_string(d.join("engine/a.rs")).unwrap(), "1\n");
    // branch delete: refuses the checked-out branch, deletes another
    assert!(!vcs(&d, &["branch", "--zone", "engine", "-d", "snapshot/v1"]).status.success());
    ok(&d, &["zone", "checkout", "engine", "main"]);
    assert!(ok(&d, &["branch", "--zone", "engine", "-d", "snapshot/v1"]).contains("deleted engine/snapshot/v1"));
    // zone delete needs --force while history exists
    assert!(!vcs(&d, &["zone", "delete", "engine"]).status.success());
    ok(&d, &["zone", "delete", "engine", "--force"]);
    assert!(!d.join("engine/a.rs").exists());
    ok(&d, &["zone", "create", "docs", "--path", "docs"]);
    ok(&d, &["zone", "rename", "docs", "manual"]);
    assert!(ok(&d, &["zone", "list"]).contains("manual"));
    assert!(ok(&d, &["fsck"]).contains("ok"));
}

#[test]
fn repack_keeps_repo_working_and_fsck_catches_pack_corruption() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "p"]);
    let d = t.path().join("p");
    for i in 0..5 {
        std::fs::write(d.join(format!("f{i}.txt")), format!("content {i}\n")).unwrap();
    }
    ok(&d, &["add", "."]);
    ok(&d, &["commit", "-m", "five files"]);
    assert!(ok(&d, &["repack"]).contains("packed"));
    assert!(ok(&d, &["repack"]).contains("packed 0 object"));
    assert!(ok(&d, &["fsck"]).contains("ok"));
    assert!(ok(&d, &["log"]).contains("five files"));
    let pd = d.join(".vcs/objects/pack");
    let pack = std::fs::read_dir(&pd)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().unwrap() == "pack")
        .unwrap();
    let mut b = std::fs::read(&pack).unwrap();
    b[20] ^= 1;
    std::fs::write(&pack, b).unwrap();
    let o = vcs(&d, &["fsck"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("PROBLEM"));
}

#[test]
fn restore_rm_mv_commands() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "demo"]);
    let d = t.path().join("demo");
    std::fs::write(d.join("a.txt"), "a\n").unwrap();
    ok(&d, &["add", "a.txt"]);
    ok(&d, &["commit", "-m", "init"]);
    std::fs::write(d.join("a.txt"), "edit\n").unwrap();
    ok(&d, &["restore", "a.txt"]);
    assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "a\n");
    ok(&d, &["mv", "a.txt", "b.txt"]);
    assert!(d.join("b.txt").exists() && !d.join("a.txt").exists());
    ok(&d, &["restore", "--staged", "."]);
    assert!(ok(&d, &["status"]).contains("deleted"));
    std::fs::write(d.join("b.txt"), "dirty\n").unwrap();
    ok(&d, &["add", "b.txt"]);
    let o = vcs(&d, &["rm", "b.txt"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("uncommitted work"));
    ok(&d, &["rm", "--force", "b.txt"]);
    assert!(!d.join("b.txt").exists());
}

#[test]
fn cherry_pick_and_revert_commands() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "demo"]);
    let d = t.path().join("demo");
    std::fs::write(d.join("a.txt"), "a\n").unwrap();
    ok(&d, &["add", "a.txt"]);
    ok(&d, &["commit", "-m", "init"]);
    ok(&d, &["branch", "feat"]);
    ok(&d, &["switch", "feat"]);
    std::fs::write(d.join("b.txt"), "b\n").unwrap();
    ok(&d, &["add", "b.txt"]);
    ok(&d, &["commit", "-m", "add b"]);
    ok(&d, &["switch", "main"]);
    assert!(ok(&d, &["cherry-pick", "feat"]).contains("cherry-pick done"));
    assert!(d.join("b.txt").exists());
    assert!(ok(&d, &["revert", "HEAD"]).contains("revert done"));
    assert!(!d.join("b.txt").exists());
    assert!(ok(&d, &["log"]).contains("Revert \"add b\""));
    ok(&d, &["op", "undo"]);
    assert!(d.join("b.txt").exists());
}

#[test]
fn ignore_and_stash_via_cli() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "demo"]);
    let d = t.path().join("demo");
    std::fs::write(d.join(".vcsignore"), "*.o\n").unwrap();
    std::fs::write(d.join("a.txt"), "1\n").unwrap();
    std::fs::write(d.join("a.o"), "bin").unwrap();
    let st = ok(&d, &["status"]);
    assert!(st.contains("a.txt") && !st.contains("a.o"), "{st}");
    ok(&d, &["add", "."]);
    ok(&d, &["commit", "-m", "c"]);
    std::fs::write(d.join("a.txt"), "2\n").unwrap();
    assert!(ok(&d, &["stash", "push", "-m", "wip"]).contains("stashed"));
    assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "1\n");
    assert!(ok(&d, &["stash", "list"]).contains("stash@{0}: wip"));
    ok(&d, &["stash", "pop"]);
    assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "2\n");
    assert!(ok(&d, &["stash", "list"]).trim().is_empty());
}

#[test]
fn tag_show_blame_commands() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    std::fs::write(d.join("a.txt"), "one\ntwo\n").unwrap();
    ok(&d, &["add", "."]);
    ok(&d, &["commit", "-m", "first"]);
    std::fs::write(d.join("a.txt"), "one\n2\n").unwrap();
    ok(&d, &["add", "."]);
    ok(&d, &["commit", "-m", "second"]);

    assert!(ok(&d, &["tag"]).contains("no tags"));
    ok(&d, &["tag", "light", "HEAD~1"]);
    ok(&d, &["tag", "v1", "-m", "release one", "--signature", "sig"]);
    let list = ok(&d, &["tag"]);
    assert!(list.contains("light") && list.contains("annotated: release one"), "{list}");
    let dup = vcs(&d, &["tag", "v1"]);
    assert!(!dup.status.success() && String::from_utf8_lossy(&dup.stderr).contains("already exists"));

    let shown = ok(&d, &["show", "v1"]);
    assert!(
        shown.contains("release one") && shown.contains("second") && shown.contains("+2"),
        "{shown}"
    );
    assert!(ok(&d, &["show"]).contains("-two"));
    assert!(!vcs(&d, &["show", "nope"]).status.success());

    let blame = ok(&d, &["blame", "a.txt"]);
    let rows: Vec<&str> = blame.lines().collect();
    assert_eq!(rows.len(), 2, "{blame}");
    assert!(rows[0].ends_with("one") && rows[1].ends_with("2"));
    assert_ne!(rows[0].split(' ').next(), rows[1].split(' ').next());
    assert!(!vcs(&d, &["blame", "nope.txt"]).status.success());

    ok(&d, &["tag", "-d", "light"]);
    assert!(!vcs(&d, &["tag", "-d", "light"]).status.success());
    assert!(ok(&d, &["op", "log"]).contains("tag delete root/light"));
    assert!(ok(&d, &["fsck"]).contains("ok"));
}

#[test]
fn chunked_large_files_work_with_restore_show_blame_and_merge() {
    // A pseudo-random 3 MiB file is stored as chunks plus a manifest; every read path must reassemble it.
    let big: Vec<u8> = {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        (0..3 * 1024 * 1024)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    };
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "demo"]);
    let d = t.path().join("demo");
    std::fs::write(d.join("big.bin"), &big).unwrap();
    ok(&d, &["add", "big.bin"]);
    ok(&d, &["commit", "-m", "big"]);
    std::fs::write(d.join("big.bin"), b"scribble").unwrap();
    ok(&d, &["restore", "big.bin"]);
    assert_eq!(std::fs::read(d.join("big.bin")).unwrap(), big);
    ok(&d, &["show"]);
    let blame = vcs(&d, &["blame", "big.bin"]);
    assert!(
        String::from_utf8_lossy(&blame.stderr).contains("binary"),
        "blame must refuse binary cleanly, not report corruption"
    );
    ok(&d, &["branch", "side"]);
    ok(&d, &["switch", "side"]);
    let mut changed = big.clone();
    changed[0] ^= 0xff;
    std::fs::write(d.join("big.bin"), &changed).unwrap();
    ok(&d, &["add", "big.bin"]);
    ok(&d, &["commit", "-m", "change"]);
    ok(&d, &["show"]);
    ok(&d, &["switch", "main"]);
    ok(&d, &["merge", "side"]);
    assert_eq!(std::fs::read(d.join("big.bin")).unwrap(), changed);
    assert!(ok(&d, &["fsck"]).contains("ok"));
}
