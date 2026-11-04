//! Fixtures under tests/fixtures/p4real were captured from a REAL Helix Core server (p4d 2026.1, p4 2025.2,
//! `scripts/p4_capture.sh`): `p4 -G describe -s`, `p4 changes -l`, `p4 print -q`. `expected/N.manifest` is the
//! ground truth: the tree that `p4 sync //depot/...@N` produced in an empty workspace (mode, sha256, size / link target).
//! The test imports the capture and compares the tree of every changelist with its manifest.
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use vcs_objects::{EntryMode, Object, ObjectId};
use vcs_repo::{P4View, Repo};

fn fx(p: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(p)
}

fn manifest_of(r: &Repo, id: &ObjectId) -> Vec<String> {
    let Object::Commit(c) = r.store().get(id).unwrap() else { panic!() };
    let mut out = BTreeMap::new();
    walk(r, &c.root, "", &mut out);
    out.into_values().collect()
}
fn walk(r: &Repo, t: &ObjectId, pre: &str, out: &mut BTreeMap<String, String>) {
    let Object::Tree(t) = r.store().get(t).unwrap() else { panic!() };
    for e in t.entries() {
        let p = format!("{pre}{}", e.name);
        match e.mode {
            EntryMode::Directory => walk(r, &e.id, &format!("{p}/"), out),
            EntryMode::Symlink => {
                let b = r.store().get_blob(&e.id).unwrap();
                out.insert(p.clone(), format!("L {p} {}", String::from_utf8(b).unwrap()));
            }
            m => {
                let b = r.store().get_blob(&e.id).unwrap();
                let mode = if m == EntryMode::Executable { "755" } else { "644" };
                let h: String = Sha256::digest(&b).iter().map(|x| format!("{x:02x}")).collect();
                out.insert(p.clone(), format!("{mode} {p} {h} {}", b.len()));
            }
        }
    }
}

/// Path of a manifest line ("<mode> <path> <sha256> <size>" or "L <path> <target>"); paths may contain spaces.
fn path_of(l: &str) -> &str {
    let t: Vec<&str> = l.splitn(2, ' ').collect();
    let rest = t[1];
    let cut = if t[0] == "L" { 1 } else { 2 };
    let mut end = rest.len();
    for _ in 0..cut {
        end = rest[..end].rfind(' ').unwrap();
    }
    &rest[..end]
}

fn expected(dir: &Path, n: u64) -> Vec<String> {
    let t = std::fs::read_to_string(dir.join(format!("expected/{n}.manifest"))).unwrap();
    let mut v: Vec<String> = t.lines().map(str::to_owned).collect();
    v.sort_by(|a, b| path_of(a).cmp(path_of(b)));
    v
}

fn commit_for(r: &Repo, n: u64) -> ObjectId {
    r.log("root", Some("main"), 100)
        .unwrap()
        .into_iter()
        .find(|(_, c)| c.message.ends_with(&format!("Perforce-Change: {n}\n")))
        .unwrap_or_else(|| panic!("no commit for change {n}"))
        .0
}

/// Known discrepancies (documented in docs/research/perforce-benchmark.md); skipped by the tree comparison:
/// * depot paths with p4-escaped characters (`%40` `%23` `%25` `%2A`): decoded since the p4-escaped-paths fix, and
///   checked by `real_escaped_names_are_decoded`,
/// (`utf16` content is converted back to UTF-16LE with BOM by the importer, issue #302.)
fn known_diff(l: &str) -> bool {
    let p = path_of(l);
    p.contains("we@ird") || p.contains("we%40ird")
}

