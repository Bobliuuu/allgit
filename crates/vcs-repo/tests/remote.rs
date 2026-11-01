use std::fs;
use vcs_objects::Signature;
use vcs_remote::{load_manifest, Backend, MemBackend};
use vcs_repo::{Repo, SyncAction};

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
fn read(r: &Repo, p: &str) -> String {
    fs::read_to_string(r.worktree().join(p)).unwrap()
}
fn commit_all(r: &Repo, m: &str) {
    r.add(&["".into()]).unwrap();
    r.commit(m, sig()).unwrap();
}
fn first_repo(dir: &std::path::Path) -> Repo {
    Repo::init(&dir.join("a")).unwrap()
}
fn seeded(remote: &MemBackend, dir: &std::path::Path) -> Repo {
    let a = first_repo(dir);
    a.create_zone("engine", "engine").unwrap();
    a.create_zone("assets", "assets").unwrap();
    write(&a, "README", "readme");
    write(&a, "engine/core.rs", "core v1");
    write(&a, "assets/big.bin", &"B".repeat(50_000));
    commit_all(&a, "init");
    let rep = a.publish(remote, None, sig()).unwrap();
    assert_eq!(rep.updated.len(), 3);
    a
}

#[test]
fn publish_then_full_clone_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    seeded(&remote, d.path());
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), None).unwrap();
    assert_eq!(read(&b, "engine/core.rs"), "core v1");
    assert_eq!(read(&b, "README"), "readme");
    assert_eq!(read(&b, "assets/big.bin").len(), 50_000);
    let st = b.status().unwrap();
    assert!(st.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty()) && st.untracked.is_empty());
    assert!(b.fsck().unwrap().problems.is_empty());
    assert_eq!(b.zones().unwrap().path_of("engine"), Some("engine"));
}

#[test]
fn sparse_clone_downloads_and_materializes_only_requested_zones() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    seeded(&remote, d.path());
    let before = remote.bytes_read();
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), Some(&["engine".to_string()])).unwrap();
    let read_bytes = remote.bytes_read() - before;
    assert!(
        // Metadata read-ahead starts at 4 KiB and grows 4x per request. When the encoded metadata is a few
        // bytes over 4 KiB the second request is 16 KiB, so the total varies run to run (about 21.8 KB worst
        // case seen). What must hold is that the 50 KB asset is not fetched.
        read_bytes < 30_000,
        "sparse clone read {read_bytes} bytes; the 50KB asset must not be fetched"
    );
    assert_eq!(read(&b, "engine/core.rs"), "core v1");
    assert!(!b.worktree().join("assets").exists());
    assert!(!b.worktree().join("README").exists(), "root not requested");
    let st = b.status().unwrap();
    assert!(st.unsubscribed.contains(&"assets".to_string()));
    assert!(
        st.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty()),
        "missing files are not deletions"
    );
    // subscribe later: now it is fetched
    b.fetch(&remote, Some(&["assets".to_string()])).unwrap();
    b.zone_add_fetching(&remote, "assets", None).unwrap();
    assert_eq!(read(&b, "assets/big.bin").len(), 50_000);
}

#[test]
fn disjoint_publishers_then_sync_fast_forwards() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let a = seeded(&remote, d.path());
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), None).unwrap();
    // a edits engine, b edits assets, both publish
    write(&a, "engine/core.rs", "core v2");
    commit_all(&a, "engine v2");
    write(&b, "assets/small.txt", "s");
    commit_all(&b, "asset add");
    a.publish(&remote, None, sig()).unwrap();
    b.publish(&remote, None, sig()).unwrap();
    let (m, _) = load_manifest(&remote).unwrap();
    assert_eq!(m.generation, 3);
    // sync brings each side the other's change
    let ra = a.sync(&remote).unwrap();
    assert!(ra
        .actions
        .iter()
        .any(|(ar, _, act)| ar == "assets" && matches!(act, SyncAction::FastForwarded { .. })));
    assert_eq!(read(&a, "assets/small.txt"), "s");
    let rb = b.sync(&remote).unwrap();
    assert!(rb
        .actions
        .iter()
        .any(|(ar, _, act)| ar == "engine" && matches!(act, SyncAction::FastForwarded { .. })));
    assert_eq!(read(&b, "engine/core.rs"), "core v2");
    // second sync is a no-op
    assert!(b.sync(&remote).unwrap().actions.iter().all(|(_, _, a)| *a == SyncAction::UpToDate));
    assert!(a.fsck().unwrap().problems.is_empty() && b.fsck().unwrap().problems.is_empty());
}

