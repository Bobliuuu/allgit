//! Commit notes (`Repo::notes_*`): storage, hiding, gc, fsck, publish/fetch, bundles (docs/notes.md).

use std::fs;
use std::time::{Duration, SystemTime};
use vcs_objects::names::NOTES_BRANCH;
use vcs_objects::Signature;
use vcs_remote::{load_manifest, MemBackend};
use vcs_repo::bundle;
use vcs_repo::{GcOptions, Repo};

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}
fn write(r: &Repo, p: &str, c: &str) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
}
fn commit_all(r: &Repo, m: &str) {
    r.add(&["".into()]).unwrap();
    r.commit(m, sig()).unwrap();
}
fn repo_with_history(dir: &std::path::Path, name: &str) -> Repo {
    let r = Repo::init(&dir.join(name)).unwrap();
    r.create_zone("engine", "engine").unwrap();
    write(&r, "README", "1");
    write(&r, "engine/core.rs", "1");
    commit_all(&r, "one");
    write(&r, "README", "2");
    write(&r, "engine/core.rs", "2");
    commit_all(&r, "two");
    r
}
fn show(r: &Repo, zone: &str, rev: &str) -> Option<String> {
    r.notes_show(zone, rev).unwrap().map(|(_, b)| String::from_utf8(b).unwrap())
}
fn fsck_ok(r: &Repo) {
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}

#[test]
fn add_show_append_remove_list_round_trip() {
    let d = tempfile::tempdir().unwrap();
    let r = repo_with_history(d.path(), "a");
    assert_eq!(show(&r, "root", "HEAD"), None);
    assert!(r.notes_list("root").unwrap().is_empty());

    let target = r.notes_add("root", "HEAD~1", "first note\n\n\n", false, sig()).unwrap();
    assert_eq!(target, r.resolve_rev("root", "HEAD~1").unwrap());
    assert_eq!(
        show(&r, "root", "HEAD~1").as_deref(),
        Some("first note\n"),
        "one final newline, like git"
    );
    assert_eq!(show(&r, "root", "HEAD"), None);

    // an existing note is refused without force, kept intact
    let e = r.notes_add("root", "HEAD~1", "other", false, sig()).unwrap_err().to_string();
    assert!(e.contains("existing notes"), "{e}");
    assert_eq!(show(&r, "root", "HEAD~1").as_deref(), Some("first note\n"));
    r.notes_add("root", "HEAD~1", "replaced", true, sig()).unwrap();
    assert_eq!(show(&r, "root", "HEAD~1").as_deref(), Some("replaced\n"));

    // append: blank line between, creates when missing
    r.notes_append("root", "HEAD~1", "more", sig()).unwrap();
    assert_eq!(show(&r, "root", "HEAD~1").as_deref(), Some("replaced\n\nmore\n"));
    r.notes_append("root", "HEAD", "fresh", sig()).unwrap();
    assert_eq!(show(&r, "root", "HEAD").as_deref(), Some("fresh\n"));

    let list = r.notes_list("root").unwrap();
    assert_eq!(list.len(), 2);
    let head = r.resolve_rev("root", "HEAD").unwrap();
    let n = list.iter().find(|n| n.commit == head).unwrap();
    assert_eq!(r.notes_read_blob(&n.blob).unwrap(), b"fresh\n");

    // remove
    r.notes_remove("root", "HEAD", sig()).unwrap();
    assert_eq!(show(&r, "root", "HEAD"), None);
    assert!(r.notes_remove("root", "HEAD", sig()).unwrap_err().to_string().contains("no note"));
    assert_eq!(r.notes_list("root").unwrap().len(), 1);
    r.notes_remove("root", "HEAD~1", sig()).unwrap();
    assert!(r.notes_list("root").unwrap().is_empty());

    // empty text is refused (git would remove)
    assert!(r.notes_add("root", "HEAD", " \n\n", false, sig()).is_err());
    // the history of the notes ref is an ordinary commit chain
    assert!(r.refs().unwrap().notes_head("root").is_some());
    fsck_ok(&r);
}

