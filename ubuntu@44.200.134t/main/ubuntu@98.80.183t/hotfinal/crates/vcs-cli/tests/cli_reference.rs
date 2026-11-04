//! The committed command reference (docs/guide/12-command-reference.md) must match `vcs --help`.
//! Fix a failure with: VCS_BIN=target/release/vcs scripts/gen_cli_reference.sh

#[cfg(unix)]
#[test]
fn committed_command_reference_matches_the_binary() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = std::process::Command::new("sh")
        .arg(root.join("scripts/gen_cli_reference.sh"))
        .arg("--check")
        .env("VCS_BIN", env!("CARGO_BIN_EXE_vcs"))
        .output()
        .expect("run sh");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
