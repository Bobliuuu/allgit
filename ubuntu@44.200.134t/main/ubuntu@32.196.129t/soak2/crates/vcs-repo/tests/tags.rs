//! Tags, revision lookup, show and blame.
use std::fs;
use vcs_objects::{ObjectId, Signature};
use vcs_repo::Repo;

fn sig(t: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: t,
    }
}
fn setup() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    (d, r)
}
fn commit(r: &Repo, p: &str, content: &str, msg: &str, t: u64) -> ObjectId {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, content).unwrap();
    r.add(&["".into()]).unwrap();
    r.commit(msg, sig(t)).unwrap().commits[0].2
}
fn descs(r: &Repo) -> Vec<String> {
    r.op_log(100).unwrap().into_iter().map(|(_, o)| o.description).collect()
}

#[test]
fn lightweight_and_annotated_tags_list_delete_and_log_ops() {
    let (_d, r) = setup();
    let c1 = commit(&r, "f", "1\n", "one", 1);
    let c2 = commit(&r, "f", "2\n", "two", 2);
    assert_eq!(r.tag_create("root", "light", Some("HEAD~1"), None).unwrap(), c1);
    assert_eq!(
        r.tag_create("root", "v1", None, Some(("release 1\n", sig(5), b"sig".to_vec())))
            .unwrap(),
        c2
    );
    let tags = r.tag_list("root").unwrap();
    assert_eq!(tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["light", "v1"]);
    assert!(tags[0].annotation.is_none() && tags[0].commit == c1);
    let a = tags[1].annotation.as_ref().unwrap();
    assert_eq!(
        (a.message.as_str(), a.signature.as_slice(), a.target),
        ("release 1\n", &b"sig"[..], c2)
    );
    // tags resolve as revisions, annotated ones peel to the commit
    assert_eq!(r.resolve_rev("root", "v1").unwrap(), c2);
    assert_eq!(r.resolve_rev("root", "v1~1").unwrap(), c1);
    assert_eq!(r.resolve_rev("root", "light").unwrap(), c1);
    assert_eq!(r.resolve_rev("root", &c2.to_hex()[..8]).unwrap(), c2);
    assert_eq!(r.tag_delete("root", "light").unwrap(), c1);
    assert_eq!(r.tag_list("root").unwrap().len(), 1);
    let d = descs(&r);
    assert!(d[0].starts_with("tag delete root/light"), "{d:?}");
    assert!(d[1].starts_with("tag create root/v1 (annotated)"), "{d:?}");
    assert!(d[2].starts_with("tag create root/light"), "{d:?}");
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}

#[test]
fn tag_refusals() {
    let (_d, r) = setup();
    assert!(r
        .tag_create("root", "v1", None, None)
        .unwrap_err()
        .to_string()
        .contains("no commits"));
    commit(&r, "f", "1\n", "one", 1);
    r.tag_create("root", "v1", None, None).unwrap();
    let e = r.tag_create("root", "v1", None, None).unwrap_err().to_string();
    assert!(e.contains("already exists"), "{e}");
    assert!(r.tag_create("root", "bad..name", None, None).is_err());
    assert!(r.tag_create("root", "v2", Some("nope"), None).is_err());
    assert!(r.tag_create("root", "v2", Some("HEAD~5"), None).is_err());
    assert!(r
        .tag_create("ghost", "v2", None, None)
        .unwrap_err()
        .to_string()
        .contains("no such zone"));
    assert!(r.tag_delete("root", "missing").unwrap_err().to_string().contains("no such tag"));
    assert!(r.tag_create("root", "big", None, Some(("m", sig(1), vec![0; 5000]))).is_err());
    // nothing was recorded for refused operations
    assert_eq!(r.tag_list("root").unwrap().len(), 1);
}

#[test]
fn tags_survive_reopen_and_pin_zone_delete() {
    let (_d, r) = setup();
    r.create_zone("engine", "engine").unwrap();
    commit(&r, "engine/a", "1\n", "one", 1);
    r.tag_create("engine", "v1", None, Some(("m", sig(2), vec![]))).unwrap();
    let r2 = Repo::discover(r.worktree()).unwrap();
    assert_eq!(r2.tag_list("engine").unwrap().len(), 1);
    assert!(r2.tag_list("root").unwrap().is_empty()); // tags are per zone
    r.zone_remove("engine").unwrap();
    let e = r.zone_delete("engine", false).unwrap_err().to_string();
    assert!(e.contains("1 tag(s)"), "{e}");
    r.zone_delete("engine", true).unwrap();
}

#[test]
fn tag_object_is_kept_by_gc() {
    let (_d, r) = setup();
    commit(&r, "f", "1\n", "one", 1);
    r.tag_create("root", "v1", None, Some(("m", sig(2), vec![]))).unwrap();
    // an operation view does not hold tags, so only the tag ref keeps the object alive
    let rep = r
        .gc_plan_at(
            std::time::SystemTime::now() + std::time::Duration::from_secs(10_000),
            std::time::Duration::ZERO,
        )
        .unwrap();
    assert!(rep.would_delete.is_empty(), "{:?}", rep.would_delete);
}

