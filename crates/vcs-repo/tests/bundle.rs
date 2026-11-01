//! Bundles: roundtrip, incremental, hostile input, crash atomicity (docs/bundle.md).

use std::fs;
use std::path::Path;
use vcs_objects::Signature;
use vcs_repo::bundle::{self, MAGIC};
use vcs_repo::{failpoint, Repo};

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
fn source(dir: &Path) -> Repo {
    let a = Repo::init(&dir.join("a")).unwrap();
    a.create_zone("engine", "engine").unwrap();
    a.create_zone("assets", "assets").unwrap();
    write(&a, "README", "readme");
    write(&a, "engine/core.rs", "core v1");
    write(&a, "assets/big.bin", &"B".repeat(50_000));
    commit_all(&a, "init");
    a
}

#[test]
fn create_verify_clone_roundtrip_multi_zone() {
    let d = tempfile::tempdir().unwrap();
    let a = source(d.path());
    let f = d.path().join("all.bundle");
    let info = a.bundle_create(&f, &[], None).unwrap();
    assert_eq!(info.heads.len(), 3);
    assert!(info.prerequisites.is_empty());
    let v = bundle::verify_file(&f).unwrap();
    assert_eq!(v.objects, info.objects);
    assert_eq!(v.bytes, fs::metadata(&f).unwrap().len());
    // deterministic
    let f2 = d.path().join("again.bundle");
    a.bundle_create(&f2, &[], None).unwrap();
    assert_eq!(fs::read(&f).unwrap(), fs::read(&f2).unwrap());
    let b = Repo::clone_from_bundle(&f, &d.path().join("b"), sig()).unwrap();
    assert_eq!(fs::read_to_string(b.worktree().join("engine/core.rs")).unwrap(), "core v1");
    assert_eq!(fs::read_to_string(b.worktree().join("README")).unwrap(), "readme");
    assert_eq!(fs::read(b.worktree().join("assets/big.bin")).unwrap().len(), 50_000);
    assert!(b.fsck().unwrap().problems.is_empty());
    let st = b.status().unwrap();
    assert!(st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()) && st.untracked.is_empty());
    assert_eq!(b.remote_refs().unwrap().branches.len(), 3);
}

#[test]
fn zone_selector_limits_the_bundle() {
    let d = tempfile::tempdir().unwrap();
    let a = source(d.path());
    let f = d.path().join("e.bundle");
    let info = a.bundle_create(&f, &["engine".into()], None).unwrap();
    assert_eq!(info.heads.len(), 1);
    assert_eq!(info.zones.len(), 1);
    assert!(a.bundle_create(&f, &["nope".into()], None).is_err());
    let one = a.bundle_create(&f, &["engine/main".into()], None).unwrap();
    assert_eq!(one.heads[0].branch, "main");
}

#[test]
fn incremental_bundle_needs_its_prerequisite_and_fetches_atomically() {
    let d = tempfile::tempdir().unwrap();
    let a = source(d.path());
    let full = d.path().join("full.bundle");
    a.bundle_create(&full, &[], None).unwrap();
    let b = Repo::clone_from_bundle(&full, &d.path().join("b"), sig()).unwrap();
    // new work in two zones
    write(&a, "engine/core.rs", "core v2");
    write(&a, "assets/new.bin", "new");
    commit_all(&a, "second");
    let inc = d.path().join("inc.bundle");
    let info = a.bundle_create(&inc, &[], Some("HEAD~1")).unwrap();
    assert!(!info.prerequisites.is_empty());
    assert!(info.objects < bundle::verify_file(&full).unwrap().objects + 10);
    // cannot be cloned, and a repo lacking the prerequisite refuses it without changes
    assert!(Repo::clone_from_bundle(&inc, &d.path().join("c"), sig()).is_err());
    let empty = Repo::init(&d.path().join("e")).unwrap();
    let before = empty.remote_refs().unwrap().branches.len();
    assert!(empty.bundle_fetch(&inc, sig()).is_err());
    assert_eq!(empty.remote_refs().unwrap().branches.len(), before);
    // the clone has the prerequisite: all zones advance together
    let rep = b.bundle_fetch(&inc, sig()).unwrap();
    assert_eq!(rep.updated.len(), 2); // root did not change
    let rr = b.remote_refs().unwrap();
    let ar = a.refs().unwrap();
    for ((z, br), id) in &ar.branches {
        assert_eq!(rr.head(z, br), Some(*id), "{z}/{br}");
    }
    assert!(b.fsck().unwrap().problems.is_empty());
    // fetching again is a no-op
    assert!(b.bundle_fetch(&inc, sig()).unwrap().updated.is_empty());
}

fn rehash(bytes: &mut [u8]) {
    let n = bytes.len() - 32;
    let h = blake3::hash(&bytes[..n]);
    bytes[n..].copy_from_slice(h.as_bytes());
}

