//! `vcs archive`: extract with the system tar/unzip (skipped if absent), reproducibility.
use std::path::Path;
use std::process::{Command, Output};

fn vcs(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_TIMESTAMP", "1700000000")
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
fn have(tool: &str) -> bool {
    Command::new(tool).arg("--help").output().is_ok() || Command::new(tool).output().is_ok()
}

#[cfg(unix)]
fn repo() -> (tempfile::TempDir, std::path::PathBuf) {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    std::fs::create_dir_all(d.join("src/deep")).unwrap();
    std::fs::write(d.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(d.join("src/deep/b.txt"), "beta\n").unwrap();
    std::fs::write(d.join("run.sh"), "#!/bin/sh\necho hi\n").unwrap();
    std::fs::set_permissions(d.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    symlink("a.txt", d.join("link")).unwrap();
    let big: Vec<u8> = (0..300_000u32).flat_map(|i| i.to_le_bytes()).collect();
    std::fs::write(d.join("big.bin"), &big).unwrap();
    ok(&d, &["add", "."]);
    ok(&d, &["commit", "-m", "init"]);
    (t, d)
}

#[cfg(unix)]
fn check_tree(x: &Path) {
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::read_to_string(x.join("a.txt")).unwrap(), "alpha\n");
    assert_eq!(std::fs::read_to_string(x.join("src/deep/b.txt")).unwrap(), "beta\n");
    assert_ne!(
        std::fs::metadata(x.join("run.sh")).unwrap().permissions().mode() & 0o111,
        0,
        "exec bit"
    );
    assert_eq!(std::fs::metadata(x.join("a.txt")).unwrap().permissions().mode() & 0o111, 0);
    assert_eq!(std::fs::read_link(x.join("link")).unwrap().to_str(), Some("a.txt"));
    assert_eq!(std::fs::metadata(x.join("big.bin")).unwrap().len(), 1_200_000);
}

#[cfg(unix)]
#[test]
fn tar_roundtrip_and_reproducible() {
    let (t, d) = repo();
    let f1 = t.path().join("one.tar");
    let f2 = t.path().join("two.tar");
    ok(&d, &["archive", "-o", f1.to_str().unwrap()]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    ok(&d, &["archive", "HEAD", "-o", f2.to_str().unwrap()]);
    assert_eq!(std::fs::read(&f1).unwrap(), std::fs::read(&f2).unwrap(), "reproducible");
    if !have("tar") {
        eprintln!("skipped: no system tar");
        return;
    }
    let x = t.path().join("x");
    std::fs::create_dir(&x).unwrap();
    let st = Command::new("tar")
        .args(["-xf", f1.to_str().unwrap(), "-C", x.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(st.success());
    check_tree(&x);
    let mtime = std::fs::metadata(x.join("a.txt")).unwrap().modified().unwrap();
    assert_eq!(mtime.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(), 1_700_000_000);
    let list = Command::new("tar").args(["-tf", f1.to_str().unwrap()]).output().unwrap();
    assert!(String::from_utf8_lossy(&list.stdout).contains("src/deep/b.txt"));
}

#[cfg(unix)]
#[test]
fn zip_roundtrip_prefix_and_errors() {
    let (t, d) = repo();
    let f = t.path().join("out.zip");
    ok(&d, &["archive", "-o", f.to_str().unwrap()]);
    let f2 = t.path().join("out2.zip");
    ok(&d, &["archive", "-o", f2.to_str().unwrap()]);
    assert_eq!(std::fs::read(&f).unwrap(), std::fs::read(&f2).unwrap());
    if have("unzip") {
        let x = t.path().join("x");
        std::fs::create_dir(&x).unwrap();
        let st = Command::new("unzip")
            .args(["-q", f.to_str().unwrap(), "-d", x.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(st.success());
        check_tree(&x);
        let t = Command::new("unzip").args(["-tq", f.to_str().unwrap()]).status().unwrap();
        assert!(t.success(), "unzip -t verifies crc");
    } else {
        eprintln!("skipped: no system unzip");
    }
    let p = t.path().join("p.tar");
    ok(&d, &["archive", "--prefix", "proj-1/", "-o", p.to_str().unwrap()]);
    if have("tar") {
        let l = Command::new("tar").args(["-tf", p.to_str().unwrap()]).output().unwrap();
        assert!(String::from_utf8_lossy(&l.stdout).lines().all(|n| n.starts_with("proj-1/")));
    }
    assert!(!vcs(&d, &["archive", "-o", t.path().join("x.rar").to_str().unwrap()])
        .status
        .success());
    assert!(!vcs(&d, &["archive", "nosuchrev", "-o", f.to_str().unwrap()]).status.success());
}
