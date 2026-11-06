//! `vcs rerere`: record a conflict resolution, replay it for merge, rebase and cherry-pick
//! (docs/rerere.md). The last test runs the same story with real git (when a `git` binary is on
//! the PATH) and compares what the user sees. vcs itself never calls git.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn vcs(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env_remove("VCS_RERERE_ENABLED")
        .output()
        .unwrap()
}
fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}
fn ok(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(o.status.success(), "vcs {args:?} failed: {}", text(&o));
    text(&o)
}
fn write(dir: &Path, f: &str, c: &str) {
    std::fs::write(dir.join(f), c).unwrap();
}
fn read(dir: &Path, f: &str) -> String {
    std::fs::read_to_string(dir.join(f)).unwrap()
}
fn commit_all(dir: &Path, msg: &str) {
    ok(dir, &["add", "."]);
    ok(dir, &["commit", "-m", msg]);
}

const BASE: &str = "one\ntwo\nthree\nfour\nfive\n";
const MAIN: &str = "one\ntwo\nthree-main\nfour\nfive\n";
const TOPIC: &str = "one\ntwo\nthree-topic\nfour\nfive\n";
const BOTH: &str = "one\ntwo\nthree-both\nfour\nfive\n";

/// main and topic both rewrite line 3 of f.txt; HEAD is on main.
fn repo(t: &Path, rerere: bool) -> PathBuf {
    let d = t.join("r");
    ok(t, &["init", "r"]);
    if rerere {
        ok(&d, &["config", "set", "rerere.enabled", "on"]);
    }
    write(&d, "f.txt", BASE);
    commit_all(&d, "base");
    ok(&d, &["switch", "-c", "topic"]);
    write(&d, "f.txt", TOPIC);
    commit_all(&d, "topic edit");
    ok(&d, &["switch", "main"]);
    write(&d, "f.txt", MAIN);
    commit_all(&d, "main edit");
    d
}

fn rr_dir(d: &Path) -> PathBuf {
    d.join(".vcs").join("rr-cache")
}
fn entries(d: &Path, name: &str) -> usize {
    std::fs::read_dir(rr_dir(d))
        .map(|rd| rd.flatten().filter(|e| e.path().join(name).exists()).count())
        .unwrap_or(0)
}

#[test]
fn off_by_default_records_nothing() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path(), false);
    assert!(!vcs(&d, &["merge", "topic"]).status.success());
    assert!(read(&d, "f.txt").contains("<<<<<<<"));
    assert!(!rr_dir(&d).exists());
    assert_eq!(ok(&d, &["rerere", "status"]), "");
}

#[test]
fn merge_records_then_replays_a_resolution() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path(), true);
    assert!(!vcs(&d, &["merge", "topic"]).status.success());
    assert!(read(&d, "f.txt").contains("<<<<<<<"));
    assert_eq!((entries(&d, "preimage"), entries(&d, "postimage")), (1, 0));
    assert_eq!(ok(&d, &["rerere", "status"]), "f.txt\n");
    assert_eq!(ok(&d, &["rerere", "remaining"]), "f.txt\n");
    // the diff shows nothing before the edit, the edit after
    assert_eq!(ok(&d, &["rerere", "diff"]), "");
    write(&d, "f.txt", BOTH);
    let diff = ok(&d, &["rerere", "diff"]);
    assert!(
        diff.contains("+three-both") && diff.contains("-three-main") && diff.contains("-<<<<<<<"),
        "{diff}"
    );
    let o = ok(&d, &["add", "f.txt"]);
    assert!(o.contains("Recorded resolution for 'f.txt'"), "{o}");
    assert_eq!((entries(&d, "preimage"), entries(&d, "postimage")), (1, 1));
    ok(&d, &["commit", "-m", "merge topic"]);
    assert_eq!(ok(&d, &["rerere", "status"]), "", "tracking ends with the merge");

    // same conflict again
    ok(&d, &["reset", "--hard", "HEAD^"]);
    assert_eq!(read(&d, "f.txt"), MAIN);
    let o = vcs(&d, &["merge", "topic"]);
    assert!(!o.status.success(), "a replayed file still needs `add`, as in git");
    assert!(text(&o).contains("Resolved 'f.txt' using previous resolution."), "{}", text(&o));
    assert_eq!(read(&d, "f.txt"), BOTH);
    assert_eq!(ok(&d, &["rerere", "status"]), "", "replayed paths are no longer tracked, as in git");
    assert_eq!(ok(&d, &["rerere", "remaining"]), "", "replayed: nothing left to resolve by hand");
    assert!(vcs(&d, &["commit", "-m", "x"]).status.code() != Some(0), "not staged yet");
    ok(&d, &["add", "f.txt"]);
    ok(&d, &["commit", "-m", "merge topic again"]);
    assert_eq!(read(&d, "f.txt"), BOTH);
}