#[test]
fn notes_are_per_zone_and_the_ref_is_not_a_user_branch() {
    let d = tempfile::tempdir().unwrap();
    let r = repo_with_history(d.path(), "a");
    r.notes_add("root", "HEAD", "root note", false, sig()).unwrap();
    r.notes_add("engine", "HEAD", "engine note", false, sig()).unwrap();
    assert_eq!(show(&r, "root", "HEAD").as_deref(), Some("root note\n"));
    assert_eq!(show(&r, "engine", "HEAD").as_deref(), Some("engine note\n"));
    // an engine commit is not a root-zone commit
    let engine_head = r.resolve_rev("engine", "HEAD").unwrap().to_hex();
    assert!(r.notes_add("root", &engine_head, "x", false, sig()).is_err());

    // hidden from branch listings, not creatable or checked out by name
    for z in ["root", "engine"] {
        assert!(r.branches(z).unwrap().iter().all(|(b, _, _)| b != NOTES_BRANCH), "{z}");
        assert!(r.refs().unwrap().branches_of(z).iter().all(|(b, _)| b != NOTES_BRANCH));
    }
    assert!(r.create_branch("root", NOTES_BRANCH).is_err());
    assert!(r.checkout("root", NOTES_BRANCH).is_err());
    fsck_ok(&r);
}

#[test]
fn gc_keeps_notes_and_their_history() {
    let d = tempfile::tempdir().unwrap();
    let r = repo_with_history(d.path(), "a");
    r.notes_add("root", "HEAD", "keep me", false, sig()).unwrap();
    r.notes_append("root", "HEAD", "and me", sig()).unwrap();
    r.notes_remove("root", "HEAD", sig()).unwrap();
    r.notes_add("root", "HEAD~1", "still here", false, sig()).unwrap();
    let later = SystemTime::now() + Duration::from_secs(30 * 86400);
    r.gc_sweep_at(later, GcOptions::new(Duration::from_secs(14 * 86400))).unwrap();
    assert_eq!(show(&r, "root", "HEAD~1").as_deref(), Some("still here\n"));
    fsck_ok(&r);
    // the note history (parent chain) survived too
    let head = r.refs().unwrap().notes_head("root").unwrap();
    let mut n = 0;
    let mut cur = Some(head);
    while let Some(c) = cur {
        n += 1;
        let vcs_objects::Object::Commit(co) = r.store().get(&c).unwrap() else {
            panic!("not a commit")
        };
        cur = co.parents.first().copied();
    }
    assert_eq!(n, 4);
}

#[test]
fn publish_and_clone_carry_notes_and_fetch_fast_forwards() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let a = repo_with_history(d.path(), "a");
    a.notes_add("root", "HEAD", "from a", false, sig()).unwrap();
    a.notes_add("engine", "HEAD", "engine from a", false, sig()).unwrap();
    let rep = a.publish(&remote, None, sig()).unwrap();
    assert!(rep.notes_diverged.is_empty());
    let (m, _) = load_manifest(&remote).unwrap();
    assert!(m.head("root", NOTES_BRANCH).is_some() && m.head("engine", NOTES_BRANCH).is_some());
    // nothing changed: nothing to publish
    assert!(a.publish(&remote, None, sig()).unwrap().updated.is_empty());

    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), None).unwrap();
    assert_eq!(show(&b, "root", "HEAD").as_deref(), Some("from a\n"), "clone brings the note blobs");
    assert_eq!(show(&b, "engine", "HEAD").as_deref(), Some("engine from a\n"));
    assert!(b.branches("root").unwrap().iter().all(|(n, _, _)| n != NOTES_BRANCH));
    fsck_ok(&b);

    // b annotates another commit, publishes; a fetches and fast-forwards
    b.notes_add("root", "HEAD~1", "from b", false, sig()).unwrap();
    assert_eq!(
        b.publish(&remote, None, sig()).unwrap().updated.len(),
        1,
        "only the notes ref moved"
    );
    a.fetch(&remote, None).unwrap();
    assert_eq!(show(&a, "root", "HEAD~1").as_deref(), Some("from b\n"));
    assert_eq!(show(&a, "root", "HEAD").as_deref(), Some("from a\n"));
    fsck_ok(&a);
}

