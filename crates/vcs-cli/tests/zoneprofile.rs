//! `vcs profile`, `sync --auto`, `clone --auto-profile` with the real binaries on a generated
//! multi-zone repository, against a directory remote and against `vcs-server` over http.
//! Contract: docs/zone-profile.md.
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

const DAY: u64 = 86_400_000;
const NOW: u64 = 100 * DAY;

fn vcs(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_PRINCIPAL", "alice")
        .env("VCS_TOKEN", "tok-alice")
        .env("VCS_PROFILE_NOW_MS", NOW.to_string())
        .env("VCS_NO_CACHE", "1")
        .output()
        .unwrap()
}
fn ok(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(
        o.status.success(),
        "vcs {args:?} failed: {}{}",
        String::from_utf8_lossy(&o.stderr),
        String::from_utf8_lossy(&o.stdout)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn fail(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(!o.status.success(), "vcs {args:?} unexpectedly succeeded");
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn write(dir: &Path, f: &str, c: &[u8]) {
    let p = dir.join(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}

/// Incompressible, deterministic bytes.
fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// (requests, bytes) from a `requests: N, downloaded: M bytes` line (the last one printed).
fn stats(out: &str) -> (u64, u64) {
    let l = out
        .lines()
        .filter(|l| l.contains("requests ") || l.contains("requests:"))
        .last()
        .unwrap_or_else(|| panic!("no stats line in {out}"));
    let num = |after: &str| -> u64 {
        let i = l.find(after).unwrap() + after.len();
        l[i..]
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    (num("requests"), num("downloaded"))
}

const ZONES: [(&str, &[&str]); 8] = [
    ("web", &["ui"]),
    ("ui", &["core"]),
    ("api", &["core"]),
    ("core", &[]),
    ("billing", &["core"]),
    ("docs", &[]),
    ("legacy", &[]),
    ("tools", &[]),
];

fn publisher(root: &Path, url: &str) -> PathBuf {
    ok(root, &["init", "pub"]);
    let a = root.join("pub");
    for (z, _) in ZONES {
        ok(&a, &["zone", "create", z, "--path", &format!("crates/{z}")]);
    }
    for (i, (z, deps)) in ZONES.iter().enumerate() {
        let mut toml = format!("[package]\nname = \"{z}\"\nversion = \"0.1.0\"\n\n[dependencies]\n");
        for d in *deps {
            toml.push_str(&format!("{d} = {{ path = \"../{d}\" }}\n"));
        }
        write(&a, &format!("crates/{z}/Cargo.toml"), toml.as_bytes());
        for f in 0..12 {
            write(&a, &format!("crates/{z}/src/f{f}.rs"), &noise((i * 100 + f) as u64, 6000));
        }
    }
    write(&a, "README", b"root\n");
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "init"]);
    ok(&a, &["remote", "set", url]);
    ok(&a, &["publish"]);
    a
}

fn hot_of(out: &str) -> Vec<String> {
    let mut v = vec![];
    let mut on = false;
    for l in out.lines() {
        if l.starts_with("hot zones") {
            on = true;
        } else if l.starts_with("skipped zones") {
            on = false;
        } else if on && l.starts_with("  ") && !l.trim_start().starts_with('-') {
            v.push(l.split_whitespace().next().unwrap().to_owned());
        }
    }
    v
}
fn skipped_of(out: &str) -> Vec<String> {
    let mut v = vec![];
    let mut on = false;
    for l in out.lines() {
        if l.starts_with("skipped zones") {
            on = true;
        } else if on && l.starts_with("  ") && !l.trim_start().starts_with('-') && !l.trim_start().starts_with("get one") {
            v.push(l.split_whitespace().next().unwrap().to_owned());
        } else if on && !l.starts_with(' ') {
            on = false;
        }
    }
    v
}

/// A profile of a web developer: edits `web`, reads `ui` in the same sessions.
fn seed_profile(path: &Path) {
    let mut s = String::from("vcs-profile 1\n");
    for d in [90u64, 93, 96, 99] {
        s.push_str(&format!("{}\te\tweb\tsrc\n{}\tr\tui\tsrc\n", d * DAY, d * DAY + 60_000));
    }
    s.push_str(&format!("{}\tr\tdocs\t\n", 20 * DAY));
    std::fs::write(path, s).unwrap();
}

fn scenario(root: &Path, url: &str) {
    let a = publisher(root, url);

    // baseline: everything
    let out = ok(root, &["clone", url, "full", "--stats"]);
    let (req_full, bytes_full) = stats(&out);
    // the same full clone with ranged reads only (what `--auto` uses), so requests compare fairly
    let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(["clone", url, "full0", "--stats"])
        .current_dir(root)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_TOKEN", "tok-alice")
        .env("VCS_SMALL_PACK_BYTES", "0")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (req_full0, bytes_full0) = stats(&String::from_utf8_lossy(&o.stdout));

    // predicted clone: dry run first (changes nothing)
    let prof = root.join("seed.profile");
    seed_profile(&prof);
    let dry = ok(
        root,
        &[
            "clone",
            url,
            "dry",
            "--auto-profile",
            prof.to_str().unwrap(),
            "--dry-run",
            "--explain",
        ],
    );
    assert!(!root.join("dry").exists(), "dry run must not create the destination");
    assert!(dry.contains("used directly"), "{dry}");

    let out = ok(root, &["clone", url, "auto", "--auto-profile", prof.to_str().unwrap(), "--stats"]);
    let (req_auto, bytes_auto) = stats(&out);
    let hot = hot_of(&out);
    for z in ["root", "web", "ui", "core"] {
        assert!(hot.contains(&z.to_owned()), "{z} should be hot (edited, read, dependency): {out}");
    }
    let skipped = skipped_of(&out);
    // nothing is dropped silently: hot + skipped cover every zone
    let mut all: Vec<String> = hot.iter().chain(skipped.iter()).cloned().collect();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), ZONES.len() + 1, "{out}");
    assert!(
        skipped.contains(&"billing".to_owned()) && skipped.contains(&"legacy".to_owned()),
        "{out}"
    );
    assert!(out.contains("vcs profile fetch"), "{out}");
    let auto = root.join("auto");
    assert!(auto.join("crates/web/src/f0.rs").exists() && auto.join("crates/core/src/f3.rs").exists());
    assert!(!auto.join("crates/billing/src/f0.rs").exists(), "skipped zone has no contents");
    eprintln!("{out}");
    assert!(bytes_auto < bytes_full, "auto {bytes_auto} B must be less than full {bytes_full} B");
    assert!(req_auto > 0 && req_full > 0);
    eprintln!("clone: full(whole small packs) {req_full} req / {bytes_full} B, full(ranged) {req_full0} req / {bytes_full0} B, auto {req_auto} req / {bytes_auto} B");

    // skipped zones are listed offline and fetchable with one command; commits and trees are local
    let sk = ok(&auto, &["profile", "skipped"]);
    assert!(sk.contains("billing") && sk.contains("commits and trees local"), "{sk}");
    let f = ok(&auto, &["profile", "fetch", "billing", "--stats"]);
    assert!(f.contains("billing: fetched and subscribed"), "{f}");
    assert_eq!(
        std::fs::read(auto.join("crates/billing/src/f5.rs")).unwrap(),
        noise(4 * 100 + 5, 6000)
    );
    assert!(ok(&auto, &["profile", "fetch", "billing"]).contains("already subscribed"));
    ok(&auto, &["fsck"]);
    assert!(fail(&auto, &["profile", "fetch", "nope"]).contains("no such zone"));

    // a change on the remote: web and legacy change; sync --auto brings web (subscribed), keeps legacy as listed
    write(&a, "crates/web/src/f0.rs", &noise(999, 6000));
    write(&a, "crates/legacy/src/f0.rs", &noise(998, 6000));
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "more"]);
    ok(&a, &["publish"]);
    let out = ok(&auto, &["sync", "--auto", "--stats"]);
    assert!(out.contains("web/main: fast-forwarded"), "{out}");
    assert!(!out.contains("legacy/main: fast-forwarded"), "legacy is not subscribed: {out}");
    assert!(skipped_of(&out).contains(&"legacy".to_owned()), "{out}");
    assert_eq!(std::fs::read(auto.join("crates/web/src/f0.rs")).unwrap(), noise(999, 6000));
    let (_, sync_bytes) = stats(&out);
    assert!(
        sync_bytes < bytes_full,
        "incremental auto sync {sync_bytes} B < full clone {bytes_full} B"
    );
    // the legacy zone is still fetchable on demand, at its new head
    ok(&auto, &["profile", "fetch", "legacy"]);
    assert_eq!(std::fs::read(auto.join("crates/legacy/src/f0.rs")).unwrap(), noise(998, 6000));
    ok(&auto, &["fsck"]);
}

