//! Operations store the tag view only when it changed (large sets), so an operation costs O(changes),
//! not O(tags); undo/restore still reproduce the tags of any operation.
use std::fs;
use vcs_objects::{Object, Signature};
use vcs_repo::Repo;

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}
fn commit(r: &Repo, p: &str, c: &str) {
    fs::write(r.worktree().join(p), c).unwrap();
    r.add(&["".into()]).unwrap();
    r.commit("m", sig()).unwrap();
}
fn tag_names(r: &Repo) -> Vec<String> {
    r.tag_list("root").unwrap().into_iter().map(|t| t.name).collect()
}

#[test]
fn unchanged_large_tag_view_is_not_repeated_and_restore_still_works() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "a", "1");
    for i in 0..80 {
        r.tag_create("root", &format!("t{i:03}"), None, None).unwrap();
    }
    let ops_after_tags = r.op_log(1).unwrap()[0].0;
    commit(&r, "a", "2");
    commit(&r, "a", "3");
    let log = r.op_log(2).unwrap();
    assert!(log[0].1.view.tags.is_none(), "unchanged tags must be omitted");
    assert!(log[1].1.view.tags.is_none());
    let size = |id| match r.store().get(id).unwrap() {
        Object::Operation(o) => o.view.tags.map_or(0, |t| t.len()),
        _ => unreachable!(),
    };
    assert_eq!(size(&log[0].0), 0);
    assert!(fs::read_to_string(r.worktree().join(".vcs").join("features"))
        .unwrap()
        .contains("op-tags-delta"));

    // a changed set is stored in full again, and later omissions refer to it
    r.tag_delete("root", "t000").unwrap();
    assert_eq!(r.op_log(1).unwrap()[0].1.view.tags.as_ref().map(|t| t.len()), Some(79));
    commit(&r, "a", "4");
    assert!(r.op_log(1).unwrap()[0].1.view.tags.is_none());
    let after_delete_commit = r.op_log(1).unwrap()[0].0;
    r.tag_create("root", "late", None, None).unwrap();
    assert_eq!(tag_names(&r).len(), 80);

    // restore to an operation whose tag view was omitted: tags of the nearest earlier full view come back
    r.op_restore(&after_delete_commit).unwrap();
    let names = tag_names(&r);
    assert_eq!(names.len(), 79);
    assert!(!names.contains(&"t000".to_owned()) && !names.contains(&"late".to_owned()));
    // and to the first one (80 tags, t000 present)
    r.op_restore(&ops_after_tags).unwrap();
    assert_eq!(tag_names(&r).len(), 80);
    assert!(tag_names(&r).contains(&"t000".to_owned()));
}

#[test]
fn small_tag_sets_keep_their_bytes() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "a", "1");
    r.tag_create("root", "v1", None, None).unwrap();
    commit(&r, "a", "2");
    assert!(r.op_log(1).unwrap()[0].1.view.tags.is_some());
    // the repository must not be marked op-tags-delta (older binaries could not read it)
    let features = fs::read_to_string(r.worktree().join(".vcs").join("features")).unwrap_or_default();
    assert!(!features.contains("op-tags-delta"), "{features}");
}
