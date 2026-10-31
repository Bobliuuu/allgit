//! `vcs import-p4` over the hand-written fixtures (no p4 binary or server involved).
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn import_p4_cli() {
    let fx = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vcs-repo/tests/fixtures/p4");
    let t = tempfile::tempdir().unwrap();
    let run = |dir: &Path, args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    run(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    let out = run(
        &d,
        &[
            "import-p4",
            fx.join("basic").to_str().unwrap(),
            "--view",
            fx.join("basic.view").to_str().unwrap(),
        ],
    );
    assert!(out.contains("imported 4 changelist(s)"), "{out}");
    assert!(out.contains("change 104"), "{out}");
    assert_eq!(std::fs::read(d.join("app/NEW.txt")).unwrap(), b"branched\n");
    assert!(run(&d, &["log"]).contains("Perforce-Change: 103"));
}