fn check_all(dir: &Path, typed: bool) -> (Repo, tempfile::TempDir) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let view = P4View::parse(&std::fs::read_to_string(fx("p4real.view")).unwrap()).unwrap();
    let rep = r.import_p4(dir, &view, "main").unwrap();
    assert_eq!(rep.changes_imported, 10, "{rep:?}");
    for n in 1..=10u64 {
        let mut got = manifest_of(&r, &commit_for(&r, n));
        got.retain(|l| !known_diff(l));
        got.sort_by(|a, b| path_of(a).cmp(path_of(b)));
        let mut want = expected(&fx("p4real"), n);
        want.retain(|l| !known_diff(l));
        if typed {
            assert_eq!(got, want, "tree of change {n} differs from `p4 sync @{n}`");
        } else {
            // text-form describe has no file types: only the paths can match
            let paths = |v: &[String]| v.iter().map(|l| path_of(l).to_owned()).collect::<Vec<_>>();
            assert_eq!(paths(&got), paths(&want), "paths of change {n}");
        }
    }
    (r, d)
}

#[test]
fn real_metadata_matches() {
    let (r, _d) = check_all(&fx("p4real"), true);
    let log = r.log("root", Some("main"), 100).unwrap();
    assert_eq!(log.len(), 10); // change 11 is pending and ignored
    let c1 = &log.iter().find(|(_, c)| c.message.ends_with("Perforce-Change: 1\n")).unwrap().1;
    assert_eq!(
        c1.message,
        "Initial import of proj\nwith a second description line\n\nPerforce-Change: 1\n"
    );
    assert_eq!((c1.author.name.as_str(), c1.author.email.as_str()), ("alice", "alice@perforce"));
    assert_eq!(c1.author.timestamp, 1793500238); // `time` field of the real p4 -G describe
    let c2 = &log.iter().find(|(_, c)| c.message.ends_with("Perforce-Change: 2\n")).unwrap().1;
    assert_eq!(c2.author.name, "bob");
}

/// Ground truth for the escaped-name discrepancy: `p4 sync` wrote `we@ird#na%me*.txt`.
#[test]
fn real_escaped_names_are_decoded() {
    let (r, _d) = {
        let d = tempfile::tempdir().unwrap();
        let r = Repo::init(&d.path().join("r")).unwrap();
        let view = P4View::parse(&std::fs::read_to_string(fx("p4real.view")).unwrap()).unwrap();
        r.import_p4(&fx("p4real"), &view, "main").unwrap();
        (r, d)
    };
    let got = manifest_of(&r, &commit_for(&r, 10));
    assert!(got.iter().any(|l| path_of(l) == "proj/we@ird#na%me*.txt"), "{got:?}");
}

#[test]
fn real_marshal_output_matches_p4_sync_at_every_change() {
    let _ = check_all(&fx("p4real"), true);
}

#[test]
fn real_text_describe_imports_same_paths_without_types() {
    // copy the capture with the text describes in place of the marshalled ones
    let d = tempfile::tempdir().unwrap();
    let dst = d.path().join("txt");
    std::fs::create_dir_all(dst.join("describe")).unwrap();
    let src = fx("p4real");
    std::fs::copy(src.join("changes.txt"), dst.join("changes.txt")).unwrap();
    for e in std::fs::read_dir(src.join("describe-text")).unwrap() {
        let e = e.unwrap();
        std::fs::copy(e.path(), dst.join("describe").join(e.file_name())).unwrap();
    }
    copy_dir(&src.join("print"), &dst.join("print"));
    let (r, _d2) = check_all(&dst, false);
    // timestamps from the text form (server time, here UTC) equal the epoch `time` of the marshalled form
    let log = r.log("root", Some("main"), 100).unwrap();
    let c1 = &log.iter().find(|(_, c)| c.message.ends_with("Perforce-Change: 1\n")).unwrap().1;
    assert_eq!(c1.author.timestamp, 1793500238);
}

fn copy_dir(a: &Path, b: &Path) {
    std::fs::create_dir_all(b).unwrap();
    for e in std::fs::read_dir(a).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &b.join(e.file_name()));
        } else {
            std::fs::copy(e.path(), b.join(e.file_name())).unwrap();
        }
    }
}
