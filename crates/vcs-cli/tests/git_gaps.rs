//! Native versions of the remaining git gaps: `cherry-pick A..B`, `cherry`, `rebase --onto` and
//! `--root`, `apply -3`, `am -3`, `publish --tags|--prune|--mirror`. vcs never calls git here.
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
    assert!(
        o.status.success(),
        "vcs {args:?} failed: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn fail(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(!o.status.success(), "vcs {args:?} unexpectedly succeeded");
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}
fn write(dir: &Path, f: &str, c: &str) {
    std::fs::write(dir.join(f), c).unwrap();
}
fn read(dir: &Path, f: &str) -> String {
    std::fs::read_to_string(dir.join(f)).unwrap()
}
fn commit_file(dir: &Path, f: &str, c: &str, msg: &str) {
    write(dir, f, c);
    ok(dir, &["add", "."]);
    ok(dir, &["commit", "-m", msg]);
}
fn subjects(dir: &Path) -> Vec<String> {
    ok(dir, &["log", "--oneline", "-n", "50"])
        .lines()
        .map(|l| l.split_once(' ').map(|x| x.1).unwrap_or("").to_owned())
        .collect()
}

/// main: base; feat: c1 c2 c3 (each adds a file).
fn feat_repo(t: &Path) -> std::path::PathBuf {
    let d = t.join("r");
    ok(t, &["init", "r"]);
    commit_file(&d, "base.txt", "base\n", "base");
    ok(&d, &["switch", "-c", "feat"]);
    for n in 1..=3 {
        commit_file(&d, &format!("c{n}.txt"), &format!("c{n}\n"), &format!("c{n}"));
    }
    ok(&d, &["switch", "main"]);
    d
}

#[test]
fn cherry_pick_range_and_cherry() {
    let t = tempfile::tempdir().unwrap();
    let d = feat_repo(t.path());
    // c1 and c2 only
    let out = ok(&d, &["cherry-pick", "feat~3..feat~1"]);
    assert_eq!(out.matches("cherry-picked").count(), 2, "{out}");
    assert!(d.join("c1.txt").exists() && d.join("c2.txt").exists() && !d.join("c3.txt").exists());
    assert_eq!(&subjects(&d)[..3], ["c2", "c1", "base"]);
    // cherry: c1, c2 have equivalents on main, c3 does not
    let out = ok(&d, &["cherry", "main", "feat", "-v"]);
    let l: Vec<&str> = out.lines().collect();
    assert_eq!(l.len(), 3, "{out}");
    assert!(l[0].starts_with("- ") && l[0].ends_with("c1"), "{out}");
    assert!(l[1].starts_with("- ") && l[1].ends_with("c2"), "{out}");
    assert!(l[2].starts_with("+ ") && l[2].ends_with("c3"), "{out}");
    // the whole range again: c1 and c2 change nothing and are skipped, c3 is picked
    let out = ok(&d, &["cherry-pick", "feat~3..feat"]);
    assert!(
        out.matches("skipped").count() == 2 && out.matches("cherry-picked").count() == 1,
        "{out}"
    );
    assert_eq!(&subjects(&d)[..2], ["c3", "c2"]);
    assert!(ok(&d, &["cherry", "main", "feat"]).lines().all(|l| l.starts_with("- ")));
}

#[test]
fn cherry_pick_range_stops_on_conflict_and_names_the_rest() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("r");
    ok(t.path(), &["init", "r"]);
    commit_file(&d, "f", "1\n2\n3\n", "base");
    ok(&d, &["switch", "-c", "feat"]);
    commit_file(&d, "f", "1\nFEAT\n3\n", "feat edit");
    commit_file(&d, "g", "g\n", "feat g");
    ok(&d, &["switch", "main"]);
    commit_file(&d, "f", "1\nMAIN\n3\n", "main edit");
    let o = vcs(&d, &["cherry-pick", "main..feat"]);
    assert!(!o.status.success());
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("conflict: f") && out.contains("Then pick the rest: vcs cherry-pick"),
        "{out}"
    );
    write(&d, "f", "1\nBOTH\n3\n");
    ok(&d, &["add", "f"]);
    ok(&d, &["commit", "-m", "resolved"]);
    let rest = out
        .split("vcs cherry-pick ")
        .nth(1)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .trim()
        .to_owned();
    ok(&d, &["cherry-pick", &rest]);
    assert_eq!(read(&d, "g"), "g\n");
    // a merge commit in the range is refused up front
    assert!(fail(&d, &["cherry-pick", "nosuchrev..feat"]).contains("nosuchrev"));
}