#[test]
fn diverged_notes_merge_on_fetch_and_publish_refuses_until_then() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let a = repo_with_history(d.path(), "a");
    a.notes_add("root", "HEAD", "base", false, sig()).unwrap();
    a.publish(&remote, None, sig()).unwrap();
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), None).unwrap();

    // both sides annotate: different commits, and one commit on both sides
    a.notes_add("root", "HEAD~1", "a only", false, sig()).unwrap();
    a.notes_append("root", "HEAD", "a extra", sig()).unwrap();
    b.notes_add("engine", "HEAD", "b engine", false, sig()).unwrap();
    b.notes_append("root", "HEAD", "b extra", sig()).unwrap();
    assert!(b.publish(&remote, None, sig()).unwrap().notes_diverged.is_empty());

    // a is now behind AND ahead: its notes are not published, with a report instead of an error
    let rep = a.publish(&remote, None, sig()).unwrap();
    assert_eq!(rep.notes_diverged, vec!["root".to_owned()]);
    a.fetch(&remote, None).unwrap();
    assert_eq!(show(&a, "root", "HEAD~1").as_deref(), Some("a only\n"));
    assert_eq!(show(&a, "engine", "HEAD").as_deref(), Some("b engine\n"));
    assert_eq!(show(&a, "root", "HEAD").as_deref(), Some("base\n\na extra\n\nbase\n\nb extra\n"));
    fsck_ok(&a);
    let rep = a.publish(&remote, None, sig()).unwrap();
    assert!(rep.notes_diverged.is_empty() && rep.updated.len() == 1);
    b.fetch(&remote, None).unwrap();
    assert_eq!(show(&b, "root", "HEAD"), show(&a, "root", "HEAD"));
    assert_eq!(show(&b, "root", "HEAD~1").as_deref(), Some("a only\n"));
}

#[test]
fn bundles_carry_notes() {
    let d = tempfile::tempdir().unwrap();
    let a = repo_with_history(d.path(), "a");
    a.notes_add("root", "HEAD", "in a bundle", false, sig()).unwrap();
    a.notes_add("engine", "HEAD~1", "engine too", false, sig()).unwrap();
    let f = d.path().join("all.bundle");
    a.bundle_create(&f, &[], None).unwrap();
    bundle::verify_file(&f).unwrap();
    let c = Repo::clone_from_bundle(&f, &d.path().join("c"), sig()).unwrap();
    assert_eq!(show(&c, "root", "HEAD").as_deref(), Some("in a bundle\n"));
    assert_eq!(show(&c, "engine", "HEAD~1").as_deref(), Some("engine too\n"));
    assert!(c.branches("root").unwrap().iter().all(|(b, _, _)| b != NOTES_BRANCH));
    fsck_ok(&c);

    // a zone-selected bundle fetched into an existing repository folds the notes in
    let f2 = d.path().join("root.bundle");
    a.notes_append("root", "HEAD", "later", sig()).unwrap();
    a.bundle_create(&f2, &["root".to_owned()], None).unwrap();
    let rep = c.bundle_fetch(&f2, sig()).unwrap();
    assert!(rep.updated.iter().any(|u| u.1 == NOTES_BRANCH));
    assert_eq!(show(&c, "root", "HEAD").as_deref(), Some("in a bundle\n\nlater\n"));
    assert_eq!(show(&c, "engine", "HEAD~1").as_deref(), Some("engine too\n"));
}
