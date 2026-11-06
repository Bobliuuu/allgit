//! fsck reads and decodes objects in batches (bounded memory: 16 GB at 1,000,000 commits before). The report must not
//! depend on the batch size: same problems for a clean repository and for damage, including a chunk manifest whose
//! chunks sit in other batches.
use std::fs;
use vcs_objects::{Object, ObjectId, Signature};
use vcs_repo::Repo;

fn sig(t: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: t,
    }
}
fn rnd(n: usize, mut x: u64) -> Vec<u8> {
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}
fn object_path(r: &Repo, id: &ObjectId) -> std::path::PathBuf {
    let h = id.to_hex();
    r.worktree().join(".vcs/objects").join(&h[..2]).join(&h[2..])
}

#[test]
fn fsck_report_does_not_depend_on_the_batch_size() {
    std::env::set_var("VCS_PACK_MIN", "1000000"); // keep objects loose so one can be deleted
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("a")).unwrap();
    fs::write(r.worktree().join("big.bin"), rnd(2 << 20, 3)).unwrap();
    for i in 0..12 {
        fs::write(r.worktree().join(format!("f{i}.txt")), format!("file {i}")).unwrap();
        r.add(&["".into()]).unwrap();
        r.commit(&format!("c{i}"), sig(1000 + i)).unwrap();
    }
    let report = |batch: &str| {
        std::env::set_var("VCS_FSCK_BATCH", batch);
        let rep = r.fsck().unwrap();
        let mut p = rep.problems.clone();
        p.sort();
        (rep.objects, p)
    };
    let (n_small, p_small) = report("3");
    let (n_big, p_big) = report("1000000");
    assert!(p_small.is_empty() && p_big.is_empty(), "{p_small:?} {p_big:?}");
    assert_eq!(n_small, n_big);
    assert!(n_small > 12 * 2, "the repository must span many batches of 3");
    // damage: delete a chunk of the manifest and a tree; both batch sizes report the same problems
    let head = r.refs().unwrap().head("root", "main").unwrap();
    let Object::Commit(c) = r.store().get(&head).unwrap() else {
        panic!()
    };
    let Object::Tree(t) = r.store().get(&c.root).unwrap() else {
        panic!()
    };
    let big = t.entries().iter().find(|e| e.name == "big.bin").unwrap().id;
    let Object::Chunked(m) = r.store().get(&big).unwrap() else {
        panic!("not chunked")
    };
    fs::remove_file(object_path(&r, &m.chunks[0].id)).unwrap();
    let (_, q_small) = report("3");
    let (_, q_big) = report("1000000");
    assert!(q_small.iter().any(|x| x.contains("missing chunk")), "{q_small:?}");
    assert_eq!(q_small, q_big);
    std::env::remove_var("VCS_FSCK_BATCH");
}