#[test]
fn rebase_onto_and_root() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("r");
    ok(t.path(), &["init", "r"]);
    commit_file(&d, "base.txt", "base\n", "base");
    ok(&d, &["switch", "-c", "a"]);
    commit_file(&d, "a1.txt", "a1\n", "a1");
    ok(&d, &["switch", "-c", "b"]);
    commit_file(&d, "b1.txt", "b1\n", "b1");
    commit_file(&d, "b2.txt", "b2\n", "b2");
    ok(&d, &["switch", "main"]);
    commit_file(&d, "m1.txt", "m1\n", "m1");
    ok(&d, &["switch", "b"]);
    // b = base a1 b1 b2; move b1 b2 (the commits `a` lacks) onto main, dropping a1
    ok(&d, &["rebase", "--onto", "main", "a"]);
    assert_eq!(subjects(&d), ["b2", "b1", "m1", "base"]);
    assert!(!d.join("a1.txt").exists() && d.join("m1.txt").exists() && d.join("b2.txt").exists());
    ok(&d, &["op", "undo"]);
    assert_eq!(subjects(&d), ["b2", "b1", "a1", "base"]);
    assert!(d.join("a1.txt").exists() && !d.join("m1.txt").exists());

    // --root: replay the whole branch (root commit included) onto another commit
    ok(&d, &["switch", "main"]);
    ok(&d, &["switch", "-c", "other"]);
    commit_file(&d, "o.txt", "o\n", "o1");
    ok(&d, &["switch", "a"]);
    ok(&d, &["rebase", "--onto", "other", "--root"]);
    assert_eq!(subjects(&d), ["a1", "base", "o1", "m1", "base"]);
    assert!(d.join("o.txt").exists() && d.join("a1.txt").exists() && d.join("m1.txt").exists());
    // argument errors
    assert!(fail(&d, &["rebase", "--onto", "main"]).contains("usage"));
    assert!(fail(&d, &["rebase", "--onto", "main", "a", "--root"]).contains("usage"));
}

const TEN: &str = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";

fn patch_repo(t: &Path, name: &str, file: &str) -> std::path::PathBuf {
    let d = t.join(name);
    ok(t, &["init", name]);
    commit_file(&d, "f", TEN, "base");
    if file != TEN {
        commit_file(&d, "f", file, "local edit");
    }
    d
}

