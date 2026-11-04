//! Request breakdown of the remote hot paths (issues #240 and #227): a 1-file publish, a fetch
//! with nothing new, a fetch with one new commit, and a sparse clone. Counts, not time. Run with
//! `--nocapture` to print the per-request breakdown quoted in docs/benchmarks.md.
use std::fs;
use vcs_objects::Signature;
use vcs_remote::{LoggingBackend, MemBackend};
use vcs_repo::Repo;

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
fn seeded(remote: &MemBackend, dir: &std::path::Path) -> Repo {
    let a = Repo::init(&dir.join("a")).unwrap();
    a.create_zone("engine", "engine").unwrap();
    a.create_zone("assets", "assets").unwrap();
    write(&a, "README", "readme");
    write(&a, "engine/core.rs", "core v1");
    write(&a, "assets/big.bin", &"B".repeat(50_000));
    commit_all(&a, "init");
    a.publish(remote, None, sig()).unwrap();
    a
}

#[test]
fn request_breakdown_of_publish_fetch_and_sparse_clone() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let a = seeded(&remote, d.path());
    let baseline = std::env::var("ALLGIT_BASELINE").is_ok();

    // 1-file publish into an existing history
    write(&a, "engine/core.rs", "core v2");
    commit_all(&a, "edit");
    let lb = LoggingBackend::new(&remote);
    a.publish(&lb, None, sig()).unwrap();
    lb.dump("1-file publish");
    let publish_total = lb.calls().len();
    let publish_manifest_gets = lb.count("GET manifest") + lb.count("GET-IF-NONE-MATCH manifest");

    // a second repository: clone, then fetch with nothing new, then fetch after one new commit
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), None).unwrap();
    let lb = LoggingBackend::new(&remote);
    b.fetch(&lb, None).unwrap();
    lb.dump("fetch, first time after clone (nothing new)");
    let fetch_first = lb.calls().len();
    lb.clear();
    b.fetch(&lb, None).unwrap();
    lb.dump("fetch, nothing new");
    let fetch_idle = lb.calls().len();
    let idle_manifest_gets = lb.count("GET manifest");
    write(&a, "engine/core.rs", "core v3");
    commit_all(&a, "edit again");
    a.publish(&remote, None, sig()).unwrap();
    lb.clear();
    b.fetch(&lb, None).unwrap();
    lb.dump("fetch, one new commit");

    // sparse clone of one zone: requests and bytes
    let before = remote.bytes_read();
    let lb = LoggingBackend::new(&remote);
    Repo::clone_from(&lb, "mem", &d.path().join("c"), Some(&["engine".to_string()])).unwrap();
    lb.dump("sparse clone of one zone");
    println!("sparse clone read {} bytes", remote.bytes_read() - before);

    println!("SUMMARY publish_requests={publish_total} publish_manifest_reads={publish_manifest_gets} fetch_after_clone={fetch_first} idle_fetch_requests={fetch_idle} idle_fetch_full_manifest_gets={idle_manifest_gets}");
    if !baseline {
        assert_eq!(publish_manifest_gets, 1, "publish reads the manifest once");
        assert_eq!(idle_manifest_gets, 0, "an idle fetch must not download the manifest");
        assert_eq!(fetch_idle, 1, "an idle fetch is one conditional request");
    }
}
