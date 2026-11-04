//! A fetch reads only the pack indexes it has not seen: the ones kept by the clone (and earlier
//! fetches) come from `.vcs/remote-idx`, and a damaged cached index is detected and replaced.
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

fn idx_gets(calls: &[String]) -> Vec<String> {
    calls
        .iter()
        .filter(|c| c.starts_with("GET ") && c.ends_with(".idx"))
        .cloned()
        .collect()
}

fn commit_publish(o: &Repo, mem: &MemBackend, n: usize) {
    for i in 0..30 {
        fs::write(o.worktree().join(format!("f{n}_{i}.txt")), format!("round {n} file {i}")).unwrap();
    }
    o.add(&["".into()]).unwrap();
    o.commit(&format!("r{n}"), sig()).unwrap();
    o.publish(mem, None, sig()).unwrap();
}

#[test]
fn fetch_downloads_only_new_pack_indexes_and_repairs_a_damaged_cache() {
    let d = tempfile::tempdir().unwrap();
    let o = Repo::init(&d.path().join("origin")).unwrap();
    let mem = MemBackend::new();
    commit_publish(&o, &mem, 0);
    commit_publish(&o, &mem, 1);

    let c = Repo::clone_from(&mem, "mem", &d.path().join("clone"), None).unwrap();
    let cache = c.worktree().join(".vcs/remote-idx");
    let cached = |dir: &std::path::Path| fs::read_dir(dir).map(|r| r.count()).unwrap_or(0);
    assert!(cached(&cache) >= 1, "the clone keeps the indexes it read");

    commit_publish(&o, &mem, 2);
    let log = LoggingBackend::new(&mem);
    let got = c.fetch(&log, None).unwrap();
    assert!(!got.is_empty());
    let gets = idx_gets(&log.calls());
    assert_eq!(gets.len(), 1, "only the new pack's index is downloaded: {gets:?}");

    // the same files, byte for byte, as an uncached fetch would give
    assert_eq!(
        c.remote_refs().unwrap().head("root", "main"),
        o.refs().unwrap().head("root", "main")
    );

    // damage every cached index: the next fetch must not trust them, and must repair them
    commit_publish(&o, &mem, 3);
    for e in fs::read_dir(&cache).unwrap() {
        let p = e.unwrap().path();
        let mut b = fs::read(&p).unwrap();
        let n = b.len();
        b[n / 2] ^= 0xff;
        fs::write(&p, b).unwrap();
    }
    let log = LoggingBackend::new(&mem);
    c.fetch(&log, None).unwrap();
    assert!(idx_gets(&log.calls()).len() >= 3, "damaged cache entries were refetched");
    assert_eq!(
        c.remote_refs().unwrap().head("root", "main"),
        o.refs().unwrap().head("root", "main")
    );
    let log = LoggingBackend::new(&mem);
    c.fetch(&log, None).unwrap();
    assert_eq!(idx_gets(&log.calls()).len(), 0, "repaired entries are served locally");
}

#[test]
fn torn_or_empty_manifest_cache_is_ignored_by_fetch() {
    // the cache is written without fsync: a crash may leave it empty or cut short
    let d = tempfile::tempdir().unwrap();
    let o = Repo::init(&d.path().join("origin")).unwrap();
    let mem = MemBackend::new();
    commit_publish(&o, &mem, 0);
    let c = Repo::clone_from(&mem, "mem", &d.path().join("clone"), None).unwrap();
    let cache = c.worktree().join(".vcs/remote-manifest");
    for n in [0usize, 7, 40] {
        commit_publish(&o, &mem, n + 10);
        let full = fs::read(&cache).unwrap_or_default();
        fs::write(&cache, &full[..full.len().min(n)]).unwrap();
        let got = c.fetch(&mem, None).unwrap();
        assert!(!got.is_empty());
        assert_eq!(
            c.remote_refs().unwrap().head("root", "main"),
            o.refs().unwrap().head("root", "main")
        );
    }
}
