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
        read_bytes < 5_000,
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
    b.zone_add("assets", None).unwrap();
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
