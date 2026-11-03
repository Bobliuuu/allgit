//! `vcs log --grep` follows `git log --grep`: basic regex by default, -E extended, -F fixed, -i.
//! The expected results are produced by running git itself on the same commit subjects.
use std::path::Path;
use std::process::Command;

fn run(bin: &str, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let o = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(o.status.success(), "{bin} {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

// Known difference, not tested: git matches `--grep '^$'` against every commit because it searches the raw
// commit buffer, whose header ends in an empty line; vcs searches the message text only.
const SUBJECTS: &[&str] = &[
    "net+api: timeouts",
    "api+core: shared error type",
    "core: ledger rounding fix",
    "storage: drop legacy index",
    "fix (parser) crash",
    "Fix a.b dot",
    "fixaxb no dot",
    "aaa three",
    "docs: update API reference",
    "x* star",
    "price $5 end$",
    "^caret start",
    "a|b pipe",
    "brace {2} here",
    "tab[1] bracket",
];

#[test]
fn grep_matches_git_for_each_flavour() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("git not installed: skipped");
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let (g, v) = (t.path().join("g"), t.path().join("v"));
    std::fs::create_dir_all(&g).unwrap();
    std::fs::create_dir_all(&v).unwrap();
    let vcs = env!("CARGO_BIN_EXE_vcs");
    let genv = [
        ("GIT_AUTHOR_NAME", "D"),
        ("GIT_AUTHOR_EMAIL", "d@e"),
        ("GIT_COMMITTER_NAME", "D"),
        ("GIT_COMMITTER_EMAIL", "d@e"),
    ];
    run("git", &g, &["init", "-q"], &genv);
    run(vcs, &v, &["init", "."], &[]);
    let venv = [("VCS_AUTHOR_NAME", "D"), ("VCS_AUTHOR_EMAIL", "d@e")];
    for (i, s) in SUBJECTS.iter().enumerate() {
        for d in [&g, &v] {
            std::fs::write(d.join("f"), format!("{i}\n")).unwrap();
        }
        run("git", &g, &["add", "f"], &genv);
        run("git", &g, &["commit", "-q", "-m", s], &genv);
        run(vcs, &v, &["add", "f"], &venv);
        run(vcs, &v, &["commit", "-m", s], &venv);
    }
    let cases: &[&[&str]] = &[
        &["--grep", "net+api"],
        &["--grep", "api+core: shared"],
        &["--grep", "fix"],
        &["--grep", "Fix", "-i"],
        &["--grep", "fix", "-i"],
        &["--grep", "a.b"],
        &["--grep", "a.b", "-F"],
        &["--grep", "a\\.b"],
        &["--grep", "^core"],
        &["--grep", "core$"],
        &["--grep", "type$"],
        &["--grep", "a$b"],
        &["--grep", "x*"],
        &["--grep", "*"],
        &["--grep", "aa*"],
        &["--grep", "a\\+"],
        &["--grep", "a\\{3\\}"],
        &["--grep", "a{3}"],
        &["--grep", "a|b"],
        &["--grep", "a\\|b"],
        &["--grep", "(parser)"],
        &["--grep", "\\(parser\\)"],
        &["--grep", "(parser)", "-E"],
        &["--grep", "a+", "-E"],
        &["--grep", "net+api", "-E"],
        &["--grep", "ledger|legacy", "-E"],
        &["--grep", "ledger\\|legacy"],
        &["--grep", "^(fix|Fix)", "-E"],
        &["--grep", "[0-9]"],
        &["--grep", "tab[[]1]"],
        &["--grep", "{2}"],
        &["--grep", "\\^caret"],
        &["--grep", "$5"],
        &["--grep", "5 end$"],
        &["--grep", "nomatchatall"],
    ];
    let mut bad = Vec::new();
    for c in cases {
        let mut ga = vec!["log", "--format=%s"];
        ga.extend_from_slice(c);
        let mut va = vec!["log", "--format", "%s"];
        va.extend_from_slice(c);
        let want = run("git", &g, &ga, &genv);
        let o = Command::new(vcs).args(&va).current_dir(&v).output().unwrap();
        let mut got = String::from_utf8_lossy(&o.stdout).into_owned();
        if got.trim() == "no matching commits" {
            got.clear();
        }
        if want != got {
            bad.push(format!(
                "{c:?}\n git: {want:?}\n vcs: {got:?} (stderr {})",
                String::from_utf8_lossy(&o.stderr)
            ));
        }
    }
    assert!(bad.is_empty(), "differences from git:\n{}", bad.join("\n"));
}