#[test]
fn non_fast_forward_publish_is_refused_with_guidance() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let a = seeded(&remote, d.path());
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), None).unwrap();
    write(&a, "README", "from a");
    commit_all(&a, "a edit");
    write(&b, "README", "from b");
    commit_all(&b, "b edit");
    a.publish(&remote, None, sig()).unwrap();
    let before = load_manifest(&remote).unwrap().0;
    // b doesn't know about a's commit yet: publish compares against the *live* remote head
    let err = b.publish(&remote, None, sig()).unwrap_err().to_string();
    assert!(
        err.contains("Cannot publish root/main") && err.contains("Remote branch advanced"),
        "{err}"
    );
    assert_eq!(load_manifest(&remote).unwrap().0, before, "remote untouched");
    // sync reports divergence and does not touch b's work
    let rep = b.sync(&remote).unwrap();
    assert!(rep.actions.iter().any(|(a, _, act)| a == "root" && *act == SyncAction::Diverged));
    assert_eq!(read(&b, "README"), "from b");
}

#[test]
fn multi_zone_publish_is_one_remote_transaction() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let a = seeded(&remote, d.path());
    let gen0 = load_manifest(&remote).unwrap().0.generation;
    write(&a, "engine/core.rs", "v2");
    write(&a, "assets/big.bin", "tiny");
    commit_all(&a, "cross-zone change");
    let rep = a.publish(&remote, None, sig()).unwrap();
    assert_eq!(rep.updated.len(), 2);
    let (m, _) = load_manifest(&remote).unwrap();
    assert_eq!(m.generation, gen0 + 1, "both zones advanced in ONE manifest generation");
}

#[test]
fn publish_is_idempotent_and_noop_when_up_to_date() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let a = seeded(&remote, d.path());
    let g = load_manifest(&remote).unwrap().0.generation;
    let rep = a.publish(&remote, None, sig()).unwrap();
    assert!(rep.updated.is_empty() && rep.outcome.is_none());
    assert_eq!(load_manifest(&remote).unwrap().0.generation, g);
}

#[test]
fn sync_leaves_dirty_zone_alone() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let a = seeded(&remote, d.path());
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), None).unwrap();
    write(&a, "engine/core.rs", "remote change");
    commit_all(&a, "c");
    a.publish(&remote, None, sig()).unwrap();
    write(&b, "engine/core.rs", "local uncommitted");
    let rep = b.sync(&remote).unwrap();
    assert!(rep.actions.iter().any(|(ar, _, act)| ar == "engine" && *act == SyncAction::Dirty));
    assert_eq!(read(&b, "engine/core.rs"), "local uncommitted");
}

#[test]
fn conflicting_zone_definitions_are_rejected() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    seeded(&remote, d.path());
    // independent repo defines "engine" at a different path
    let c = Repo::init(&d.path().join("c")).unwrap();
    c.create_zone("engine", "src/engine").unwrap();
    write(&c, "src/engine/x", "x");
    commit_all(&c, "m");
    // publish a branch that does not exist remotely, so the ref check passes and the
    // zone-definition check is what rejects it
    c.create_branch("engine", "other").unwrap();
    c.checkout("engine", "other").unwrap();
    let e = c.publish(&remote, None, sig()).unwrap_err().to_string();
    assert!(e.contains("zone definition conflict"), "{e}");
    // and an overlapping one under another name
    let f = Repo::init(&d.path().join("f")).unwrap();
    f.create_zone("eng2", "engine/sub").unwrap();
    write(&f, "engine/sub/x", "x");
    commit_all(&f, "m");
    assert!(f.publish(&remote, None, sig()).unwrap_err().to_string().contains("overlaps"));
}

#[test]
fn clone_into_nonempty_dir_refused() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    seeded(&remote, d.path());
    fs::create_dir_all(d.path().join("x")).unwrap();
    fs::write(d.path().join("x/keep"), "k").unwrap();
    assert!(Repo::clone_from(&remote, "mem", &d.path().join("x"), None).is_err());
    assert_eq!(fs::read_to_string(d.path().join("x/keep")).unwrap(), "k");
    let _ = &remote as &dyn Backend;
}

fn object_path(r: &Repo, id: &vcs_objects::ObjectId) -> std::path::PathBuf {
    let h = id.to_hex();
    r.worktree().join(".vcs/objects").join(&h[..2]).join(&h[2..])
}

