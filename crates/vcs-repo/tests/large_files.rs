//! Large-file layout (files of 16 MiB and up): round trips, dedup of an edited version, status fast path,
//! streaming checkout that never exposes a partial file, ranged and lazy reads, fsck and gc reachability.
use std::fs;
use std::path::Path;
use std::time::Duration;
use vcs_objects::{content_id, Object, ObjectId, Signature, LARGE_MAX, LARGE_MIN};
use vcs_remote::MemBackend;
use vcs_repo::Repo;
use vcs_store::ObjectStore;

const MIB: usize = 1 << 20;

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
fn object_path(r: &Repo, id: &ObjectId) -> std::path::PathBuf {
    let h = id.to_hex();
    r.worktree().join(".vcs/objects").join(&h[..2]).join(&h[2..])
}
fn no_temp_files(dir: &Path) {
    for e in fs::read_dir(dir).unwrap() {
        let n = e.unwrap().file_name().to_string_lossy().into_owned();
        assert!(!n.starts_with(".vcs-tmp"), "left a temporary file: {n}");
    }
}
fn repo_with_big(dir: &Path, data: &[u8]) -> Repo {
    let r = Repo::init(&dir.join("a")).unwrap();
    fs::write(r.worktree().join("big.bin"), data).unwrap();
    fs::write(r.worktree().join("small.txt"), "small").unwrap();
    commit_all(&r);
    r
}

#[test]
fn large_file_roundtrip_dedup_status_streaming_checkout_fsck_gc() {
    let d = tempfile::tempdir().unwrap();
    let v1 = rnd(40 * MIB, 1);
    let r = repo_with_big(d.path(), &v1);
    let c1 = r.head_commit("root").unwrap().unwrap();
    let (id, m) = manifest_of(&r);
    assert!(m.large, "a 40 MiB file uses the large layout");
    assert_eq!(id, content_id(&v1));
    assert_eq!(m.total_len as usize, v1.len());
    assert!(m.chunks.len() >= 5 && m.chunks.len() < 80, "{} chunks", m.chunks.len());
    for (i, c) in m.chunks.iter().enumerate() {
        assert!(c.len as usize <= LARGE_MAX && (c.len as usize >= LARGE_MIN || i + 1 == m.chunks.len()));
    }
    assert!(
        r.worktree().join(".vcs/features").is_file(),
        "writing a large manifest records the feature"
    );
    assert!(r.fsck().unwrap().problems.is_empty());

    // status: clean, clean after a touch (mtime only), modified after a one-byte edit and after a size change
    let clean = |r: &Repo| {
        r.status()
            .unwrap()
            .zones
            .iter()
            .all(|z| z.staged.is_empty() && z.unstaged.is_empty())
    };
    assert!(clean(&r));
    let big = r.worktree().join("big.bin");
    let f = fs::OpenOptions::new().write(true).open(&big).unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5)).unwrap();
    drop(f);
    assert!(clean(&r), "a touch must not make the file modified");
    let mut edited = v1.clone();
    edited[20 * MIB] ^= 1;
    fs::write(&big, &edited).unwrap();
    assert!(!clean(&r), "a one-byte edit must be seen");
    fs::write(&big, &v1[..v1.len() - 1]).unwrap();
    assert!(!clean(&r), "a size change must be seen");
    fs::write(&big, &v1).unwrap();
    assert!(clean(&r));

    // second version: 1 MiB edited in the middle adds only the touched chunks
    let before: std::collections::HashSet<ObjectId> = r.store().list().unwrap().into_iter().collect();
    let mut v2 = v1.clone();
    v2[19 * MIB..20 * MIB].copy_from_slice(&rnd(MIB, 9));
    fs::write(&big, &v2).unwrap();
    commit_all(&r);
    let c2 = r.head_commit("root").unwrap().unwrap();
    let (id2, m2) = manifest_of(&r);
    assert_eq!(id2, content_id(&v2));
    let new_chunks: Vec<_> = m2.chunks.iter().filter(|c| !before.contains(&c.id)).collect();
    assert!(
        !new_chunks.is_empty() && new_chunks.len() <= 3,
        "a 1 MiB edit added {} chunks of {}",
        new_chunks.len(),
        m2.chunks.len()
    );
    let new_bytes: u64 = new_chunks.iter().map(|c| c.len as u64).sum();
    assert!(new_bytes <= 3 * LARGE_MAX as u64, "{new_bytes} new bytes");

    // streaming checkout back to v1 and forward to v2 is byte-identical and leaves no temporary file
    r.checkout_commit("root", "main", &c1).unwrap();
    assert_eq!(fs::read(&big).unwrap(), v1);
    no_temp_files(r.worktree());
    r.checkout_commit("root", "main", &c2).unwrap();
    assert_eq!(fs::read(&big).unwrap(), v2);
    no_temp_files(r.worktree());
    assert!(clean(&r));

    // gc keeps every chunk of both versions; fsck is clean
    let plan = r
        .gc_plan_at(
            std::time::SystemTime::now() + 30 * Duration::from_secs(86400),
            Duration::from_secs(86400),
        )
        .unwrap();
    for c in m.chunks.iter().chain(&m2.chunks) {
        assert!(
            !plan.would_delete.iter().any(|(i, _)| *i == c.id),
            "chunk {} would be deleted",
            c.id
        );
    }
    assert!(r.fsck().unwrap().problems.is_empty());
}