#[test]
fn the_other_direction_reuses_the_same_resolution() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path(), true);
    ok(&d, &["branch", "main0"]);
    vcs(&d, &["merge", "topic"]);
    write(&d, "f.txt", BOTH);
    ok(&d, &["add", "f.txt"]);
    ok(&d, &["commit", "-m", "m"]);
    ok(&d, &["switch", "topic"]);
    // ours/theirs swapped: the conflict has the same two sides
    let o = vcs(&d, &["merge", "main0"]);
    assert!(text(&o).contains("Resolved 'f.txt' using previous resolution."), "{}", text(&o));
    assert_eq!(read(&d, "f.txt"), BOTH);
    assert_eq!(entries(&d, "preimage"), 1, "one conflict, one cache entry");
}

#[test]
fn context_changes_around_the_conflict_carry_over() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path(), true);
    vcs(&d, &["merge", "topic"]);
    write(&d, "f.txt", BOTH);
    ok(&d, &["add", "f.txt"]);
    ok(&d, &["commit", "-m", "m"]);
    ok(&d, &["reset", "--hard", "HEAD^"]);
    // a new line at the top of both sides, away from the conflict
    ok(&d, &["switch", "topic"]);
    write(&d, "f.txt", &format!("zero\n{TOPIC}"));
    commit_all(&d, "topic top");
    ok(&d, &["switch", "main"]);
    write(&d, "f.txt", &format!("zero\n{MAIN}"));
    commit_all(&d, "main top");
    vcs(&d, &["merge", "topic"]);
    assert_eq!(read(&d, "f.txt"), format!("zero\n{BOTH}"));
}

#[test]
fn rebase_replays_the_resolution() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path(), true);
    ok(&d, &["switch", "topic"]);
    ok(&d, &["branch", "topic2"]);
    let o = vcs(&d, &["rebase", "main"]);
    assert!(!o.status.success());
    assert_eq!(ok(&d, &["rerere", "status"]), "f.txt\n");
    write(&d, "f.txt", BOTH);
    ok(&d, &["add", "f.txt"]);
    ok(&d, &["rebase", "--continue"]);
    assert_eq!(read(&d, "f.txt"), BOTH);
    assert_eq!(entries(&d, "postimage"), 1);

    ok(&d, &["switch", "topic2"]);
    let o = vcs(&d, &["rebase", "main"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("Resolved 'f.txt' using previous resolution."), "{}", text(&o));
    assert_eq!(read(&d, "f.txt"), BOTH);
    ok(&d, &["add", "f.txt"]);
    ok(&d, &["rebase", "--continue"]);
    assert_eq!(read(&d, "f.txt"), BOTH);
}

#[test]
fn cherry_pick_replays_the_resolution() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path(), true);
    ok(&d, &["branch", "side"]);
    let o = vcs(&d, &["cherry-pick", "topic"]);
    assert!(!o.status.success());
    write(&d, "f.txt", BOTH);
    ok(&d, &["add", "f.txt"]);
    ok(&d, &["commit", "-m", "picked"]);
    ok(&d, &["switch", "side"]);
    let o = vcs(&d, &["cherry-pick", "topic"]);
    assert!(text(&o).contains("Resolved 'f.txt' using previous resolution."), "{}", text(&o));
    assert_eq!(read(&d, "f.txt"), BOTH);
}

#[test]
fn auto_update_stages_the_replayed_file() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path(), true);
    vcs(&d, &["merge", "topic"]);
    write(&d, "f.txt", BOTH);
    ok(&d, &["add", "f.txt"]);
    ok(&d, &["commit", "-m", "m"]);
    ok(&d, &["reset", "--hard", "HEAD^"]);
    ok(&d, &["config", "set", "rerere.autoUpdate", "on"]);
    vcs(&d, &["merge", "topic"]);
    assert_eq!(read(&d, "f.txt"), BOTH);
    ok(&d, &["commit", "-m", "no add needed"]);
    assert_eq!(read(&d, "f.txt"), BOTH);
}

