//! `vcs diff` / `show` / `log` / `blame` options on hand-built histories.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(dir: &Path, envs: &[(&str, &str)], args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
    c.args(args)
        .current_dir(dir)
        .env_remove("NO_COLOR")
        .env("VCS_AUTHOR_NAME", "Ann")
        .env("VCS_AUTHOR_EMAIL", "ann@example.com");
    for (k, v) in envs {
        c.env(k, v);
    }
    c.output().unwrap()
}
fn vcs(dir: &Path, args: &[&str]) -> String {
    vcs_env(dir, &[], args)
}
fn vcs_env(dir: &Path, envs: &[(&str, &str)], args: &[&str]) -> String {
    let o = run(dir, envs, args);
    assert!(o.status.success(), "vcs {args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn fails(dir: &Path, args: &[&str]) -> String {
    let o = run(dir, &[], args);
    assert!(!o.status.success(), "vcs {args:?} should fail");
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn write(d: &Path, file: &str, content: &[u8]) {
    let p = d.join(file);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}
fn commit_at(d: &Path, ts: u64, files: &[(&str, &str)], msg: &str) {
    for (f, c) in files {
        write(d, f, c.as_bytes());
        vcs(d, &["add", f]);
    }
    vcs_env(d, &[("VCS_TIMESTAMP", &ts.to_string())], &["commit", "-m", msg]);
}
fn repo() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    vcs(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    (t, d)
}

const DAY: u64 = 86400;
const T0: u64 = 1_700_000_000; // 2023-11-14

const LIB: &str = "fn one() {}\nfn two() {}\nfn three() {}\nfn four() {}\nfn five() {}\n";

fn text(n: usize) -> String {
    (1..=n).map(|i| format!("line number {i} of the file\n")).collect()
}

#[test]
fn name_only_name_status_and_context() {
    let (_t, d) = repo();
    commit_at(&d, T0, &[("a.txt", &text(20)), ("b.txt", "keep\n"), ("gone.txt", "bye\n")], "c1");
    write(&d, "a.txt", text(20).replace("number 10 ", "number TEN ").as_bytes());
    write(&d, "new.txt", b"x\n");
    std::fs::remove_file(d.join("gone.txt")).unwrap();
    vcs(&d, &["add", "a.txt", "new.txt", "gone.txt"]);
    assert_eq!(vcs(&d, &["diff", "--staged", "--name-only"]), "a.txt\ngone.txt\nnew.txt\n");
    assert_eq!(
        vcs(&d, &["diff", "--staged", "--name-status"]),
        "M\ta.txt\nD\tgone.txt\nA\tnew.txt\n"
    );
    let u0 = vcs(&d, &["diff", "--staged", "-U0"]);
    let u1 = vcs(&d, &["diff", "--staged", "-U1"]);
    let u3 = vcs(&d, &["diff", "--staged"]);
    let ctx = |s: &str| s.lines().filter(|l| l.starts_with(' ')).count();
    assert_eq!(ctx(&u0), 0);
    assert_eq!(ctx(&u1), 2);
    assert_eq!(ctx(&u3), 6);
    assert!(u1.contains("@@ -9,3 +9,3 @@"), "{u1}");
}

#[test]
fn worktree_diff_options() {
    let (_t, d) = repo();
    commit_at(&d, T0, &[("w.txt", "alpha  beta\ngamma\n"), ("s.txt", "same\n")], "c1");
    write(&d, "w.txt", b"alpha beta\ngamma\n"); // whitespace only
    assert!(vcs(&d, &["diff"]).contains("-alpha  beta"));
    assert_eq!(vcs(&d, &["diff", "-w"]), "");
    assert_eq!(vcs(&d, &["diff", "--ignore-all-space"]), "");
    write(&d, "w.txt", b"alpha beta\ngamma delta\n");
    let w = vcs(&d, &["diff", "--word-diff"]);
    assert!(
        w.contains("gamma{+ delta+}") || w.contains("gamma{+ +}{+delta+}") || w.contains("{+ delta+}"),
        "{w}"
    );
    let stat = vcs(&d, &["diff", "--stat", "-w"]);
    assert!(stat.contains("w.txt | +1 -1"), "{stat}");
}

#[test]
fn color_respects_no_color_and_tty() {
    let (_t, d) = repo();
    commit_at(&d, T0, &[("c.txt", "one\ntwo\n")], "c1");
    write(&d, "c.txt", b"one\n2\n");
    let plain = vcs(&d, &["diff"]);
    assert!(!plain.contains('\x1b'));
    let col = vcs(&d, &["diff", "--color"]);
    assert!(col.contains("\x1b[31m-two") && col.contains("\x1b[32m+2"), "{col:?}");
    assert!(vcs(&d, &["diff", "--color=always"]).contains('\x1b'));
    assert!(!vcs(&d, &["diff", "--color=never"]).contains('\x1b'));
    // NO_COLOR beats a bare --color, but not an explicit =always
    let o = run(&d, &[("NO_COLOR", "1")], &["diff", "--color"]);
    assert!(!String::from_utf8_lossy(&o.stdout).contains('\x1b'));
    let o = run(&d, &[("NO_COLOR", "1")], &["diff", "--color=always"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains('\x1b'));
    // not a terminal: auto means no colour
    assert!(!vcs(&d, &["diff", "--color=auto"]).contains('\x1b'));
}

#[test]
fn color_moved_marks_moved_lines() {
    let (_t, d) = repo();
    commit_at(&d, T0, &[("m.txt", "head\nmoved line\nmid\nend\n")], "c1");
    write(&d, "m.txt", b"head\nmid\nend\nmoved line\n");
    let out = vcs(&d, &["diff", "--color", "--color-moved", "-U0"]);
    assert!(out.contains("\x1b[1;35m-moved line"), "{out:?}");
    assert!(out.contains("\x1b[1;36m+moved line"), "{out:?}");
    let plain = vcs(&d, &["diff", "--color"]);
    assert!(!plain.contains("1;35"), "{plain:?}");
}

#[test]
fn every_algorithm_gives_a_patch_and_names_are_validated() {
    let (_t, d) = repo();
    commit_at(&d, T0, &[("p.txt", "a\nb\nc\nd\ne\n")], "c1");
    write(&d, "p.txt", b"a\nc\nb\nd\nX\ne\n");
    for alg in ["myers", "patience", "histogram"] {
        let out = vcs(&d, &["diff", &format!("--diff-algorithm={alg}")]);
        assert!(out.contains("+X"), "{alg}: {out}");
    }
    assert!(fails(&d, &["diff", "--diff-algorithm=nope"]).contains("invalid value"));
}

#[test]
fn binary_summary_name_only_and_stat() {
    let (_t, d) = repo();
    commit_at(&d, T0, &[("x.txt", "t\n")], "c1");
    write(&d, "img.bin", &[0u8, 1, 2, 3, 4]);
    vcs(&d, &["add", "img.bin"]);
    let out = vcs(&d, &["diff", "--staged", "--diff-algorithm=patience"]);
    assert!(out.contains("Binary files /dev/null and b/img.bin differ (0 -> 5 bytes)"), "{out}");
    let st = vcs(&d, &["diff", "--staged", "--stat"]);
    assert!(st.contains("img.bin | binary") || st.contains("img.bin | Bin"), "{st}");
    let st = vcs(&d, &["diff", "--staged", "--stat", "-U1"]);
    assert!(st.contains("img.bin | Bin 0 -> 5 bytes"), "{st}");
}

#[test]
fn chunked_file_diffs_through_get_content() {
    let (_t, d) = repo();
    let big: String = (0..60_000).map(|i| format!("row {i:06} padding padding\n")).collect();
    assert!(big.len() > (1 << 20) + 1000);
    commit_at(&d, T0, &[("big.txt", &big)], "big");
    let new = big.replace("row 030000 ", "row CHANGED ");
    commit_at(&d, T0 + 10, &[("big.txt", &new)], "edit");
    let show = vcs(&d, &["show", "-U0", "--name-status"]);
    assert!(show.contains("M\tbig.txt"), "{show}");
    let show = vcs(&d, &["show", "-U0", "--diff-algorithm=histogram"]);
    assert!(
        show.contains("-row 030000 padding padding") && show.contains("+row CHANGED padding padding"),
        "{}",
        &show[..show.len().min(600)]
    );
    write(&d, "big.txt", new.replace("row 000001 ", "row ONE ").as_bytes());
    let st = vcs(&d, &["diff", "--stat", "-w"]);
    assert!(st.contains("big.txt | +1 -1"), "{st}");
}

#[test]
fn rename_and_copy_detection_in_diff_show_and_log() {
    let (_t, d) = repo();
    let body = text(12);
    commit_at(&d, T0, &[("old/name.txt", &body), ("lib.txt", LIB)], "add files");
    // rename with a small edit, plus a copy of lib.txt
    std::fs::remove_file(d.join("old/name.txt")).unwrap();
    vcs(&d, &["add", "old/name.txt"]);
    commit_at(
        &d,
        T0 + 10,
        &[
            ("new/moved.txt", &body.replace("number 5 ", "number FIVE ")),
            ("lib.txt", &LIB.replace("fn two", "fn TWO")),
            ("libcopy.txt", LIB),
        ],
        "rename and copy",
    );

    let plain = vcs(&d, &["show", "--name-status"]);
    assert!(plain.contains("D\told/name.txt") && plain.contains("A\tnew/moved.txt"), "{plain}");
    let m = vcs(&d, &["show", "--name-status", "-M"]);
    assert!(m.contains("R0") && m.contains("old/name.txt\tnew/moved.txt"), "{m}");
    assert!(m.contains("A\tlibcopy.txt"), "{m}");
    let c = vcs(&d, &["show", "--name-status", "-M", "--find-copies"]);
    assert!(c.contains("lib.txt\tlibcopy.txt") && c.contains("C1"), "{c}");
    let patch = vcs(&d, &["show", "-M"]);
    assert!(
        patch.contains("rename from old/name.txt") && patch.contains("rename to new/moved.txt"),
        "{patch}"
    );
    assert!(
        patch.contains("-line number 5 of the file") && patch.contains("+line number FIVE of the file"),
        "{patch}"
    );
    // threshold: 100% similarity needed, edited rename no longer pairs
    let strict = vcs(&d, &["show", "--name-status", "-M", "--rename-threshold=100"]);
    assert!(!strict.contains("R0"), "{strict}");
    // log lists files per commit
    let log = vcs(&d, &["log", "--oneline", "--name-status", "-M", "-n", "1"]);
    assert!(
        log.contains("rename and copy") && log.contains("old/name.txt\tnew/moved.txt"),
        "{log}"
    );
    let log = vcs(&d, &["log", "--oneline", "--name-only", "-n", "1"]);
    assert!(log.contains("new/moved.txt") && !log.contains("R0"), "{log}");
}

#[test]
fn log_follow_and_blame_follow_renames() {
    let (_t, d) = repo();
    let v1 = text(10);
    commit_at(&d, T0, &[("a.txt", &v1)], "create a");
    commit_at(&d, T0 + 10, &[("a.txt", &v1.replace("number 3 ", "number THREE "))], "edit a");
    // rename a.txt -> dir/b.txt, small change
    std::fs::remove_file(d.join("a.txt")).unwrap();
    vcs(&d, &["add", "a.txt"]);
    let v2 = v1.replace("number 3 ", "number THREE ").replace("number 8 ", "number EIGHT ");
    commit_at(&d, T0 + 20, &[("dir/b.txt", &v2)], "rename to b");
    commit_at(&d, T0 + 30, &[("dir/b.txt", &v2.replace("number 9 ", "number NINE "))], "edit b");
    commit_at(&d, T0 + 40, &[("other.txt", "x\n")], "unrelated");

    let plain = vcs(&d, &["log", "--oneline", "--", "dir/b.txt"]);
    assert_eq!(plain.lines().count(), 2, "{plain}");
    let f = vcs(&d, &["log", "--oneline", "--follow", "--", "dir/b.txt"]);
    let subjects: Vec<&str> = f.lines().map(|l| l.split_once(' ').unwrap().1).collect();
    assert_eq!(subjects, ["edit b", "rename to b", "edit a", "create a"], "{f}");
    assert!(fails(&d, &["log", "--follow"]).contains("exactly one path"));

    let b = vcs(&d, &["blame", "dir/b.txt"]);
    let first_line_commit = b.lines().next().unwrap().split(' ').next().unwrap().to_owned();
    let create = vcs(&d, &["log", "--oneline", "--format=%h %s"]);
    let create_id = create
        .lines()
        .find(|l| l.ends_with("create a"))
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .to_owned();
    assert_ne!(first_line_commit, create_id, "without -M the rename commit owns every line: {b}");
    let bm = vcs(&d, &["blame", "-M", "dir/b.txt"]);
    let l1 = bm.lines().next().unwrap();
    assert!(l1.starts_with(&create_id) && l1.contains("a.txt"), "{bm}");
    assert!(
        bm.lines().nth(2).unwrap().contains("edit a") || bm.lines().nth(2).unwrap().split(' ').next().unwrap() != create_id,
        "{bm}"
    );
}

#[test]
fn log_filters() {
    let (_t, d) = repo();
    commit_at(&d, T0, &[("f.txt", "alpha\n")], "feat: alpha");
    commit_at(&d, T0 + 10 * DAY, &[("f.txt", "alpha\nbeta needle\n")], "fix: beta");
    commit_at(&d, T0 + 20 * DAY, &[("g.txt", "g\n")], "feat: gamma");
    let day = |n: u64| {
        let s = vcs(&d, &["rev-parse", "HEAD"]);
        let _ = s;
        // YYYY-MM-DD of T0 + n days, computed by the same civil algorithm as the CLI
        let days = (T0 + n * DAY) / DAY;
        let (z, era) = (days as i64 + 719468, (days as i64 + 719468).div_euclid(146097));
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let dd = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        format!("{:04}-{:02}-{:02}", yoe + era * 400 + i64::from(m <= 2), m, dd)
    };
    let subjects = |args: &[&str]| -> Vec<String> {
        let mut a = vec!["log", "--format=%s"];
        a.extend_from_slice(args);
        vcs(&d, &a)
            .lines()
            .filter(|l| *l != "no matching commits")
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(subjects(&[]), ["feat: gamma", "fix: beta", "feat: alpha"]);
    assert_eq!(subjects(&["--since", &day(5)]), ["feat: gamma", "fix: beta"]);
    assert_eq!(subjects(&["--since", &day(15)]), ["feat: gamma"]);
    assert_eq!(subjects(&["--until", &day(10)]), ["fix: beta", "feat: alpha"]);
    assert_eq!(subjects(&["--since", &day(5), "--until", &day(15)]), ["fix: beta"]);
    assert_eq!(subjects(&["--since", "1970-01-01"]).len(), 3);
    assert_eq!(subjects(&["--since", "1 day ago"]).len(), 0);
    assert!(fails(&d, &["log", "--since", "yesteryear"]).contains("cannot parse date"));
    assert_eq!(subjects(&["--grep", "^feat"]), ["feat: gamma", "feat: alpha"]);
    assert_eq!(subjects(&["--grep", "beta|gamma"]).len(), 2);
    assert!(fails(&d, &["log", "--grep", "("]).contains("--grep"));
    assert_eq!(subjects(&["-Sneedle"]), ["fix: beta"]);
    assert_eq!(subjects(&["-S", "alpha"]), ["feat: alpha"]);
    assert_eq!(subjects(&["-Snothing-here"]).len(), 0);
    assert_eq!(subjects(&["--no-merges"]).len(), 3);
    assert_eq!(subjects(&["--merges"]).len(), 0);
    assert_eq!(subjects(&["--first-parent"]).len(), 3);
    // filters apply before -n
    assert_eq!(subjects(&["--grep", "feat", "-n", "1"]), ["feat: gamma"]);
}

#[test]
fn merge_filters_and_first_parent() {
    let (_t, d) = repo();
    commit_at(&d, T0, &[("base.txt", "b\n")], "base");
    vcs(&d, &["branch", "side"]);
    vcs(&d, &["zone", "checkout", "root", "side"]);
    commit_at(&d, T0 + 10, &[("side.txt", "s\n")], "on side");
    vcs(&d, &["zone", "checkout", "root", "main"]);
    commit_at(&d, T0 + 20, &[("main.txt", "m\n")], "on main");
    vcs_env(&d, &[("VCS_TIMESTAMP", &(T0 + 30).to_string())], &["merge", "side"]);
    let subjects = |args: &[&str]| -> Vec<String> {
        let mut a = vec!["log", "--format=%s", "-n", "50"];
        a.extend_from_slice(args);
        vcs(&d, &a).lines().map(str::to_owned).collect()
    };
    assert_eq!(subjects(&[]).len(), 4);
    assert_eq!(subjects(&["--merges"]).len(), 1);
    let nm = subjects(&["--no-merges"]);
    assert_eq!(nm.len(), 3);
    assert!(nm.contains(&"on side".to_string()));
    let fp = subjects(&["--first-parent"]);
    assert_eq!(fp.len(), 3, "{fp:?}");
    assert!(!fp.contains(&"on side".to_string()));
    assert_eq!(subjects(&["--first-parent", "--no-merges"]), ["on main", "base"]);
    assert!(fails(&d, &["log", "--merges", "--no-merges"]).contains("cannot be used"));
}

#[test]
fn format_placeholders_and_json() {
    let (_t, d) = repo();
    commit_at(&d, T0, &[("f.txt", "one\n")], "subject line\n\nbody text here");
    let full = vcs(&d, &["rev-parse", "HEAD"]);
    let full = full.trim();
    let out = vcs(&d, &["log", "--format=[%h|%an|%ae|%at|%ad|%s|%b|%%|%n]"]);
    assert!(
        out.starts_with(&format!(
            "[{}|Ann|ann@example.com|{T0}|2023-11-14 22:13:20|subject line|body text here|%|",
            &full[..full.len().min(12)]
        )) || out.contains("|Ann|ann@example.com|1700000000|2023-11-14 22:13:20|subject line|body text here|%|"),
        "{out}"
    );
    let h = vcs(&d, &["log", "--format=%H"]);
    assert_eq!(h.trim(), full);
    assert_eq!(vcs(&d, &["log", "--format=%P"]).trim(), "");
    assert!(vcs(&d, &["log", "--format=%q%z"]).starts_with("%q"));
    // --json honours the filters
    commit_at(&d, T0 + DAY, &[("g.txt", "two\n")], "second");
    let j = vcs(&d, &["--json", "log", "--grep", "^second"]);
    assert!(j.contains("\"second\"") && !j.contains("subject line"), "{j}");
    let j = vcs(&d, &["--json", "log", "-Sone"]);
    assert!(j.contains("subject line") && !j.contains("\"second\""), "{j}");
}
