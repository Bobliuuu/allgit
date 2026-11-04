//! The read-policy index walk reads a level of the history concurrently: on a backend with latency the
//! first restricted request costs round trips per tree depth, not one per file (docs/server.md, "Cost").

use std::time::{Duration, Instant};
use vcs_objects::*;
use vcs_remote::protocol::object_key;
use vcs_remote::*;
use vcs_server::readguard::ReadIndex;
use vcs_store::{MemoryStore, ObjectStore};

/// Every read waits `delay`, like a remote object store.
struct Slow<'a> {
    inner: &'a MemBackend,
    delay: Duration,
}
impl Backend for Slow<'_> {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        std::thread::sleep(self.delay);
        self.inner.get(key)
    }
    fn get_range(&self, key: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        std::thread::sleep(self.delay);
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
    };
    let t0 = Instant::now();
    let mut ix = ReadIndex::default();
    ix.refresh(&slow).unwrap();
    let took = t0.elapsed();
    // every object of the zone is attributed to it; a stranger is not
    let readable = |_: &str| false;
    for id in &ids {
        assert!(ix.denied_only(id, &readable), "{id} should be in zone z");
    }
    assert!(!ix.denied_only(&ObjectId::from_bytes([9; 32]), &readable));
    // 240 probes x 5 ms = 1.2 s one at a time; 8 at once is about 0.15 s. Generous bound for loaded CI.
    assert!(took < Duration::from_millis(700), "walk took {took:?}; probes are not concurrent");
}
