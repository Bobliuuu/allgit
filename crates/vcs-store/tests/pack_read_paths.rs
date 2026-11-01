//! Reads from packs: many objects of one block, in parallel, after the pack was replaced under
//! an open store (the kept file handle must not serve stale or wrong bytes).

use std::sync::Arc;
use vcs_store::{LocalStore, ObjectStore};

fn blobs(n: usize) -> Vec<Vec<u8>> {
    (0..n)
        .map(|i| format!("file number {i} {}", "x".repeat(i % 97)).into_bytes())
        .collect()
}

#[test]
fn every_object_of_a_packed_store_reads_back_verified_in_parallel() {
    let d = tempfile::tempdir().unwrap();
    let s = Arc::new(LocalStore::open(d.path().join("objects")).unwrap());
    let data = blobs(3000);
    let ids: Vec<_> = data.iter().map(|b| s.put_blob(b).unwrap()).collect();
    s.sync().unwrap(); // a big batch: published as one pack
    assert!(s.pack_count() >= 1);
    let mut hs = vec![];
    for t in 0..8 {
        let (s, ids, data) = (s.clone(), ids.clone(), data.clone());
        hs.push(std::thread::spawn(move || {
            for i in (t..ids.len()).step_by(3) {
                assert_eq!(s.get_blob(&ids[i]).unwrap(), data[i]);
            }
        }));
    }
    for h in hs {
        h.join().unwrap();
    }
    assert!(s.verify_packs().is_empty());
}

#[test]
fn reads_still_work_after_gc_style_rewrite_and_a_fresh_open() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("objects");
    let s = LocalStore::open(&dir).unwrap();
    let data = blobs(500);
    let ids: Vec<_> = data.iter().map(|b| s.put_blob(b).unwrap()).collect();
    s.repack().unwrap();
    for i in 0..10 {
        assert_eq!(s.get_blob(&ids[i]).unwrap(), data[i]); // handle opened and kept
    }
    let doomed: std::collections::BTreeSet<_> = ids[..250].iter().copied().collect();
    s.sweep(&doomed).unwrap(); // pack rewritten, old one unlinked
    for i in 250..500 {
        assert_eq!(s.get_blob(&ids[i]).unwrap(), data[i]);
    }
    assert!(s.get_blob(&ids[0]).is_err(), "swept object is gone");
    let s2 = LocalStore::open(&dir).unwrap();
    assert_eq!(s2.get_blob(&ids[499]).unwrap(), data[499]);
    assert!(s2.verify_packs().is_empty());
}
