//! Fixtures under tests/fixtures/p4 are hand-written from Perforce's documented output formats
//! (the -G ones generated with Python's marshal). No p4 server or binary is ever involved.
use std::collections::BTreeMap;
use std::path::PathBuf;
use vcs_objects::{EntryMode, Object, ObjectId};
use vcs_repo::{P4View, Repo};

fn fx(p: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/p4").join(p)
}

fn files_at(r: &Repo, id: &ObjectId) -> BTreeMap<String, (EntryMode, Vec<u8>)> {
    let Object::Commit(c) = r.store().get(id).unwrap() else { panic!() };
    let mut out = BTreeMap::new();
    walk(r, &c.root, "", &mut out);
    out
}
fn walk(r: &Repo, t: &ObjectId, pre: &str, out: &mut BTreeMap<String, (EntryMode, Vec<u8>)>) {
    let Object::Tree(t) = r.store().get(t).unwrap() else { panic!() };
    for e in t.entries() {
        let p = format!("{pre}{}", e.name);
        if e.mode == EntryMode::Directory {
            walk(r, &e.id, &format!("{p}/"), out);
        } else {
            out.insert(p, (e.mode, r.store().get_blob(&e.id).unwrap()));
        }
    }
}

fn view() -> P4View {
    P4View::parse(&std::fs::read_to_string(fx("basic.view")).unwrap()).unwrap()
}

#[test]
fn imports_changelists_as_commits_and_reimport_is_noop() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let rep = r.import_p4(&fx("basic"), &view(), "main").unwrap();
    assert_eq!(rep.changes_imported, 4, "{rep:?}");
    assert_eq!(rep.changes_not_imported, vec![104]); // nothing inside the view; 105 is pending
    let log = r.log("root", Some("main"), 10).unwrap();
    assert_eq!(log.len(), 4);
    let by = |n: u64| {
        log.iter()
            .find(|(_, c)| c.message.contains(&format!("Perforce-Change: {n}\n")))
            .unwrap()
    };
    assert_eq!(by(100).1.message, "Initial import of proj\n\nPerforce-Change: 100\n");
    assert_eq!(by(100).1.author.name, "alice");
    assert_eq!(by(100).1.author.timestamp, 1709164800);
    assert_eq!(by(101).1.author.timestamp, 1709285400); // 2024-03-01 09:30:00 UTC from describe text
    assert_eq!(
        by(103).1.message,
        "Integrate from release branch\nand drop the old link\n\nPerforce-Change: 103\n"
    );

    let f = files_at(&r, &by(100).0);
    assert_eq!(
        f.keys().cloned().collect::<Vec<_>>(),
        [
            "app/README.txt",
            "app/bin/run.sh",
            "app/latest",
            "app/src/main.c",
            "scripts/build.sh"
        ]
    );
    assert_eq!(f["app/bin/run.sh"].0, EntryMode::Executable);
    assert_eq!(f["scripts/build.sh"].0, EntryMode::Executable);
    assert_eq!(f["app/latest"], (EntryMode::Symlink, b"README.txt".to_vec()));
    assert_eq!(f["app/README.txt"], (EntryMode::File, b"Proj\n".to_vec()));

    let f = files_at(&r, &by(101).0);
    assert_eq!(f["app/src/main.c"].1, b"int main(void){return 1;}\n");
    assert!(f.contains_key("app/src/util.c"));

    let f = files_at(&r, &by(102).0);
    assert!(!f.contains_key("app/src/util.c") && f.contains_key("app/src/lib/util.c"));

    let head = r.refs().unwrap().head("root", "main").unwrap();
    assert_eq!(head, by(103).0);
    let f = files_at(&r, &head);
    assert!(!f.contains_key("app/latest"));
    assert_eq!(f["app/README.txt"].1, b"Proj 2\n");
    assert_eq!(f["app/NEW.txt"].1, b"branched\n");
    assert_eq!(std::fs::read(r.worktree().join("app/NEW.txt")).unwrap(), b"branched\n");
    assert!(r.fsck().unwrap().problems.is_empty());

    let before: std::collections::BTreeSet<_> = r.store().list().unwrap().into_iter().collect();
    let rep2 = r.import_p4(&fx("basic"), &view(), "main").unwrap();
    assert_eq!((rep2.changes_imported, rep2.changes_skipped), (0, 4));
    let after: std::collections::BTreeSet<_> = r.store().list().unwrap().into_iter().collect();
    assert_eq!(before, after);
}