/// Blob id of `engine/core.rs` in the clone `b`, via its head commit's tree.
fn core_blob(b: &Repo) -> vcs_objects::ObjectId {
    use vcs_objects::Object;
    let head = b.refs().unwrap().head("engine", "main").unwrap();
    let Object::Commit(c) = b.store().get(&head).unwrap() else {
        panic!()
    };
    let Object::Tree(t) = b.store().get(&c.root).unwrap() else {
        panic!()
    };
    let e = t.entries().iter().find(|e| e.name == "core.rs").expect("core.rs");
    e.id
}

#[test]
fn evicted_object_is_refetched_on_demand_and_fsck_calls_it_cache() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    seeded(&remote, d.path());
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), Some(&["engine".to_string()])).unwrap();
    let blob = core_blob(&b);
    fs::remove_file(object_path(&b, &blob)).unwrap();
    // Evicted: fsck sees a cache miss, not damage.
    let rep = b.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
    assert_eq!(rep.evictable, vec![blob]);
    // Without lazy fetch the command fails; with it, it refetches and succeeds.
    b.zone_remove("engine").unwrap_or(());
    assert!(b.zone_add("engine", None).is_err());
    b.with_lazy_fetch(&remote, || b.zone_add("engine", None)).unwrap();
    assert_eq!(read(&b, "engine/core.rs"), "core v1");
    assert!(b.store().contains(&blob).unwrap());
    let rep = b.fsck().unwrap();
    assert!(rep.problems.is_empty() && rep.evictable.is_empty());
}

#[test]
fn missing_local_only_object_is_corruption_not_cache() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let a = seeded(&remote, d.path());
    write(&a, "engine/new.rs", "unpublished");
    commit_all(&a, "local only");
    let head = a.refs().unwrap().head("engine", "main").unwrap();
    let vcs_objects::Object::Commit(c) = a.store().get(&head).unwrap() else {
        panic!()
    };
    fs::remove_file(object_path(&a, &c.root)).unwrap();
    let rep = a.fsck().unwrap();
    assert!(rep.evictable.is_empty());
    assert!(rep.problems.iter().any(|p| p.contains("missing root tree")), "{:?}", rep.problems);
}

#[test]
fn corrupt_remote_object_is_rejected_and_never_stored() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    seeded(&remote, d.path());
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), Some(&["engine".to_string()])).unwrap();
    let blob = core_blob(&b);
    fs::remove_file(object_path(&b, &blob)).unwrap();
    remote.tamper_object(&blob, vcs_objects::Object::Blob(b"evil".to_vec()).encode());
    let err = b.fetch_missing(&remote, &blob).unwrap_err().to_string();
    assert!(err.contains("hash verification"), "{err}");
    assert!(!b.store().contains(&blob).unwrap());
    assert!(!object_path(&b, &blob).exists());
    assert!(b.with_lazy_fetch(&remote, || b.zone_add("engine", None)).is_err());
    assert!(!b.store().contains(&blob).unwrap());
}

#[test]
fn renamed_zone_publishes_its_name_and_clones_and_syncs_under_it() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let a = seeded(&remote, d.path());
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), None).unwrap();
    a.zone_rename("engine", "core").unwrap();
    write(&a, "engine/core.rs", "core v2");
    commit_all(&a, "after rename");
    a.publish(&remote, Some(&["core".to_string()]), sig()).unwrap();
    let (m, _) = load_manifest(&remote).unwrap();
    assert_eq!(m.labels.get("engine").map(String::as_str), Some("core"));
    assert!(m.branches.contains_key(&("engine".into(), "main".into())), "refs stay keyed by id");
    // existing clone learns the name and fast-forwards; a fresh sparse clone can ask for it by name
    b.sync(&remote).unwrap();
    assert_eq!(b.zones().unwrap().label("engine"), "core");
    assert_eq!(read(&b, "engine/core.rs"), "core v2");
    let c = Repo::clone_from(&remote, "mem", &d.path().join("c"), Some(&["core".to_string()])).unwrap();
    assert_eq!(read(&c, "engine/core.rs"), "core v2");
    assert_eq!(c.log("core", None, 10).unwrap().len(), 2);
    for r in [&a, &b, &c] {
        assert!(r.fsck().unwrap().problems.is_empty());
    }
    // a rename made before publishing history, then renaming back, clears the published name
    a.zone_rename("core", "engine").unwrap();
    write(&a, "engine/core.rs", "core v3");
    commit_all(&a, "back");
    a.publish(&remote, None, sig()).unwrap();
    assert!(load_manifest(&remote).unwrap().0.labels.is_empty());
}