#[test]
fn forget_clear_and_gc() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path(), true);
    vcs(&d, &["merge", "topic"]);
    write(&d, "f.txt", BOTH);
    ok(&d, &["add", "f.txt"]);
    // forget works on the conflict in progress, even after `add`
    let o = ok(&d, &["rerere", "forget", "f.txt"]);
    assert!(o.contains("Forgot resolution for 'f.txt'"), "{o}");
    assert_eq!((entries(&d, "preimage"), entries(&d, "postimage")), (1, 0));
    let o = vcs(&d, &["rerere", "forget", "f.txt"]);
    assert!(!o.status.success() && text(&o).contains("no remembered resolution"), "{}", text(&o));
    ok(&d, &["commit", "-m", "m"]);
    // the next identical conflict is not replayed
    ok(&d, &["reset", "--hard", "HEAD^"]);
    let o = vcs(&d, &["merge", "topic"]);
    assert!(!text(&o).contains("Resolved"), "{}", text(&o));
    assert!(read(&d, "f.txt").contains("<<<<<<<"));
    // `rerere` with no argument records a hand edit without `add`
    write(&d, "f.txt", BOTH);
    assert!(ok(&d, &["rerere"]).contains("Recorded resolution for 'f.txt'"));
    assert_eq!(entries(&d, "postimage"), 1);
    ok(&d, &["merge", "--abort"]);

    // clear drops only what has no resolution; an unresolved conflict disappears
    ok(&d, &["reset", "--hard", "main"]);
    ok(&d, &["branch", "t3", "topic"]);
    ok(&d, &["switch", "t3"]);
    write(&d, "g.txt", "x\n");
    commit_all(&d, "g");
    ok(&d, &["switch", "main"]);
    write(&d, "g.txt", "y\n");
    commit_all(&d, "g main");
    vcs(&d, &["merge", "t3"]);
    assert_eq!(entries(&d, "preimage"), 2);
    ok(&d, &["rerere", "clear"]);
    assert_eq!(entries(&d, "preimage"), 1);
    assert_eq!(ok(&d, &["rerere", "status"]), "");
    ok(&d, &["merge", "--abort"]);

    // gc keeps fresh entries and removes old ones (age by mtime)
    let o = ok(&d, &["rerere", "gc"]);
    assert!(o.contains("removed 0 resolution(s) and 0 unresolved"), "{o}");
    let post = std::fs::read_dir(rr_dir(&d))
        .unwrap()
        .flatten()
        .map(|e| e.path().join("postimage"))
        .find(|p| p.exists())
        .unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(61 * 86_400);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&post)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let o = ok(&d, &["rerere", "gc"]);
    assert!(o.contains("removed 1 resolution(s)"), "{o}");
    assert_eq!(entries(&d, "preimage"), 0);
    let o = vcs(&d, &["config", "set", "gc.rerereResolved", "soon"]);
    assert!(!o.status.success());
}

#[test]
fn binary_and_non_text_conflicts_are_left_alone() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("r");
    ok(t.path(), &["init", "r"]);
    ok(&d, &["config", "set", "rerere.enabled", "on"]);
    std::fs::write(d.join("b.bin"), [0u8, 1, 2]).unwrap();
    commit_all(&d, "base");
    ok(&d, &["switch", "-c", "t"]);
    std::fs::write(d.join("b.bin"), [0u8, 9, 9]).unwrap();
    commit_all(&d, "t");
    ok(&d, &["switch", "main"]);
    std::fs::write(d.join("b.bin"), [0u8, 7, 7]).unwrap();
    commit_all(&d, "m");
    assert!(!vcs(&d, &["merge", "t"]).status.success());
    assert_eq!(ok(&d, &["rerere", "status"]), "");
    assert_eq!(ok(&d, &["rerere", "remaining"]), "b.bin\n");
}

// ---------- twin test against real git ----------

fn have_git() -> bool {
    Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
}

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Dev")
        .env("GIT_AUTHOR_EMAIL", "dev@example.com")
        .env("GIT_COMMITTER_NAME", "Dev")
        .env("GIT_COMMITTER_EMAIL", "dev@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_EDITOR", "true")
        .output()
        .unwrap()
}
fn gok(dir: &Path, args: &[&str]) -> String {
    let o = git(dir, args);
    assert!(o.status.success(), "git {args:?} failed: {}", text(&o));
    text(&o)
}

/// What the user can observe after a step, for both tools.
#[derive(Debug, PartialEq)]
struct View {
    file: Option<String>, // contents when the file has no markers, else None
    markers: bool,
    status: Vec<String>,
    remaining: Vec<String>,
    replayed: bool,
}

fn view_vcs(d: &Path, o: &Output) -> View {
    let f = read(d, "f.txt");
    let markers = f.contains("<<<<<<<");
    View {
        file: (!markers).then_some(f),
        markers,
        status: ok(d, &["rerere", "status"]).lines().map(str::to_owned).collect(),
        remaining: ok(d, &["rerere", "remaining"]).lines().map(str::to_owned).collect(),
        replayed: text(o).contains("Resolved 'f.txt' using previous resolution."),
    }
}
fn view_git(d: &Path, o: &Output) -> View {
    let f = read(d, "f.txt");
    let markers = f.contains("<<<<<<<");
    View {
        file: (!markers).then_some(f),
        markers,
        status: gok(d, &["rerere", "status"]).lines().map(str::to_owned).collect(),
        remaining: gok(d, &["rerere", "remaining"]).lines().map(str::to_owned).collect(),
        replayed: text(o).contains("Resolved 'f.txt' using previous resolution."),
    }
}

