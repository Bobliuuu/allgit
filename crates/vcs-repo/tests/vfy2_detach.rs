use std::fs;
use vcs_objects::Signature;
use vcs_remote::MemBackend;
use vcs_repo::Repo;

fn sig(t: u64) -> Signature { Signature { name: "T".into(), email: "t@x".into(), timestamp: t } }
fn commit(r: &Repo, c: &str, t: u64) { fs::write(r.worktree().join("a"), c).unwrap(); r.add(&["".into()]).unwrap(); r.commit(c, sig(t)).unwrap(); }

#[test]
fn detach_by_id_origin_nonexistent_ambiguous_and_local_branch_unchanged() {
    let d = tempfile::tempdir().unwrap();
    let a = Repo::init(&d.path().join("a")).unwrap();
    commit(&a, "0", 1);
    let remote = MemBackend::new();
    a.publish(&remote, None, sig(1)).unwrap();
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), None).unwrap();
    // many commits on a until two share a 4-hex prefix
    let mut ids = vec![];
    let mut amb: Option<String> = None;
    for i in 1..3000u64 {
        commit(&a, &format!("c{i}"), 10 + i);
        let id = a.resolve_rev("root", "HEAD").unwrap().to_hex();
        if let Some(o) = ids.iter().find(|o: &&String| o[..4] == id[..4]) { let _ = o; amb = Some(id[..4].to_string()); ids.push(id); break; }
        ids.push(id);
    }
    a.publish(&remote, None, sig(5000)).unwrap();
    b.fetch(&remote, None).unwrap();
    let amb = amb.expect("no 4-char collision in 3000 commits");
    let before_branch = b.resolve_rev("root", "main").unwrap();
    // ambiguous
    let e = b.checkout_detached("root", &amb).unwrap_err().to_string();
    eprintln!("ambiguous msg: {e}");
    assert!(e.to_lowercase().contains("ambig") || e.to_lowercase().contains("more than one") || e.to_lowercase().contains("multiple"), "{e}");
    // nonexistent
    assert!(b.checkout_detached("root", "deadbeefdeadbeef").is_err());
    assert!(b.checkout_detached("root", "origin/nope").is_err());
    assert!(b.checkout_detached("root", "zzzz").is_err());
    assert_eq!(fs::read_to_string(b.worktree().join("a")).unwrap(), "0", "failed detach changed the worktree");
    // by unique id
    let want = ids.iter().find(|i| i[..4] != amb[..]).unwrap().clone();
    let got = b.checkout_detached("root", &want[..16]).unwrap();
    assert_eq!(got.to_hex(), want);
    // origin/main
    let om = b.checkout_detached("root", "origin/main").unwrap();
    assert_eq!(om, a.resolve_rev("root", "HEAD").unwrap());
    assert_eq!(fs::read_to_string(b.worktree().join("a")).unwrap(), "c".to_string() + &(ids.len()).to_string());
    // local branch main still where it was; switching back works and is clean
    assert_eq!(b.resolve_rev("root", "main").unwrap(), before_branch);
    b.checkout("root", "main").unwrap();
    assert_eq!(fs::read_to_string(b.worktree().join("a")).unwrap(), "0");
    assert!(b.fsck().unwrap().problems.is_empty());
}
