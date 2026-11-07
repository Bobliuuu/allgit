//! Clone-side store request counts (full clone and zone clone), asserted with a counting backend,
//! plus clone correctness and corruption handling on the whole-small-pack path.

use std::fs;
use std::sync::Arc;
use std::time::Duration;
use vcs_objects::Signature;
use vcs_remote::timeline::TimelineBackend;
use vcs_remote::{Backend, MemBackend};
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

fn noise(seed: u64, n: usize) -> String {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (b'a' + (x % 26) as u8) as char
        })
        .collect()
}

fn big(remote: &dyn Backend, dir: &std::path::Path, files: usize, size: usize) -> Repo {
    let a = Repo::init(&dir.join("a")).unwrap();
    a.create_zone("engine", "engine").unwrap();
    a.create_zone("ui", "ui").unwrap();
    for i in 0..files {
        let z = if i % 2 == 0 { "engine" } else { "ui" };
        write(&a, &format!("{z}/d{}/f{i}.rs", i % 40), &noise(i as u64, size));
    }
    a.add(&["".into()]).unwrap();
    a.commit("big", sig()).unwrap();
    a.publish(remote, None, sig()).unwrap();
    a
}

fn seeded(remote: &dyn Backend, dir: &std::path::Path, publishes: usize) -> Repo {
    let a = Repo::init(&dir.join("a")).unwrap();
    a.create_zone("engine", "engine").unwrap();
    a.create_zone("ui", "ui").unwrap();
    for n in 0..publishes {
        for i in 0..6 {
            write(&a, &format!("engine/f{i}.rs"), &format!("engine {i} v{n} {}", "x".repeat(300 + n)));
            write(&a, &format!("ui/g{i}.rs"), &format!("ui {i} v{n} {}", "y".repeat(300 + n)));
        }
        a.add(&["".into()]).unwrap();
        a.commit(&format!("c{n}"), sig()).unwrap();
        a.publish(remote, None, sig()).unwrap();
    }
    a
}

fn files_of(root: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn go(root: &std::path::Path, dir: &std::path::Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if rel.starts_with(".vcs") {
                continue;
            }
            if p.is_dir() {
                go(root, &p, out);
            } else {
                out.insert(rel, fs::read(&p).unwrap());
            }
        }
    }
    let mut out = Default::default();
    go(root, root, &mut out);
    out
}

fn packs_on(remote: &MemBackend) -> usize {
    remote.list("packs/").unwrap().iter().filter(|k| k.ends_with(".idx")).count()
}

/// Latency 25 ms (well above scheduler noise on a loaded runner) so the timeline can tell parallel requests from a chain.
fn timed(remote: &Arc<MemBackend>) -> TimelineBackend {
    let inner: Arc<dyn Backend> = remote.clone();
    TimelineBackend::new(inner, Duration::from_millis(25))
}

#[test]
fn many_small_packs_cost_two_requests_each_in_one_wave() {
    let d = tempfile::tempdir().unwrap();
    let remote = Arc::new(MemBackend::new());
    let a = seeded(&*remote, d.path(), 10);
    let n = packs_on(&remote);
    assert!(n >= 8, "one pack per publish: {n}");
    for (name, zones) in [("full", None), ("zone", Some(vec!["engine".to_string()]))] {
        let tl = timed(&remote);
        let b = Repo::clone_from(&tl, "mem", &d.path().join(name), zones.as_deref()).unwrap();
        // format marker + manifest + LIST + (index + whole pack) per pack; nothing else, no trailer probes
        assert_eq!(tl.requests(), 3 + 2 * n, "{name}");
        // manifest/LIST, then indexes and packs: a short chain, not one round per pack
        assert!(tl.depth() <= 8, "{name}: depth {}", tl.depth());
        let rep = b.fsck().unwrap();
        assert!(rep.problems.is_empty(), "{:?}", rep.problems);
        let got = files_of(b.worktree());
        let want: std::collections::BTreeMap<_, _> = files_of(a.worktree())
            .into_iter()
            .filter(|(p, _)| zones.is_none() || p.starts_with("engine/"))
            .collect();
        assert_eq!(got, want, "{name}: checkout equals the source");
    }
}

#[test]
fn a_big_pack_is_read_in_few_ranges() {
    let d = tempfile::tempdir().unwrap();
    let remote = Arc::new(MemBackend::new());
    big(&*remote, d.path(), 2000, 4096);
    assert_eq!(packs_on(&remote), 1);
    let tl = timed(&remote);
    let b = Repo::clone_from(&tl, "mem", &d.path().join("c"), None).unwrap();
    // format marker, manifest, LIST, index, trailer, commit range, tree range and at most 8 file ranges
    assert!(tl.requests() <= 15, "{} requests", tl.requests());
    assert!(b.fsck().unwrap().problems.is_empty());
    assert_eq!(files_of(b.worktree()).len(), 2000);
}

fn flip(remote: &MemBackend, key: &str, at: impl Fn(usize) -> usize) {
    let mut d = remote.get(key).unwrap().unwrap().data;
    let i = at(d.len());
    d[i] ^= 0xFF;
    remote.tamper(key, d);
}

fn first_pack(remote: &MemBackend) -> String {
    remote.list("packs/").unwrap().into_iter().find(|k| k.ends_with(".pack")).unwrap()
}

#[test]
fn a_corrupt_small_pack_fails_the_clone() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    seeded(&remote, d.path(), 3);
    // a flipped byte in the middle: entry or hash verification catches it
    flip(&remote, &first_pack(&remote), |n| n / 2);
    let r = Repo::clone_from(&remote, "mem", &d.path().join("c"), None);
    assert!(r.is_err(), "corrupt pack body must not clone");
}

#[test]
fn a_bad_trailer_or_truncation_of_a_small_pack_fails_the_clone() {
    for what in ["trailer", "truncated", "extended"] {
        let d = tempfile::tempdir().unwrap();
        let remote = MemBackend::new();
        seeded(&remote, d.path(), 3);
        // every pack: the checkout's file reads (not the history walk) are what check a trailer
        for key in remote.list("packs/").unwrap().into_iter().filter(|k| k.ends_with(".pack")) {
            let mut v = remote.get(&key).unwrap().unwrap().data;
            match what {
                "trailer" => *v.last_mut().unwrap() ^= 0xFF,
                "truncated" => v.truncate(v.len() - 5),
                _ => v.extend_from_slice(b"junk"),
            }
            remote.tamper(&key, v);
        }
        let r = Repo::clone_from(&remote, "mem", &d.path().join("c"), None);
        assert!(r.is_err(), "{what}: damaged pack must not clone");
    }
}

#[test]
fn a_pack_removed_between_list_and_read_is_retried() {
    // compaction removes packs after the clone listed them: the clone re-lists and still gets everything
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new().with_gc();
    remote.set_now_ms(1_000_000);
    let a = seeded(&remote, d.path(), 6);
    let want = files_of(a.worktree());
    let rep = vcs_remote::compact::compact(
        &remote,
        &vcs_remote::CompactOptions {
            dry_run: false,
            all: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rep.packs_after, 1);
    let b = Repo::clone_from(&remote, "mem", &d.path().join("c"), None).unwrap();
    assert_eq!(files_of(b.worktree()), want);
    assert!(b.fsck().unwrap().problems.is_empty());
}
