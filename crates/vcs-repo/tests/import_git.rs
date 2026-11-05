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
    assert!(rep.refs_ignored.is_empty());
    assert!(rep.paths_skipped.is_empty() && rep.submodules.is_empty());
    assert_eq!((rep.symlinks, rep.tags_imported), (1, 1));
    let tags = r.tag_list("root").unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].name, "v1");
    assert_eq!(tags[0].annotation.as_ref().unwrap().tagger.name, "T");

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
    assert!(!f.contains_key("bin/run.sh"));
    assert_eq!(f["link"], b"hello\n"); // symlink target bytes

    let feat = refs.head("root", "feat").unwrap();
    let f = files_at(&r, &feat);
    assert_eq!(
        f.keys().cloned().collect::<Vec<_>>(),
        ["bin/run.sh", "moved/hello.txt", "sp ace/q\"t.txt"]
    );

    let f = files_at(&r, &main);
    assert_eq!(
        f.keys().cloned().collect::<Vec<_>>(),
        ["hello.txt", "link", "merged.txt", "sp ace/q\"t.txt"]
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
    assert_eq!((rep2.tags_imported, rep2.tags_skipped.len()), (0, 0));
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

#[test]
fn symlink_mode_is_recorded_and_submodules_are_reported() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let s = "blob\nmark :1\ndata 6\ntarget\n\
commit refs/heads/main\nmark :2\noriginal-oid c1\ncommitter A <a@x> 1 +0000\ndata 1\n1\n\
M 120000 :1 dir/ln\nM 160000 0123456789012345678901234567890123456789 vendor/sub\nM 100644 inline f\ndata 2\nf\n\n\
reset refs/tags/light\nfrom :2\n\
tag ann\nfrom :2\ntagger T <t@x> 9 +0000\ndata 4\nrel\n\
tag bad..name\nfrom :2\ntagger T <t@x> 9 +0000\ndata 1\nx\n\
done\n";
    let rep = r.import_git(s.as_bytes()).unwrap();
    assert_eq!(rep.submodules, vec!["vendor/sub".to_string()]);
    assert_eq!(rep.symlinks, 1);
    assert_eq!(rep.tags_imported, 2);
    assert_eq!(rep.tags_skipped.len(), 1, "{:?}", rep.tags_skipped);
    let tags = r.tag_list("root").unwrap();
    let names: Vec<_> = tags.iter().map(|t| (t.name.as_str(), t.annotation.is_some())).collect();
    assert_eq!(names, [("ann", true), ("light", false)]);
    assert_eq!(tags[0].annotation.as_ref().unwrap().message, "rel\n");
    let main = r.refs().unwrap().head("root", "main").unwrap();
    assert_eq!(tags[0].commit, main);
    let vcs_objects::Object::Commit(mc) = r.store().get(&main).unwrap() else {
        panic!()
    };
    let root = mc.root;
    let vcs_objects::Object::Tree(top) = r.store().get(&root).unwrap() else {
        panic!()
    };
    assert!(top.entries().iter().all(|e| e.name != "vendor"));
    let dir = top.entries().iter().find(|e| e.name == "dir").unwrap();
    let vcs_objects::Object::Tree(sub) = r.store().get(&dir.id).unwrap() else {
        panic!()
    };
    assert_eq!(sub.entries()[0].mode, vcs_objects::EntryMode::Symlink);
    assert!(std::fs::symlink_metadata(r.worktree().join("dir/ln"))
        .unwrap()
        .file_type()
        .is_symlink());
    // re-import is a no-op for tags
    let rep2 = r.import_git(s.as_bytes()).unwrap();
    assert_eq!(rep2.tags_imported, 0);
    assert_eq!(r.tag_list("root").unwrap().len(), 2);
}