#[test]
fn apply_three_way_merges_drifted_context_and_marks_overlaps() {
    let t = tempfile::tempdir().unwrap();
    let src = patch_repo(t.path(), "src", TEN);
    commit_file(&src, "f", "1\n2\n3\n4\n5\nSIX\n7\n8\n9\n10\n", "six");
    let patch = ok(&src, &["show", "HEAD"]);
    let diff = &patch[patch.find("diff --").unwrap()..];
    let pf = t.path().join("six.patch");
    std::fs::write(&pf, diff).unwrap();

    // line 3 changed locally: the hunk's context no longer matches, plain apply refuses
    let local = "1\n2\nTHREE\n4\n5\n6\n7\n8\n9\n10\n";
    let d = patch_repo(t.path(), "d1", local);
    assert!(fail(&d, &["apply", pf.to_str().unwrap()]).contains("does not apply"));
    assert_eq!(read(&d, "f"), local, "a refused apply changes nothing");
    ok(&d, &["apply", "-3", pf.to_str().unwrap()]);
    assert_eq!(read(&d, "f"), "1\n2\nTHREE\n4\n5\nSIX\n7\n8\n9\n10\n");

    // the same line changed differently: markers, nonzero exit
    let d2 = patch_repo(t.path(), "d2", "1\n2\n3\n4\n5\nLOCAL\n7\n8\n9\n10\n");
    let e = fail(&d2, &["apply", "--3way", pf.to_str().unwrap()]);
    assert!(e.contains("conflict") && e.contains("f"), "{e}");
    let f = read(&d2, "f");
    assert!(
        f.contains("        "{f}"
    );

    // already applied: a no-op, not a conflict
    let d3 = patch_repo(t.path(), "d3", "1\n2\n3\n4\n5\nSIX\n7\n8\n9\n10\n");
    ok(&d3, &["apply", "-3", pf.to_str().unwrap()]);
    assert_eq!(read(&d3, "f"), "1\n2\n3\n4\n5\nSIX\n7\n8\n9\n10\n");

    // an unrelated file is not placed at all
    let d4 = t.path().join("d4");
    ok(t.path(), &["init", "d4"]);
    commit_file(&d4, "f", "x\ny\nz\n", "other");
    assert!(fail(&d4, &["apply", "-3", pf.to_str().unwrap()]).contains("cannot be placed"));
}

#[test]
fn am_three_way_commits_clean_merges_and_stops_on_overlap() {
    let t = tempfile::tempdir().unwrap();
    let src = patch_repo(t.path(), "src", TEN);
    commit_file(&src, "f", "1\n2\n3\n4\n5\nSIX\n7\n8\n9\n10\n", "six it is");
    let out = t.path().join("out");
    ok(&src, &["format-patch", "-n", "1", "-o", out.to_str().unwrap()]);
    let mbox = std::fs::read_dir(&out).unwrap().next().unwrap().unwrap().path();
    let mbox = mbox.to_str().unwrap();

    let d = patch_repo(t.path(), "d1", "1\n2\nTHREE\n4\n5\n6\n7\n8\n9\n10\n");
    assert!(fail(&d, &["am", mbox]).contains("did not apply"));
    ok(&d, &["am", "--abort"]);
    ok(&d, &["am", "-3", mbox]);
    assert_eq!(read(&d, "f"), "1\n2\nTHREE\n4\n5\nSIX\n7\n8\n9\n10\n");
    assert_eq!(subjects(&d)[0], "six it is");

    let d2 = patch_repo(t.path(), "d2", "1\n2\n3\n4\n5\nLOCAL\n7\n8\n9\n10\n");
    let e = fail(&d2, &["am", "-3", mbox]);
    assert!(e.contains("conflicts") && e.contains("--continue"), "{e}");
    assert!(read(&d2, "f").contains("<<<<<<<"));
    // continuing before resolving is refused (markers are unstaged)
    assert!(fail(&d2, &["am", "--continue"]).contains("unstaged"));
    write(&d2, "f", "1\n2\n3\n4\n5\nBOTH\n7\n8\n9\n10\n");
    ok(&d2, &["add", "f"]);
    ok(&d2, &["am", "--continue"]);
    assert_eq!(subjects(&d2)[0], "six it is");
    assert_eq!(read(&d2, "f"), "1\n2\n3\n4\n5\nBOTH\n7\n8\n9\n10\n");
}

fn remote_pair(t: &Path) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let remote = t.join("remote");
    std::fs::create_dir_all(&remote).unwrap();
    let url = remote.to_str().unwrap().to_owned();
    let a = t.join("a");
    ok(t, &["init", "a"]);
    commit_file(&a, "f.txt", "one\n", "init");
    ok(&a, &["remote", "set", &url]);
    ok(&a, &["publish"]);
    ok(t, &["clone", &url, "b"]);
    (a, t.join("b"), url)
}

#[test]
fn publish_tags_only() {
    let t = tempfile::tempdir().unwrap();
    let (a, b, _) = remote_pair(t.path());
    commit_file(&a, "f.txt", "one\ntwo\n", "second");
    ok(&a, &["tag", "v2"]);
    let out = ok(&a, &["publish", "--tags"]);
    assert!(out.contains("tag root/v2"), "{out}");
    ok(&b, &["fetch"]);
    assert!(ok(&b, &["tag"]).contains("v2"));
    // the branch did not move: b's remote-tracking head is still the first commit
    assert_eq!(ok(&b, &["log", "--oneline"]).lines().count(), 1);
    // a plain publish then moves the branch
    ok(&a, &["publish"]);
    ok(&b, &["sync"]);
    assert_eq!(ok(&b, &["log", "--oneline"]).lines().count(), 2);
}