#[test]
fn auto_clone_and_sync_on_a_directory_remote() {
    let t = tempfile::tempdir().unwrap();
    let remote = t.path().join("remote");
    scenario(t.path(), &format!("file://{}", remote.display()));
}

// ---- vcs-server -------------------------------------------------------------------------

/// Flags that make a nested `cargo build` land next to the test's own `vcs` binary: the same target
/// directory and profile (`cargo test --release` would otherwise build a debug server elsewhere).
fn nested_build_flags(vcs: &Path) -> Vec<String> {
    let mut f = Vec::new();
    if let (Some(prof), Some(target)) = (vcs.parent(), vcs.parent().and_then(|p| p.parent())) {
        f.push("--target-dir".to_owned());
        f.push(target.display().to_string());
        match prof.file_name().and_then(|n| n.to_str()) {
            Some("debug") | None => {}
            Some("release") => f.push("--release".into()),
            Some(other) => f.extend(["--profile".to_owned(), other.to_owned()]),
        }
    }
    f
}

fn server_bin() -> PathBuf {
    let vcs = PathBuf::from(env!("CARGO_BIN_EXE_vcs"));
    let p = vcs.with_file_name("vcs-server");
    if !p.exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let st = Command::new(cargo)
            .args(["build", "-p", "vcs-server", "--bin", "vcs-server"])
            .args(nested_build_flags(&vcs))
            .status()
            .unwrap();
        assert!(st.success() && p.exists(), "could not build vcs-server");
    }
    p
}
struct Srv {
    child: Child,
    url: String,
}
impl Drop for Srv {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn start_server(dir: &Path) -> Srv {
    let h = Command::new(server_bin()).args(["hash-token", "tok-alice"]).output().unwrap();
    let hash = String::from_utf8(h.stdout).unwrap().trim().to_owned();
    let tokens = dir.join("tokens");
    std::fs::write(&tokens, format!("# principal role zones token-hash\nalice write * {hash}\n")).unwrap();
    let data = dir.join("data");
    let mut child = Command::new(server_bin())
        .args([
            "--root",
            data.to_str().unwrap(),
            "--tokens",
            tokens.to_str().unwrap(),
            "--listen",
            "127.0.0.1:0",
        ])
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stderr.take().unwrap()).read_line(&mut line).unwrap();
    let url = line
        .split_whitespace()
        .find(|w| w.starts_with("http://"))
        .unwrap_or_else(|| panic!("no address: {line:?}"))
        .to_owned();
    Srv { child, url }
}