/// The incremental per-commit tree edit must give the tree the flat rebuild gives: a stream that edits a big tree
/// in place (modify, delete a file, delete a directory, rename a directory, copy, file <-> directory) ends in the
/// same root tree id as one root commit that states the final files.
#[test]
fn incremental_commit_edits_match_a_flat_rebuild() {
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    for d in 0..6 {
        for s in 0..6 {
            for f in 0..12 {
                files.insert(format!("d{d}/s{s}/f{f}.txt"), format!("body {d} {s} {f}\n"));
            }
        }
    }
    fn modify(out: &mut String, path: &str, body: &str) {
        out.push_str(&format!("M 100644 inline {path}\ndata {}\n{body}\n", body.len()));
    }
    fn commit(out: &mut String, mark: u32, from: Option<u32>, ops: &dyn Fn(&mut String)) {
        out.push_str(&format!(
            "commit refs/heads/main\nmark :{mark}\ncommitter C <c@x> {mark}000 +0000\ndata 2\nc\n"
        ));
        if let Some(f) = from {
            out.push_str(&format!("from :{f}\n"));
        }
        ops(out);
        out.push('\n');
    }
    let mut s1 = String::from("feature done\n");
    commit(&mut s1, 1, None, &|o| {
        for (p, b) in &files {
            modify(o, p, b);
        }
    });
    commit(&mut s1, 2, Some(1), &|o| {
        modify(o, "d0/s0/f0.txt", "edited\n");
        modify(o, "d0/s0/new.txt", "new\n");
        o.push_str("D d1/s1/f3.txt\n");
        o.push_str("D d2/s2\n"); // a whole directory
        o.push_str("R d3/s3 d3/moved\n"); // directory rename
        o.push_str("C d4/s4 d5/copy4\n"); // directory copy
        o.push_str("R d0/s1/f1.txt top.txt\n");
        modify(o, "d5/s0", "now a file\n"); // file replaces a directory
        modify(o, "d4/s5/f0.txt/deep.txt", "dir replaces a file\n");
        o.push_str("D no/such/path\n");
    });
    s1.push_str("done\n");

    files.insert("d0/s0/f0.txt".into(), "edited\n".into());
    files.insert("d0/s0/new.txt".into(), "new\n".into());
    files.remove("d1/s1/f3.txt");
    files.retain(|k, _| !k.starts_with("d2/s2/"));
    let moved: Vec<_> = files.iter().filter(|(k, _)| k.starts_with("d3/s3/")).map(|(k, v)| (k.clone(), v.clone())).collect();
    for (k, v) in moved {
        files.remove(&k);
        files.insert(k.replacen("d3/s3/", "d3/moved/", 1), v);
    }
    let copied: Vec<_> = files.iter().filter(|(k, _)| k.starts_with("d4/s4/")).map(|(k, v)| (k.clone(), v.clone())).collect();
    for (k, v) in copied {
        files.insert(k.replacen("d4/s4/", "d5/copy4/", 1), v);
    }
    let v = files.remove("d0/s1/f1.txt").unwrap();
    files.insert("top.txt".into(), v);
    files.retain(|k, _| !k.starts_with("d5/s0/"));
    files.insert("d5/s0".into(), "now a file\n".into());
    files.remove("d4/s5/f0.txt");
    files.insert("d4/s5/f0.txt/deep.txt".into(), "dir replaces a file\n".into());

    let mut s2 = String::from("feature done\n");
    commit(&mut s2, 1, None, &|o| {
        for (p, b) in &files {
            modify(o, p, b);
        }
    });
    s2.push_str("done\n");

    let d = tempfile::tempdir().unwrap();
    let a = Repo::init(&d.path().join("a")).unwrap();
    a.import_git(s1.as_bytes()).unwrap();
    let b = Repo::init(&d.path().join("b")).unwrap();
    b.import_git(s2.as_bytes()).unwrap();
    let head = |r: &Repo| r.refs().unwrap().head("root", "main").unwrap();
    let (ha, hb) = (head(&a), head(&b));
    let got = files_at(&a, &ha);
    assert_eq!(got.len(), files.len());
    for (k, v) in &files {
        assert_eq!(got[k], v.as_bytes(), "{k}");
    }
    let root = |r: &Repo, id| match r.store().get(id).unwrap() {
        vcs_objects::Object::Commit(c) => c.root,
        _ => panic!(),
    };
    assert_eq!(root(&a, &ha), root(&b, &hb), "incremental edits must give the canonical tree");
}