#[test]
fn show_prints_metadata_and_first_parent_diff() {
    let (_d, r) = setup();
    let c1 = commit(&r, "f", "a\nb\n", "first", 1);
    let c2 = commit(&r, "f", "a\nB\n", "second", 2);
    let s = r.show("root", "HEAD").unwrap();
    assert_eq!(s.id, c2);
    assert_eq!(s.commit.message, "second");
    assert!(s.diff.contains("-b\n") && s.diff.contains("+B\n"), "{}", s.diff);
    let root = r.show("root", &c1.to_hex()[..10]).unwrap();
    assert!(root.diff.contains("new file") && root.diff.contains("+a\n"), "{}", root.diff);
    r.tag_create("root", "v1", Some("HEAD~1"), Some(("first release", sig(3), vec![])))
        .unwrap();
    let t = r.show("root", "v1").unwrap();
    assert_eq!(t.id, c1);
    assert_eq!(t.tag.unwrap().message, "first release");
    assert!(r.show("root", "nope").is_err());
}

#[test]
fn show_uses_repo_paths_in_a_zone() {
    let (_d, r) = setup();
    r.create_zone("engine", "engine").unwrap();
    commit(&r, "engine/a.rs", "x\n", "init", 1);
    let s = r.show("engine", "HEAD").unwrap();
    assert!(s.diff.contains("a/engine/a.rs"), "{}", s.diff);
}

#[test]
fn blame_matches_a_hand_built_history() {
    let (_d, r) = setup();
    // c1: A B C
    let c1 = commit(&r, "f.txt", "A\nB\nC\n", "c1", 1);
    // c2: A B' C D   (B changed, D added)
    let c2 = commit(&r, "f.txt", "A\nB2\nC\nD\n", "c2", 2);
    // c3: Z A B' D   (Z inserted at top, C removed)
    let c3 = commit(&r, "f.txt", "Z\nA\nB2\nD\n", "c3", 3);
    let got: Vec<(String, ObjectId)> = r.blame("f.txt", None).unwrap().into_iter().map(|l| (l.text, l.commit)).collect();
    let want = vec![("Z".to_string(), c3), ("A".into(), c1), ("B2".into(), c2), ("D".into(), c2)];
    assert_eq!(got, want);
    let nums: Vec<usize> = r.blame("f.txt", None).unwrap().iter().map(|l| l.line_no).collect();
    assert_eq!(nums, [1, 2, 3, 4]);
    // blame at an older revision
    let old: Vec<(String, ObjectId)> = r
        .blame("f.txt", Some("HEAD~1"))
        .unwrap()
        .into_iter()
        .map(|l| (l.text, l.commit))
        .collect();
    assert_eq!(
        old,
        vec![("A".to_string(), c1), ("B2".into(), c2), ("C".into(), c1), ("D".into(), c2)]
    );
}

#[test]
fn blame_follows_first_parent_only_through_a_merge() {
    let (_d, r) = setup();
    let base = commit(&r, "f", "1\n2\n", "base", 1);
    r.create_branch("root", "side").unwrap();
    r.checkout("root", "side").unwrap();
    commit(&r, "f", "1\n2\nside\n", "side", 2);
    r.checkout("root", "main").unwrap();
    let m2 = commit(&r, "g", "other\n", "main work", 3);
    let _ = m2;
    r.merge("root", "side", sig(4)).unwrap();
    let lines = r.blame("f", None).unwrap();
    let head = r.resolve_rev("root", "HEAD").unwrap();
    // the line that arrived via the side branch is attributed to the merge commit
    assert_eq!(lines[2].text, "side");
    assert_eq!(lines[2].commit, head);
    assert_eq!(lines[0].commit, base);
}

#[test]
fn blame_refusals() {
    let (_d, r) = setup();
    assert!(r.blame("f", None).is_err()); // no commits
    commit(&r, "f", "x\n", "c", 1);
    fs::create_dir_all(r.worktree().join("d")).unwrap();
    commit(&r, "d/g", "y\n", "c2", 2);
    let e = r.blame("missing", None).unwrap_err().to_string();
    assert!(e.contains("does not exist"), "{e}");
    assert!(r.blame("d", None).unwrap_err().to_string().contains("directory"));
    assert!(r.blame("", None).is_err());
    fs::write(r.worktree().join("bin"), [0u8, 1, 2]).unwrap();
    r.add(&["".into()]).unwrap();
    r.commit("bin", sig(3)).unwrap();
    assert!(r.blame("bin", None).unwrap_err().to_string().contains("binary"));
    assert!(r.blame("f", Some("HEAD~99")).is_err());
}