#[test]
fn auto_clone_and_sync_against_vcs_server() {
    let t = tempfile::tempdir().unwrap();
    let srv = start_server(t.path());
    scenario(t.path(), &srv.url);
}

// ---- recording, privacy, opt-out, hints ---------------------------------------------------

#[test]
fn profile_records_without_file_names_and_opts_out() {
    let t = tempfile::tempdir().unwrap();
    let remote = format!("file://{}", t.path().join("remote").display());
    publisher(t.path(), &remote);
    let c = t.path().join("c");
    ok(t.path(), &["clone", &remote, "c"]);
    // work in a zone directory
    write(&c, "crates/web/src/secret_plan_name.rs", b"fn x(){}\n");
    ok(&c.join("crates/web"), &["status"]);
    ok(&c, &["add", "crates/web/src/secret_plan_name.rs"]);
    ok(&c, &["commit", "-m", "x"]);
    let raw = std::fs::read_to_string(c.join(".vcs/profile")).unwrap();
    assert!(raw.starts_with("vcs-profile 1\n"));
    assert!(raw.lines().any(|l| l.contains("\te\tweb\t")), "edit of web recorded: {raw}");
    assert!(raw.lines().any(|l| l.contains("\tr\tweb\t")), "status in web recorded: {raw}");
    assert!(
        !raw.contains("secret_plan_name") && !raw.contains("f0.rs"),
        "no file names in the profile: {raw}"
    );
    assert!(ok(&c, &["profile", "show"]).contains("never uploaded"));
    // the build wrapper passes the exit status through and records a build event
    let o = vcs(&c.join("crates/ui"), &["profile", "record", "--", "sh", "-c", "exit 3"]);
    assert_eq!(o.status.code(), Some(3));
    assert!(std::fs::read_to_string(c.join(".vcs/profile")).unwrap().contains("\tb\tui\t"));
    // opt out: erases and stops recording
    ok(&c, &["profile", "off"]);
    ok(&c, &["status"]);
    let raw = std::fs::read_to_string(c.join(".vcs/profile")).unwrap();
    assert_eq!(raw.lines().count(), 2, "{raw}");
    assert!(raw.contains("disabled"));
    // VCS_PROFILE=off disables recording for one process
    ok(&c, &["profile", "on"]);
    let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(["status"])
        .current_dir(&c)
        .env("VCS_PROFILE", "off")
        .output()
        .unwrap();
    assert!(o.status.success());
    assert_eq!(std::fs::read_to_string(c.join(".vcs/profile")).unwrap().lines().count(), 1);
}