#[test]
fn publish_prune_deletes_leased_and_audited() {
    let t = tempfile::tempdir().unwrap();
    let (a, b, _) = remote_pair(t.path());
    ok(&a, &["switch", "-c", "topic"]);
    commit_file(&a, "t.txt", "t\n", "topic");
    ok(&a, &["publish"]);
    ok(&a, &["switch", "main"]);
    // topic is still local: nothing to prune
    let out = ok(&a, &["publish", "--prune"]);
    assert!(!out.contains("deleted remote branch"), "{out}");
    ok(&a, &["branch", "-d", "topic", "--force"]);
    // b never fetched topic: a prune from b would delete a head it has not seen
    ok(&b, &["fetch"]);
    let out = ok(&a, &["publish", "--prune"]);
    assert!(
        out.contains("deleted remote branch root/topic") && out.contains("to restore"),
        "{out}"
    );
    assert!(ok(&a, &["op", "log"]).contains("publish --delete root/topic"));
    // a prune never deletes a head this clone has not fetched: b branches nothing locally, never saw topic2
    ok(&a, &["switch", "-c", "topic2"]);
    commit_file(&a, "u.txt", "u\n", "topic2");
    ok(&a, &["publish"]);
    ok(&a, &["switch", "main"]);
    ok(&a, &["branch", "-d", "topic2", "--force"]);
    let _ = &b;
    assert!(ok(&a, &["publish", "--prune"]).contains("deleted remote branch root/topic2"));
    assert!(!vcs(&a, &["publish", "--prune", "--delete", "x", "--tags"]).status.success());
}

#[test]
fn publish_mirror_forces_with_lease_prunes_and_sends_tags() {
    let t = tempfile::tempdir().unwrap();
    let (a, b, _) = remote_pair(t.path());
    // b has an extra branch the mirror source lacks; a moves on and publishes
    ok(&b, &["switch", "-c", "extra"]);
    commit_file(&b, "x.txt", "x\n", "extra");
    ok(&b, &["publish"]);
    ok(&b, &["switch", "main"]);
    commit_file(&a, "f.txt", "one\nalice\n", "alice");
    ok(&a, &["publish"]);
    // b diverges
    commit_file(&b, "f.txt", "one\nbob\n", "bob");
    ok(&b, &["tag", "bob-tag"]);
    assert!(fail(&b, &["publish"]).contains("Cannot publish"));
    // mirror is leased: b has not fetched alice's head
    let e = fail(&b, &["publish", "--mirror"]);
    assert!(e.contains("stale info") && e.contains("Nothing was changed"), "{e}");
    ok(&b, &["fetch"]);
    let out = ok(&b, &["publish", "--mirror"]);
    assert!(out.contains("FORCED") && out.contains("tag root/bob-tag"), "{out}");
    assert!(!out.contains("deleted remote branch root/extra"), "extra exists locally: {out}");
    ok(&b, &["branch", "-d", "extra", "--force"]);
    let out = ok(&b, &["publish", "--mirror"]);
    assert!(out.contains("deleted remote branch root/extra"), "{out}");
    let oplog = ok(&b, &["op", "log"]);
    assert!(
        oplog.contains("publish --force root/main") && oplog.contains("publish --delete root/extra"),
        "{oplog}"
    );
    // --mirror and --tags/--delete do not mix
    assert!(!vcs(&b, &["publish", "--mirror", "--tags"]).status.success());
}

<<<<<<< HEAD
#[test]
fn symmetric_difference_in_cherry_pick_and_format_patch() {
    let t = tempfile::tempdir().unwrap();
    let d = feat_repo(t.path());
    ok(&d, &["switch", "-c", "target"]);
    ok(&d, &["switch", "main"]);
    commit_file(&d, "m1.txt", "m1\n", "m1");
    ok(&d, &["switch", "target"]);
    // main...feat: m1 (only on main) and c1..c3 (only on feat); base is common and left out
    ok(&d, &["cherry-pick", "main...feat"]);
    for f in ["m1.txt", "c1.txt", "c2.txt", "c3.txt"] {
        assert_eq!(read(&d, f), format!("{}\n", f.trim_end_matches(".txt")), "{f}");
    }
    let subs = subjects(&d);
    assert!(subs.contains(&"m1".to_owned()) && subs.contains(&"c3".to_owned()), "{subs:?}");
    assert_eq!(subs.iter().filter(|s| s.as_str() == "base").count(), 1);

    let mbox = ok(&d, &["format-patch", "--stdout", "main...feat"]);
    for s in ["m1", "c1", "c2", "c3"] {
        assert!(
            mbox.contains("Subject: [PATCH") && mbox.contains(&format!("] {s}\n")),
            "{s} missing:\n{mbox}"
        );
    }
    assert!(!mbox.contains("] base\n"), "{mbox}");
    // -n keeps only the newest N of the set
    let one = ok(&d, &["format-patch", "--stdout", "-n", "1", "main...feat"]);
    assert_eq!(
        one.matches("\nSubject: ").count() + usize::from(one.starts_with("Subject: ")),
        1,
        "{one}"
    );
    // a symmetric range with a missing side is an error naming it
    assert!(fail(&d, &["cherry-pick", "main...nosuchbranch"]).contains("nosuchbranch"));
}