#[test]
fn checkout_with_a_missing_chunk_fails_without_touching_or_half_writing_the_file() {
    let d = tempfile::tempdir().unwrap();
    let v1 = rnd(24 * MIB, 3);
    let r = repo_with_big(d.path(), &v1);
    let c1 = r.head_commit("root").unwrap().unwrap();
    let (_, m1) = manifest_of(&r);
    let mut v2 = v1.clone();
    v2[3 * MIB] ^= 0xFF;
    v2[22 * MIB] ^= 0xFF;
    fs::write(r.worktree().join("big.bin"), &v2).unwrap();
    commit_all(&r);
    // damage v1: a chunk only v1 has is gone, so restoring v1 cannot complete
    let (_, m2) = manifest_of(&r);
    let last = m1
        .chunks
        .iter()
        .rev()
        .find(|c| !m2.chunks.iter().any(|x| x.id == c.id))
        .expect("v1 has a chunk v2 lacks")
        .id;
    fs::remove_file(object_path(&r, &last)).unwrap();
    assert!(r.checkout_commit("root", "main", &c1).is_err());
    assert_eq!(
        fs::read(r.worktree().join("big.bin")).unwrap(),
        v2,
        "the existing file is untouched"
    );
    no_temp_files(r.worktree());
}

#[test]
fn ranged_reads_touch_only_the_chunks_they_need_and_lazy_fetch_pulls_just_those() {
    let d = tempfile::tempdir().unwrap();
    let data = rnd(48 * MIB, 5);
    let r = repo_with_big(d.path(), &data);
    let (mid, m) = manifest_of(&r);
    for (off, len) in [
        (0u64, 10u64),
        (MIB as u64 * 7 + 3, 5 * MIB as u64),
        (data.len() as u64 - 7, 100),
        (3, 0),
        (data.len() as u64 + 5, 4),
    ] {
        let got = r.store().get_content_range(&mid, off, len).unwrap();
        let s = (off as usize).min(data.len());
        let e = (off.saturating_add(len) as usize).min(data.len());
        assert_eq!(got, data[s..e.max(s)], "range {off}+{len}");
    }
    // publish everything, then a repo that has only the manifest reads a range lazily
    let remote = MemBackend::new();
    r.publish(&remote, None, sig()).unwrap();
    let d2 = tempfile::tempdir().unwrap();
    let b = Repo::init(&d2.path().join("b")).unwrap();
    b.fetch_missing(&remote, &mid).unwrap();
    let (off, len) = (20 * MIB as u64, 3 * MIB as u64);
    let got = b
        .with_lazy_fetch(&remote, || Ok(b.store().get_content_range(&mid, off, len)?))
        .unwrap();
    assert_eq!(got, data[off as usize..(off + len) as usize]);
    let offs = m.offsets();
    let (first, last) = m.chunk_range(&offs, off, len);
    assert!(last - first < m.chunks.len());
    for (i, c) in m.chunks.iter().enumerate() {
        assert_eq!(b.store().contains(&c.id).unwrap(), (first..last).contains(&i), "chunk {i}");
    }
}

#[test]
fn publish_clone_and_fetch_move_large_files_byte_identically() {
    let d = tempfile::tempdir().unwrap();
    let data = rnd(36 * MIB, 6);
    let r = repo_with_big(d.path(), &data);
    let remote = MemBackend::new();
    r.publish(&remote, None, sig()).unwrap();
    let c = Repo::clone_from(&remote, "mem", &d.path().join("c"), None).unwrap();
    assert_eq!(fs::read(c.worktree().join("big.bin")).unwrap(), data);
    assert!(c.fsck().unwrap().problems.is_empty());
    // publishing a second version uploads about one edited chunk, not the whole file
    let before = remote.history().len();
    let mut v2 = data.clone();
    v2[10 * MIB] ^= 1;
    fs::write(r.worktree().join("big.bin"), &v2).unwrap();
    commit_all(&r);
    r.publish(&remote, None, sig()).unwrap();
    let sent: u64 = remote.history()[before..].iter().map(|(_, d)| d.len() as u64).sum();
    assert!(sent < 3 * LARGE_MAX as u64, "second publish wrote {sent} bytes");
}