#[test]
fn checked_in_hints_choose_zones_without_any_profile() {
    let t = tempfile::tempdir().unwrap();
    let remote = format!("file://{}", t.path().join("remote").display());
    let a = publisher(t.path(), &remote);
    write(
        &a,
        ".vcs-profile-hints.toml",
        b"[profile]\nrole = \"backend\"\n[role.backend]\nzones = [\"api\"]\n[exclude]\nzones = [\"legacy\"]\n[pin]\nzones = [\"tools\"]\n",
    );
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "hints"]);
    ok(&a, &["publish"]);
    let out = ok(t.path(), &["clone", &remote, "c", "--auto", "--explain"]);
    let hot = hot_of(&out);
    for z in ["api", "tools", "root"] {
        assert!(hot.contains(&z.to_owned()), "{z}: {out}");
    }
    assert!(!hot.contains(&"web".to_owned()), "{out}");
    assert!(out.contains("excluded by hints"), "{out}");
    // api depends on core: pulled in by the dependency expansion even though only api is named
    assert!(hot.contains(&"core".to_owned()), "{out}");
    // a typo in the hints is an error, not a silent change
    write(&a, ".vcs-profile-hints.toml", b"[profile]\nrolle = \"x\"\n");
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "typo"]);
    ok(&a, &["publish"]);
    assert!(fail(t.path(), &["clone", &remote, "c2", "--auto"]).contains("unknown key"));
}

#[test]
fn budgets_are_honoured_and_skips_are_explained() {
    let t = tempfile::tempdir().unwrap();
    let remote = format!("file://{}", t.path().join("remote").display());
    publisher(t.path(), &remote);
    let prof = t.path().join("p");
    seed_profile(&prof);
    let out = ok(
        t.path(),
        &[
            "clone",
            &remote,
            "c",
            "--auto-profile",
            prof.to_str().unwrap(),
            "--budget-zones",
            "1",
        ],
    );
    let hot = hot_of(&out);
    assert_eq!(hot.iter().filter(|z| *z != "root").count(), 1, "{out}");
    assert!(out.contains("over the zone budget (1)"), "{out}");
    let out = ok(
        t.path(),
        &[
            "clone",
            &remote,
            "d",
            "--auto-profile",
            prof.to_str().unwrap(),
            "--budget-bytes",
            "1K",
        ],
    );
    assert!(out.contains("over the byte budget"), "{out}");
    assert!(fail(
        t.path(),
        &[
            "clone",
            &remote,
            "e",
            "--auto-profile",
            prof.to_str().unwrap(),
            "--budget-bytes",
            "x"
        ]
    )
    .contains("--budget-bytes"));
}
