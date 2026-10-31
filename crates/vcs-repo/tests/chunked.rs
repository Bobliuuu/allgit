//! Large files are stored as chunk manifests; status, fsck, gc, repack and remote fetch handle them.
use std::fs;
use std::time::Duration;
use vcs_objects::{content_id, Object, ObjectId, Signature};
use vcs_remote::{packfetch, Backend, MemBackend};
use vcs_repo::Repo;

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
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
fn commit_all(r: &Repo) {
    r.add(&["".into()]).unwrap();
    r.commit("m", sig()).unwrap();
}
fn object_path(r: &Repo, id: &ObjectId) -> std::path::PathBuf {
    let h = id.to_hex();
    r.worktree().join(".vcs/objects").join(&h[..2]).join(&h[2..])
}
/// Manifest id of `big.bin` in the head commit of the root zone.
fn manifest_of(r: &Repo) -> (ObjectId, vcs_objects::ChunkedBlob) {
    let head = r.refs().unwrap().head("root", "main").unwrap();
    let Object::Commit(c) = r.store().get(&head).unwrap() else {
        panic!()
    };
    let Object::Tree(t) = r.store().get(&c.root).unwrap() else {
        panic!()
    };
    let e = t.entries().iter().find(|e| e.name == "big.bin").unwrap();
    let Object::Chunked(m) = r.store().get(&e.id).unwrap() else {
        panic!("big.bin is not a chunk manifest")
    };
    (e.id, m)
}
fn repo_with_big(dir: &std::path::Path, data: &[u8]) -> Repo {
    let r = Repo::init(&dir.join("a")).unwrap();
    fs::write(r.worktree().join("big.bin"), data).unwrap();
    fs::write(r.worktree().join("small.txt"), "small").unwrap();
    commit_all(&r);
    r
}

#[test]
fn big_file_is_a_manifest_status_clean_fsck_ok_and_checkout_restores_it() {
    let d = tempfile::tempdir().unwrap();
    let data = rnd(3 << 20, 1);
    let r = repo_with_big(d.path(), &data);
    let (id, m) = manifest_of(&r);
    assert_eq!(id, content_id(&data));
    assert!(m.chunks.len() > 16);
    let st = r.status().unwrap();
    assert!(st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()));
    assert!(r.fsck().unwrap().problems.is_empty());
    // content change is detected, and restored by checkout from chunks
    let mut edited = data.clone();
    edited[1 << 20] ^= 1;
    fs::write(r.worktree().join("big.bin"), &edited).unwrap();
    assert!(r.status().unwrap().zones.iter().any(|z| !z.unstaged.is_empty()));
    // publish + clone moves the chunks through sync and materializes the file from them
    let remote = MemBackend::new();
    r.publish(&remote, None, sig()).unwrap();
    let c = Repo::clone_from(&remote, "mem", &d.path().join("c"), None).unwrap();
    assert_eq!(fs::read(c.worktree().join("big.bin")).unwrap(), data);
    assert!(c.fsck().unwrap().problems.is_empty());
}

#[test]
fn small_edit_to_a_big_file_adds_few_objects() {
    let d = tempfile::tempdir().unwrap();
    let data = rnd(4 << 20, 2);
    let r = repo_with_big(d.path(), &data);
    let before = r.store().list().unwrap().len();
    let mut edited = data.clone();
    edited[2 << 20] ^= 0xFF;
    fs::write(r.worktree().join("big.bin"), &edited).unwrap();
    commit_all(&r);
    let added = r.store().list().unwrap().len() - before;
    // new manifest + 1 or 2 chunks + tree + commit + operation records; far below the chunk count
    assert!(added < 12, "edit added {added} objects");
}

#[test]
fn fsck_reports_missing_and_corrupt_chunks_and_repack_keeps_it_valid() {
    let d = tempfile::tempdir().unwrap();
    let r = repo_with_big(d.path(), &rnd(2 << 20, 3));
    let (_, m) = manifest_of(&r);
    r.repack().unwrap();
    assert!(r.fsck().unwrap().problems.is_empty());
    // fresh repo for loose-file damage
    let d2 = tempfile::tempdir().unwrap();
    let r = repo_with_big(d2.path(), &rnd(2 << 20, 3));
    fs::remove_file(object_path(&r, &m.chunks[0].id)).unwrap();
    let p = r.fsck().unwrap().problems;
    assert!(p.iter().any(|x| x.contains("missing chunk")), "{p:?}");
    let d3 = tempfile::tempdir().unwrap();
    let r = repo_with_big(d3.path(), &rnd(2 << 20, 3));
    let path = object_path(&r, &m.chunks[1].id);
    let mut bytes = fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(&path, bytes).unwrap();
    assert!(!r.fsck().unwrap().problems.is_empty());
    // truncation of a chunk file
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    assert!(!r.fsck().unwrap().problems.is_empty());
}

#[test]
fn gc_roots_follow_manifests_to_chunks() {
    let d = tempfile::tempdir().unwrap();
    let r = repo_with_big(d.path(), &rnd(2 << 20, 4));
    let (_, m) = manifest_of(&r);
    let plan = r
        .gc_plan_at(
            std::time::SystemTime::now() + 30 * Duration::from_secs(86400),
            Duration::from_secs(86400),
        )
        .unwrap();
    for c in &m.chunks {
        assert!(
            !plan.would_delete.iter().any(|(i, _)| *i == c.id),
            "chunk {} would be deleted",
            c.id
        );
    }
    assert!(plan.would_delete.is_empty(), "{:?}", plan.would_delete);
}

#[test]
fn lazy_fetch_reads_one_chunk_from_a_remote_pack_by_range() {
    let d = tempfile::tempdir().unwrap();
    let data = rnd(3 << 20, 5);
    let r = repo_with_big(d.path(), &data);
    let (mid, m) = manifest_of(&r);
    r.repack().unwrap();
    let pack = fs::read_dir(r.worktree().join(".vcs/objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "pack"))
        .unwrap();
    // The remote holds only a pack: no loose objects at all.
    let remote = MemBackend::new();
    packfetch::upload_pack(&remote, "p1", &pack).unwrap();
    let pack_len = fs::metadata(&pack).unwrap().len();
    // A second repo that lacks the chunk fetches just that chunk on demand.
    let d2 = tempfile::tempdir().unwrap();
    let b = Repo::init(&d2.path().join("b")).unwrap();
    let before = remote.bytes_read();
    b.fetch_missing(&remote, &m.chunks[7].id).unwrap();
    let used = remote.bytes_read() - before;
    assert!(b.store().contains(&m.chunks[7].id).unwrap());
    assert!(used < pack_len / 20, "read {used} of {pack_len} pack bytes for one chunk");
    // the manifest too, then with_lazy_fetch pulls the rest while reading content
    b.fetch_missing(&remote, &mid).unwrap();
    let got = b.with_lazy_fetch(&remote, || Ok(b.store().get_content(&mid)?)).unwrap();
    assert_eq!(got, data);
    // a tampered remote pack never reaches the store
    let d3 = tempfile::tempdir().unwrap();
    let c = Repo::init(&d3.path().join("c")).unwrap();
    let mut bad = fs::read(&pack).unwrap();
    for x in bad.iter_mut().skip(12) {
        *x ^= 0xFF;
    }
    remote.tamper("packs/p1.pack", bad);
    assert!(c.fetch_missing(&remote, &m.chunks[3].id).is_err());
    assert!(!c.store().contains(&m.chunks[3].id).unwrap());
    let _: &dyn Backend = &remote;
}
