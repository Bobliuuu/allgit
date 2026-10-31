//! Fixture streams are hand-written in fast-export syntax; git is never run.
use std::collections::BTreeMap;
use std::fs;
use vcs_repo::Repo;

const STREAM: &str = "\
feature done
blob
mark :1
original-oid aaaa000000000000000000000000000000000001
data 6
hello
blob
mark :2
original-oid aaaa000000000000000000000000000000000002
data 7
script
commit refs/heads/main
mark :3
original-oid bbbb000000000000000000000000000000000001
author Ann <ann@x> 1000 +0000
committer Ann <ann@x> 1000 +0000
data 8
initial
M 100644 :1 hello.txt
M 100755 :2 bin/run.sh
M 100644 :1 \"sp ace/q\\\"t.txt\"

commit refs/heads/main
mark :4
original-oid bbbb000000000000000000000000000000000002
author Ann <ann@x> 2000 +0000
committer Bob <bob@x> 2001 +0000
data 7
second
from :3
M 100644 inline hello.txt
data 6
HELLO
D bin/run.sh
M 120000 :1 link

commit refs/heads/feat
mark :5
original-oid bbbb000000000000000000000000000000000003
committer Cy <cy@x> 3000 +0000
data 5
side
from :3
R hello.txt moved/hello.txt

commit refs/heads/main
mark :6
original-oid bbbb000000000000000000000000000000000004
committer Ann <ann@x> 4000 +0000
data 6
merge
from :4
merge :5
M 100644 :2 merged.txt

tag v1
from :6
tagger T <t@x> 5000 +0000
data 2
t
done
";

fn files_at(r: &Repo, id: &vcs_objects::ObjectId) -> BTreeMap<String, Vec<u8>> {
    let c = match r.store().get(id).unwrap() {
        vcs_objects::Object::Commit(c) => c,
        _ => panic!(),
    };
    let mut out = BTreeMap::new();
    walk(r, &c.root, "", &mut out);
    out
}
fn walk(r: &Repo, t: &vcs_objects::ObjectId, pre: &str, out: &mut BTreeMap<String, Vec<u8>>) {
    let vcs_objects::Object::Tree(t) = r.store().get(t).unwrap() else {
        panic!()
    };
    for e in t.entries() {
        let p = format!("{pre}{}", e.name);
        if e.mode == vcs_objects::EntryMode::Directory {
            walk(r, &e.id, &format!("{p}/"), out);
        } else {
            out.insert(p, r.store().get_blob(&e.id).unwrap());
        }
    }
}

#[test]
fn imports_every_file_at_every_commit_and_reimport_is_noop() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let rep = r.import_git(STREAM.as_bytes()).unwrap();
    assert_eq!(rep.commits_imported, 4);
    assert_eq!(rep.blobs, 2);
    assert_eq!(rep.refs_ignored, vec!["v1".to_string()]);
    assert_eq!(rep.paths_skipped.len(), 1);

    let refs = r.refs().unwrap();
    let main = refs.head("root", "main").unwrap();
    let log = r.log("root", Some("main"), 10).unwrap();
    assert_eq!(log.len(), 4);
    assert_eq!(log.last().unwrap().1.message, "initial\n");

    let c3 = &log.iter().find(|(_, c)| c.message == "initial\n").unwrap().0;
    let f = files_at(&r, c3);
    assert_eq!(
        f.keys().cloned().collect::<Vec<_>>(),
        ["bin/run.sh", "hello.txt", "sp ace/q\"t.txt"]
    );
    assert_eq!(f["hello.txt"], b"hello\n");
    assert_eq!(f["bin/run.sh"], b"script\n");

    let c4 = &log.iter().find(|(_, c)| c.message == "second\n").unwrap();
    assert_eq!(c4.1.author.name, "Ann");
    let f = files_at(&r, &c4.0);
    assert_eq!(f["hello.txt"], b"HELLO\n");
    assert!(!f.contains_key("bin/run.sh") && !f.contains_key("link"));

    let feat = refs.head("root", "feat").unwrap();
    let f = files_at(&r, &feat);
    assert_eq!(
        f.keys().cloned().collect::<Vec<_>>(),
        ["bin/run.sh", "moved/hello.txt", "sp ace/q\"t.txt"]
    );

    let f = files_at(&r, &main);
    assert_eq!(
        f.keys().cloned().collect::<Vec<_>>(),
        ["hello.txt", "merged.txt", "sp ace/q\"t.txt"]
    ); // tree starts from the first parent
    let mc = match r.store().get(&main).unwrap() {
        vcs_objects::Object::Commit(c) => c,
        _ => panic!(),
    };
    assert_eq!(mc.parents.len(), 2);

    // working tree materialised for the unborn checked-out branch
    assert_eq!(fs::read(r.worktree().join("merged.txt")).unwrap(), b"script\n");
    let st = r.status().unwrap();
    assert!(st.untracked.is_empty() && st.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty()));
    assert!(r.fsck().unwrap().problems.is_empty());

    // re-import: nothing new
    let before: std::collections::BTreeSet<_> = r.store().list().unwrap().into_iter().collect();
    let rep2 = r.import_git(STREAM.as_bytes()).unwrap();
    assert_eq!(rep2.commits_imported, 0);
    assert_eq!(rep2.commits_skipped, 4);
    assert_eq!(rep2.objects_written, 0);
    assert!(rep2.branches_updated.is_empty());
    let after: std::collections::BTreeSet<_> = r.store().list().unwrap().into_iter().collect();
    assert_eq!(before, after);
    assert_eq!(r.refs().unwrap().head("root", "main"), Some(main));
}

#[test]
fn incremental_import_extends_history() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let first = "blob\nmark :1\ndata 2\na\ncommit refs/heads/work\nmark :2\noriginal-oid c1\ncommitter A <a@x> 1 +0000\ndata 1\n1\nM 100644 :1 a\n\n";
    r.import_git(first.as_bytes()).unwrap();
    let second = format!(
        "{first}blob\nmark :3\ndata 2\nb\ncommit refs/heads/work\nmark :4\noriginal-oid c2\ncommitter A <a@x> 2 +0000\ndata 1\n2\nfrom :2\nM 100644 :3 b\n"
    );
    let rep = r.import_git(second.as_bytes()).unwrap();
    assert_eq!((rep.commits_imported, rep.commits_skipped), (1, 1));
    assert_eq!(r.log("root", Some("work"), 10).unwrap().len(), 2);
}

#[test]
fn malformed_streams_are_errors() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    assert!(r.import_git("blob\ndata 10\nabc".as_bytes()).is_err());
    assert!(r.import_git("bogus\n".as_bytes()).is_err());
    assert!(r
        .import_git("commit refs/heads/x\ncommitter A <a@x> 1 +0000\ndata 1\nm\nfrom :9\n".as_bytes())
        .is_err());
    assert!(r.refs().unwrap().branches.is_empty());
}
