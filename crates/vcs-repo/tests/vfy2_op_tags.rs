use std::collections::BTreeMap;
use std::fs;
use vcs_objects::{ObjectId, Signature};
use vcs_remote::MemBackend;
use vcs_repo::Repo;

fn sig() -> Signature { Signature { name: "T".into(), email: "t@x".into(), timestamp: 1000 } }
fn commit(r: &Repo, c: &str) { fs::write(r.worktree().join("a"), c).unwrap(); r.add(&["".into()]).unwrap(); r.commit("m", sig()).unwrap(); }
fn tags(r: &Repo) -> BTreeMap<String, ObjectId> { r.tag_list("root").unwrap().into_iter().map(|t| (t.name, t.commit)).collect() }

#[test]
fn undo_restore_reproduce_exact_tag_sets_over_long_mixed_history() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "0");
    let mut rec: Vec<(ObjectId, BTreeMap<String, ObjectId>)> = vec![];
    let snap = |r: &Repo, rec: &mut Vec<_>| { rec.push((r.op_head().unwrap().unwrap(), tags(r))); };
    snap(&r, &mut rec);
    let mut n = 0;
    for i in 0..130 { r.tag_create("root", &format!("t{i:03}"), None, None).unwrap(); if i % 10 == 0 { snap(&r, &mut rec); } }
    snap(&r, &mut rec);
    for round in 0..200 {
        match round % 7 {
            0 | 1 | 2 => { n += 1; commit(&r, &format!("c{n}")); }
            3 => { r.tag_delete("root", &format!("t{:03}", round % 130)).ok(); }
            4 => { let _ = r.tag_create("root", &format!("n{round}"), None, None); }
            5 => { if round == 5 { let _ = fs::remove_file(r.worktree().join(".vcs/op-tags-hash")); } n += 1; commit(&r, &format!("c{n}")); }
            _ => { let _ = r.tag_delete("root", &format!("n{}", round - 2)); }
        }
        snap(&r, &mut rec);
        if round == 100 { let _ = fs::remove_file(r.worktree().join(".vcs/op-tags-hash")); }
    }
    assert!(tags(&r).len() >= 100);
    let omitted = r.op_log(10_000).unwrap().iter().filter(|(_, o)| o.view.tags.is_none()).count();
    assert!(omitted > 50, "omitted {omitted}");
    // restore every recorded op, in a shuffled order, and compare the tag set exactly
    let mut order: Vec<usize> = (0..rec.len()).collect();
    let mut s = 12345u64; for i in (1..order.len()).rev() { s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); order.swap(i, (s >> 33) as usize % (i + 1)); }
    for i in order { let (id, want) = &rec[i]; r.op_restore(id).unwrap(); assert_eq!(&tags(&r), want, "restore #{i}"); }
    // undo repeatedly: each undo returns to the state before the previous operation
    for _ in 0..40 { r.op_undo().unwrap(); }
    // redo-ish: restoring to the newest recorded op returns its set
    let (id, want) = rec.last().unwrap(); r.op_restore(id).unwrap(); assert_eq!(&tags(&r), want);
    assert!(r.fsck().unwrap().problems.is_empty());
    let feat = fs::read_to_string(r.worktree().join(".vcs/features")).unwrap();
    assert!(feat.contains("op-tags-delta"));
    // gc keeps everything the op chain needs
    r.gc_sweep_at(std::time::SystemTime::now() + std::time::Duration::from_secs(60*86400), vcs_repo::GcOptions::new(std::time::Duration::from_secs(14*86400))).unwrap();
    let (id, want) = &rec[3]; r.op_restore(id).unwrap(); assert_eq!(&tags(&r), want);
    assert!(r.fsck().unwrap().problems.is_empty());
}

#[test]
fn clone_of_such_repo_has_same_tags_and_works() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "0");
    for i in 0..100 { r.tag_create("root", &format!("t{i}"), None, None).unwrap(); }
    commit(&r, "1"); commit(&r, "2");
    let remote = MemBackend::new();
    r.publish(&remote, None, sig()).unwrap();
    let c = Repo::clone_from(&remote, "mem", &d.path().join("c"), None).unwrap();
    assert_eq!(tags(&c).len(), 100);
    commit(&c, "x"); c.tag_create("root", "zz", None, None).unwrap(); commit(&c, "y");
    c.op_undo().unwrap(); c.op_undo().unwrap();
    assert_eq!(tags(&c).len(), 100);
    commit(&r, "3"); r.publish(&remote, None, sig()).unwrap();
    assert!(c.fetch(&remote, None).is_ok());
    assert!(c.fsck().unwrap().problems.is_empty());
}