/// main: base. v1: p1 p2 p3. v2: p1 (same), p2 (edited), p4 (new, big); p3 dropped.
fn rd_repo(t: &Path) -> std::path::PathBuf {
    let d = t.join("r");
    ok(t, &["init", "r"]);
    commit_file(&d, "base.txt", "base\n", "base");
    ok(&d, &["switch", "-c", "v1"]);
    commit_file(&d, "a.txt", "a\n", "p1");
    commit_file(&d, "b.txt", "b\n", "p2");
    commit_file(&d, "c.txt", "c\n", "p3");
    ok(&d, &["switch", "main"]);
    ok(&d, &["switch", "-c", "v2"]);
    commit_file(&d, "a.txt", "a\n", "p1");
    commit_file(&d, "b.txt", "b changed\n", "p2");
    let big: String = (0..30).map(|i| format!("line {i}\n")).collect();
    commit_file(&d, "d.txt", &big, "p4");
    d
}

#[test]
fn range_diff_pairs_by_patch() {
    let t = tempfile::tempdir().unwrap();
    let d = rd_repo(t.path());
    let out = ok(&d, &["range-diff", "main..v1", "main..v2"]);
    let l: Vec<&str> = out.lines().collect();
    assert!(l[0].starts_with("1:  ") && l[0].contains(" = 1:  ") && l[0].ends_with(" p1"), "{out}");
    assert!(l[1].starts_with("2:  ") && l[1].contains(" ! 2:  ") && l[1].ends_with(" p2"), "{out}");
    assert!(l.iter().any(|x| x.starts_with("    @@ b.txt")), "{out}");
    assert!(l.iter().any(|x| x.starts_with("    -") && x.ends_with("b")), "{out}");
    assert!(l.iter().any(|x| x.starts_with("    +") && x.ends_with("b changed")), "{out}");
    assert!(l.iter().any(|x| x.starts_with("3:  ") && x.contains(" < -:  ------- p3")), "{out}");
    assert!(l.iter().any(|x| x.starts_with("-:  ------- > 3:  ") && x.ends_with(" p4")), "{out}");
    // the three-argument form is the same comparison
    assert_eq!(ok(&d, &["range-diff", "main", "v1", "v2"]), out);
    // -s drops the inner diff
    let s = ok(&d, &["range-diff", "-s", "main..v1", "main..v2"]);
    assert_eq!(s.lines().count(), 4, "{s}");
    assert!(!s.contains("@@"), "{s}");
    // identical ranges: all `=`
    let same = ok(&d, &["range-diff", "main..v1", "main..v1"]);
    assert_eq!(same.lines().count(), 3);
    assert!(same.lines().all(|x| x.contains(" = ")), "{same}");
    // the symmetric form runs, and a malformed call is refused
    ok(&d, &["range-diff", "v1...v2"]);
    assert!(fail(&d, &["range-diff", "main..v1"]).contains("usage"));
    // `vcs git range-diff` translates
    assert_eq!(ok(&d, &["git", "range-diff", "main..v1", "main..v2"]), out);
}