#[test]
fn truncated_and_corrupt_files_fail_cleanly() {
    let d = tempfile::tempdir().unwrap();
    let a = source(d.path());
    let f = d.path().join("x.bundle");
    a.bundle_create(&f, &[], None).unwrap();
    let good = fs::read(&f).unwrap();
    let t = d.path().join("t.bundle");
    for n in (0..good.len())
        .step_by(1.max(good.len() / 400))
        .chain([good.len() - 1, good.len() - 32, 8, 15, 16])
    {
        fs::write(&t, &good[..n]).unwrap();
        assert!(bundle::verify_file(&t).is_err(), "truncated to {n} accepted");
    }
    // single bit flips anywhere are caught by the trailer
    for i in (0..good.len()).step_by(1.max(good.len() / 300)) {
        let mut m = good.clone();
        m[i] ^= 0x10;
        fs::write(&t, &m).unwrap();
        assert!(bundle::verify_file(&t).is_err(), "flip at {i} accepted");
    }
    fs::write(&t, b"hello").unwrap();
    assert!(!bundle::is_bundle_file(&t));
    assert!(bundle::is_bundle_file(&f));
    assert_eq!(&good[..8], MAGIC);
}

/// Mutations that keep the trailer valid exercise the structural checks, not just the hash.
/// Whatever happens it must not panic or hang, and a repository must stay consistent.
#[test]
fn mutation_with_recomputed_hash_never_panics_or_corrupts() {
    let d = tempfile::tempdir().unwrap();
    let a = source(d.path());
    let f = d.path().join("x.bundle");
    a.bundle_create(&f, &[], None).unwrap();
    let good = fs::read(&f).unwrap();
    let t = d.path().join("m.bundle");
    let target = Repo::init(&d.path().join("t")).unwrap();
    let mut seed = 0x9e3779b97f4a7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let (mut rejected, mut accepted) = (0, 0);
    for round in 0..600 {
        let mut m = good.clone();
        let body = m.len() - 32;
        match round % 4 {
            0 => {
                let i = (next() as usize) % body;
                m[i] ^= 1 << (next() % 8);
            }
            1 => {
                let i = (next() as usize) % body;
                m[i] = next() as u8;
                let j = (next() as usize) % body;
                m[j] = next() as u8;
            }
            2 => {
                // overwrite a 4-byte field with a huge value (length fields)
                let i = (next() as usize) % (body - 4);
                m[i..i + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            }
            _ => {
                let cut = (next() as usize) % body;
                m.drain(cut..cut + 1 + (next() as usize % 8).min(body - cut - 1));
            }
        }
        rehash(&mut m);
        fs::write(&t, &m).unwrap();
        match bundle::verify_file(&t) {
            Err(_) => rejected += 1,
            Ok(_) => accepted += 1,
        }
        // applying must be all-or-nothing whatever the verdict
        let before = target.remote_refs().unwrap().branches.clone();
        if target.bundle_fetch(&t, sig()).is_err() {
            assert_eq!(target.remote_refs().unwrap().branches, before);
        }
    }
    assert!(rejected > 300, "only {rejected} of 600 mutants rejected ({accepted} accepted)");
    assert!(target.fsck().unwrap().problems.is_empty());
}

#[test]
fn oversized_header_length_and_unknown_version_are_refused() {
    let d = tempfile::tempdir().unwrap();
    let t = d.path().join("h.bundle");
    // header length claims 4 GiB in a 100 byte file
    let mut b = Vec::new();
    b.extend_from_slice(MAGIC);
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&u32::MAX.to_le_bytes());
    b.extend_from_slice(&[0u8; 44]);
    b.extend_from_slice(&[0u8; 32]);
    rehash(&mut b);
    fs::write(&t, &b).unwrap();
    let e = bundle::verify_file(&t).unwrap_err().to_string();
    assert!(e.contains("header length"), "{e}");
    // unknown version
    let mut b2 = b.clone();
    b2[8..12].copy_from_slice(&7u32.to_le_bytes());
    rehash(&mut b2);
    fs::write(&t, &b2).unwrap();
    assert!(bundle::verify_file(&t).unwrap_err().to_string().contains("version"));
}

#[test]
fn crash_during_multi_zone_apply_leaves_all_or_nothing() {
    let d = tempfile::tempdir().unwrap();
    let a = source(d.path());
    let f = d.path().join("all.bundle");
    a.bundle_create(&f, &[], None).unwrap();
    // count the state-file writes of a successful apply
    let probe = Repo::init(&d.path().join("probe")).unwrap();
    failpoint::reset();
    probe.bundle_fetch(&f, sig()).unwrap();
    let writes = failpoint::writes();
    assert!(writes >= 2, "zones + remote-refs expected, saw {writes}");
    for n in 0..writes {
        let t = Repo::init(&d.path().join(format!("t{n}"))).unwrap();
        failpoint::reset();
        failpoint::arm(Some(n));
        let r = t.bundle_fetch(&f, sig());
        failpoint::arm(None);
        assert!(r.is_err(), "crash {n} not injected");
        let rr = t.remote_refs().unwrap();
        assert!(
            rr.branches.is_empty(),
            "crash {n}: partial refs {:?}",
            rr.branches.keys().collect::<Vec<_>>()
        );
        assert!(t.fsck().unwrap().problems.is_empty());
        // retry completes everything
        t.bundle_fetch(&f, sig()).unwrap();
        assert_eq!(t.remote_refs().unwrap().branches.len(), 3);
        assert!(t.fsck().unwrap().problems.is_empty());
    }
}