#[test]
fn incremental_import_adds_only_new_changes() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("fx");
    copy_dir(&fx("basic"), &src);
    let full = std::fs::read_to_string(src.join("changes.txt")).unwrap();
    // only changes 100 and 101 are known at first
    let r = Repo::init(&d.path().join("r")).unwrap();
    std::fs::write(
        src.join("changes.txt"),
        "Change 101 on 2024/03/01 by bob@ws2\n\nChange 100 on 2024/02/29 by alice@ws1\n",
    )
    .unwrap();
    let rep = r.import_p4(&src, &view(), "p4").unwrap();
    assert_eq!(rep.changes_imported, 2);
    std::fs::write(src.join("changes.txt"), full).unwrap();
    let rep = r.import_p4(&src, &view(), "p4").unwrap();
    assert_eq!((rep.changes_imported, rep.changes_skipped), (2, 2));
    let log = r.log("root", Some("p4"), 10).unwrap();
    assert_eq!(log.len(), 4);
    // linear: each commit has exactly one parent except the first
    assert_eq!(log.iter().filter(|(_, c)| c.parents.is_empty()).count(), 1);
}

fn copy_dir(a: &std::path::Path, b: &std::path::Path) {
    std::fs::create_dir_all(b).unwrap();
    for e in std::fs::read_dir(a).unwrap() {
        let e = e.unwrap();
        let t = b.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), t).unwrap();
        }
    }
}

#[test]
fn view_rules() {
    let v = P4View::parse("//d/a/... x/...\n-//d/a/s/...\n//d/a/s/keep //client/k.txt\n//d/z/... ...\n").unwrap();
    assert_eq!(v.map("//d/a/f.c").as_deref(), Some("x/f.c"));
    assert_eq!(v.map("//d/a/s/f.c"), None);
    assert_eq!(v.map("//d/a/s/keep").as_deref(), None.or(Some("k.txt")));
    assert_eq!(v.map("//d/z/q/r").as_deref(), Some("q/r"));
    assert_eq!(v.map("//d/other"), None);
    assert!(P4View::parse("//d/*/x y\n").is_err());
    assert!(P4View::parse("//d/a/... y\n").is_err());
    assert!(P4View::parse("").is_err());
}

#[test]
fn missing_print_and_bad_inputs_are_errors() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("fx");
    copy_dir(&fx("basic"), &src);
    std::fs::remove_file(src.join("print/101/depot/proj/src/util.c")).unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    assert!(r.import_p4(&src, &view(), "main").is_err());
    // truncated marshal file
    let m = std::fs::read(src.join("describe/100.marshal")).unwrap();
    std::fs::write(src.join("describe/100.marshal"), &m[..m.len() / 2]).unwrap();
    assert!(r.import_p4(&src, &view(), "main").is_err());
    // a traversing depot path maps to an invalid zone path and is skipped, never read
    std::fs::write(
        src.join("describe/101.txt"),
        "Change 101 by bob@ws2 on 2024/03/01 09:30:00\n\n\tx\n\nAffected files ...\n\n... //depot/proj/../../evil#1 add\n",
    )
    .unwrap();
    std::fs::copy(fx("basic/describe/100.marshal"), src.join("describe/100.marshal")).unwrap();
    let rep = r.import_p4(&src, &view(), "main").unwrap();
    assert!(rep.paths_skipped.iter().any(|p| p.contains("evil")), "{rep:?}");
}