#[test]
fn range_diff_creation_factor_pairs_more() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("r");
    ok(t.path(), &["init", "r"]);
    commit_file(&d, "base.txt", "base\n", "base");
    ok(&d, &["switch", "-c", "x"]);
    commit_file(&d, "f.txt", "one\ntwo\nthree\n", "work");
    ok(&d, &["switch", "main"]);
    ok(&d, &["switch", "-c", "y"]);
    commit_file(&d, "f.txt", "one\ntwo\nthree\nfour\n", "work again");
    // default factor 60: a small edit is paired
    let out = ok(&d, &["range-diff", "-s", "main..x", "main..y"]);
    assert!(out.contains(" ! 1:  "), "{out}");
    // factor 0: leaving a commit unpaired is free, so nothing is paired
    let out = ok(&d, &["range-diff", "-s", "--creation-factor", "0", "main..x", "main..y"]);
    assert!(out.contains(" < ") && out.contains(" > "), "{out}");
}

#[test]
fn show_branch_matrix() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("r");
    ok(t.path(), &["init", "r"]);
    commit_file(&d, "base.txt", "base\n", "base");
    ok(&d, &["switch", "-c", "topic"]);
    commit_file(&d, "t1.txt", "1\n", "t1");
    commit_file(&d, "t2.txt", "2\n", "t2");
    ok(&d, &["switch", "main"]);
    commit_file(&d, "m1.txt", "m\n", "m1");
    let out = ok(&d, &["show-branch", "main", "topic"]);
    let l: Vec<&str> = out.lines().collect();
    assert_eq!(l[0], "* [main] m1", "{out}");
    assert_eq!(l[1], " ! [topic] t2", "{out}");
    assert_eq!(l[2], "--", "{out}");
    let rows = &l[3..];
    assert!(rows.contains(&"*  [main] m1"), "{out}");
    assert!(rows.contains(&" + [topic] t2"), "{out}");
    assert!(rows.contains(&" + [topic^] t1"), "{out}");
    assert!(rows.contains(&"*+ [main^] base"), "{out}");
    assert_eq!(rows.len(), 4, "{out}");
    // default: every local branch
    assert_eq!(ok(&d, &["show-branch"]), out);
    // --list: heads only
    assert_eq!(ok(&d, &["show-branch", "--list"]), "* [main] m1\n  [topic] t2\n");
    // --sha1-name and --no-name
    let s = ok(&d, &["show-branch", "--sha1-name", "main", "topic"]);
    assert!(!s.lines().skip(3).any(|x| x.contains("main^")), "{s}");
    let n = ok(&d, &["show-branch", "--no-name", "main", "topic"]);
    assert!(n.lines().skip(3).any(|x| x == " + t2"), "{n}");
    // --merge-base is `merge-base`, --independent drops ancestors
    let mb = ok(&d, &["merge-base", "main", "topic"]);
    assert_eq!(ok(&d, &["show-branch", "--merge-base", "main", "topic"]), mb);
    let ind = ok(&d, &["show-branch", "--independent", "main", "main~1", "topic"]);
    assert_eq!(ind.lines().count(), 2, "{ind}");
    // --more goes past the common ancestor
    let more = ok(&d, &["show-branch", "--more", "1", "main", "topic"]);
    assert_eq!(more.lines().count(), out.lines().count(), "root commit has no parent: {more}");
    // a single revision: its history, no matrix header
    let one = ok(&d, &["show-branch", "--more", "1", "topic"]);
    assert_eq!(one, "[topic] t2\n[topic^] t1\n", "{one}");
    // `vcs git show-branch` translates, and refuses what is not implemented
    assert_eq!(ok(&d, &["git", "show-branch", "main", "topic"]), out);
    assert!(fail(&d, &["git", "show-branch", "-r"]).contains("no vcs equivalent"));
}

#[test]
fn show_branch_merge_shows_dash_and_second_parent_names() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("r");
    ok(t.path(), &["init", "r"]);
    commit_file(&d, "base.txt", "base\n", "base");
    ok(&d, &["switch", "-c", "side"]);
    commit_file(&d, "s.txt", "s\n", "s1");
    ok(&d, &["switch", "main"]);
    commit_file(&d, "m.txt", "m\n", "m1");
    ok(&d, &["merge", "side", "-m", "join"]);
    ok(&d, &["switch", "-c", "old", "main~2"]);
    ok(&d, &["switch", "main"]);
    let out = ok(&d, &["show-branch", "main", "old"]);
    assert!(out.lines().any(|x| x == "*  [main] join" || x == "-  [main] join"), "{out}");
    assert!(out.contains("[main^2] s1") && out.contains("[main^] m1"), "{out}");
}
