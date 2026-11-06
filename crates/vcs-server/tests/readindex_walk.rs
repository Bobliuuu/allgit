//! The read-policy index walk reads a level of the history concurrently: on a backend with latency the
//! first restricted request costs round trips per tree depth, not one per file (docs/server.md, "Cost").

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use vcs_objects::*;
use vcs_remote::protocol::object_key;
use vcs_remote::*;
use vcs_server::readguard::ReadIndex;
use vcs_store::{MemoryStore, ObjectStore};

/// Every read waits `delay`, like a remote object store.
struct Slow<'a> {
    inner: &'a MemBackend,
    delay: Duration,
    /// Reads in flight now, and the most there ever were at once.
    now: AtomicUsize,
    peak: AtomicUsize,
}
impl Slow<'_> {
    fn wait(&self) {
        let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(n, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        self.now.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Backend for Slow<'_> {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        self.wait();
        self.inner.get(key)
    }
    fn get_range(&self, key: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.wait();
        self.inner.get_range(key, o, l)
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        self.inner.exists(key)
    }
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        self.inner.put_if_absent(key, data)
    }
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        self.inner.put_if_match(key, data, tag)
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        self.inner.list(prefix)
    }
}

#[test]
fn walk_is_concurrent_and_complete() {
    let s = MemoryStore::new();
    let mem = MemBackend::new();
    let mut ids = vec![];
    let mut entries = vec![];
    for i in 0..240 {
        let id = s.put_blob(format!("file {i}").as_bytes()).unwrap();
        ids.push(id);
        entries.push(TreeEntry {
            name: format!("f{i}"),
            mode: EntryMode::File,
            id,
        });
    }
    let t = s.put(&Object::Tree(Tree::new(entries).unwrap())).unwrap();
    let c = s
        .put(&Object::Commit(Commit {
            zone: "z".into(),
            parents: vec![],
            root: t,
            change_id: ChangeId::from_bytes([1; 16]),
            author: Signature {
                name: "c".into(),
                email: "c".into(),
                timestamp: 1,
            },
            message: "m".into(),
        }))
        .unwrap();
    ids.extend([t, c]);
    for id in &ids {
        mem.put_if_absent(&object_key(id), &s.get(id).unwrap().encode()).unwrap();
    }
    let mut m = Manifest::default();
    m.branches.insert(("z".into(), "main".into()), c);
    m.zones.insert("z".into(), "z".into());
    mem.put_if_absent("manifest", &m.encode()).unwrap();

    let slow = Slow {
        inner: &mem,
        delay: Duration::from_millis(5),
        now: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
    };
    let mut ix = ReadIndex::default();
    ix.refresh(&slow).unwrap();
    // every object of the zone is attributed to it; a stranger is not
    let readable = |_: &str| false;
    for id in &ids {
        assert!(ix.denied_only(id, &readable), "{id} should be in zone z");
    }
    assert!(!ix.denied_only(&ObjectId::from_bytes([9; 32]), &readable));
    // Judged by the most reads in flight at once, not by wall time (which flakes on a loaded runner): a walk
    // that read one object at a time would peak at 1.
    let peak = slow.peak.load(Ordering::SeqCst);
    assert!(peak >= 2, "probes are not concurrent (peak {peak} reads in flight)");
}