fn git_repo(t: &Path) -> PathBuf {
    let d = t.join("g");
    std::fs::create_dir_all(&d).unwrap();
    gok(&d, &["init", "-q", "-b", "main"]);
    gok(&d, &["config", "rerere.enabled", "true"]);
    write(&d, "f.txt", BASE);
    gok(&d, &["add", "."]);
    gok(&d, &["commit", "-qm", "base"]);
    gok(&d, &["switch", "-qc", "topic"]);
    write(&d, "f.txt", TOPIC);
    gok(&d, &["commit", "-qam", "topic edit"]);
    gok(&d, &["switch", "-q", "main"]);
    write(&d, "f.txt", MAIN);
    gok(&d, &["commit", "-qam", "main edit"]);
    d
}

#[test]
fn twin_git_merge_rebase_pick_forget() {
    if !have_git() {
        eprintln!("git not on PATH: twin test skipped");
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let v = repo(t.path(), true);
    let g = git_repo(t.path());
    let mut step = 0;
    let mut same = |name: &str, a: View, b: View| {
        step += 1;
        assert_eq!(a, b, "step {step} ({name}): vcs (left) differs from git (right)");
    };

    // 1. first conflict: markers, tracked, nothing replayed
    let (ov, og) = (vcs(&v, &["merge", "topic"]), git(&g, &["merge", "topic"]));
    same("first merge", view_vcs(&v, &ov), view_git(&g, &og));
    // 2. resolve + commit, no replay since it is the first time
    write(&v, "f.txt", BOTH);
    write(&g, "f.txt", BOTH);
    ok(&v, &["add", "f.txt"]);
    gok(&g, &["add", "f.txt"]);
    ok(&v, &["commit", "-m", "m"]);
    gok(&g, &["commit", "-qm", "m"]);
    // 3. same merge again after undoing it: replayed, still unmerged until add
    ok(&v, &["reset", "--hard", "HEAD^"]);
    gok(&g, &["reset", "-q", "--hard", "HEAD^"]);
    let (ov, og) = (vcs(&v, &["merge", "topic"]), git(&g, &["merge", "topic"]));
    same("second merge", view_vcs(&v, &ov), view_git(&g, &og));
    write(&v, "f.txt", BOTH);
    ok(&v, &["add", "f.txt"]);
    gok(&g, &["add", "f.txt"]);
    ok(&v, &["commit", "-m", "m2"]);
    gok(&g, &["commit", "-qm", "m2"]);
    // 4. swapped sides: main is back at its own edit, topic merges main
    ok(&v, &["reset", "--hard", "HEAD^"]);
    gok(&g, &["reset", "-q", "--hard", "HEAD^"]);
    ok(&v, &["switch", "topic"]);
    gok(&g, &["switch", "-q", "topic"]);
    let (ov, og) = (vcs(&v, &["merge", "main"]), git(&g, &["merge", "main"]));
    same("swapped merge", view_vcs(&v, &ov), view_git(&g, &og));
    ok(&v, &["merge", "--abort"]);
    gok(&g, &["merge", "--abort"]);
    // 5. rebase replays it too
    ok(&v, &["branch", "topic0"]);
    gok(&g, &["branch", "topic0"]);
    let (ov, og) = (vcs(&v, &["rebase", "main"]), git(&g, &["rebase", "main"]));
    same("rebase", view_vcs(&v, &ov), view_git(&g, &og));
    ok(&v, &["add", "f.txt"]);
    gok(&g, &["add", "f.txt"]);
    ok(&v, &["rebase", "--continue"]);
    gok(&g, &["-c", "core.editor=true", "rebase", "--continue"]);
    assert_eq!(read(&v, "f.txt"), read(&g, "f.txt"));
    // 6. forget: afterwards the same conflict is not replayed any more
    ok(&v, &["switch", "main"]);
    gok(&g, &["switch", "-q", "main"]);
    let (ov, og) = (vcs(&v, &["merge", "topic0"]), git(&g, &["merge", "topic0"]));
    same("merge before forget", view_vcs(&v, &ov), view_git(&g, &og));
    ok(&v, &["rerere", "forget", "f.txt"]);
    gok(&g, &["rerere", "forget", "f.txt"]);
    ok(&v, &["merge", "--abort"]);
    gok(&g, &["merge", "--abort"]);
    let (ov, og) = (vcs(&v, &["merge", "topic0"]), git(&g, &["merge", "topic0"]));
    same("merge after forget", view_vcs(&v, &ov), view_git(&g, &og));
}
