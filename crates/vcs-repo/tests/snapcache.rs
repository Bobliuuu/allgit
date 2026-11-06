//! Materialized-tree snapshot cache (#490): a checkout restored from a snapshot equals the normal
//! checkout (files, bytes, exec bits, links, clean status), a different tree never sees a stale
//! snapshot, a damaged or partial snapshot is dropped and the normal path runs, and the cache is
//! off when asked. Unix only (copy-on-write clones).
#![cfg(unix)]
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use vcs_objects::Signature;
use vcs_remote::MemBackend;
use vcs_repo::Repo;

static ENV: Mutex<()> = Mutex::new(());

/// Serializes the tests (they set process environment) and resets it.
fn env(vars: &[(&str, &str)]) -> MutexGuard<'static, ()> {
    let g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    for k in ["VCS_SNAPSHOT", "VCS_SNAP_DIR", "VCS_SNAPSHOT_MIN_FILES", "VCS_SNAPSHOT_KEEP"] {
        std::env::remove_var(k);
    }
    std::env::set_var("VCS_SNAPSHOT_MIN_FILES", "1");
    for (k, v) in vars {
        std::env::set_var(k, v);
    }
    g
}

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}

#[derive(Debug, PartialEq)]
enum Node {
    File(Vec<u8>, bool),
    Link(PathBuf),
}

fn tree(root: &Path) -> BTreeMap<String, Node> {
    fn go(root: &Path, dir: &Path, out: &mut BTreeMap<String, Node>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if rel == ".vcs" {
                continue;
            }
            let m = fs::symlink_metadata(&p).unwrap();
            if m.is_dir() {
                go(root, &p, out)
            } else if m.file_type().is_symlink() {
                out.insert(rel, Node::Link(fs::read_link(&p).unwrap()));
            } else {
                out.insert(rel, Node::File(fs::read(&p).unwrap(), m.permissions().mode() & 0o111 != 0));
            }
        }
    }
    let mut out = BTreeMap::new();
    go(root, root, &mut out);
    out
}

fn assert_clean(r: &Repo, ctx: &str) {
    let st = r.status().unwrap();
    for z in &st.zones {
        assert!(
            z.staged.is_empty() && z.unstaged.is_empty(),
            "{ctx}: not clean {:?} {:?}",
            z.staged,
            z.unstaged
        );
    }
    assert!(st.untracked.is_empty(), "{ctx}: untracked {:?}", st.untracked);
}

fn put(root: &Path, p: &str, body: &str) {
    let abs = root.join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, body).unwrap();
}

/// A repository with `n` files over several directories, an executable and a symlink, plus an
/// `engine` zone.
fn repo(dir: &Path, n: usize) -> Repo {
    let r = Repo::init(&dir.join("main")).unwrap();
    r.create_zone("engine", "engine").unwrap();
    let w = r.worktree().to_path_buf();
    for i in 0..n {
        put(&w, &format!("src/d{}/f{i}.txt", i % 7), &format!("body {i}\n"));
    }
    put(&w, "run.sh", "#!/bin/sh\n");
    fs::set_permissions(w.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("src/d0/f0.txt", w.join("link")).unwrap();
    put(&w, "engine/core.rs", "core\n");
    put(&w, "top.md", "top\n");
    r.add(&["".into()]).unwrap();
    r.commit("init", sig()).unwrap();
    r
}

/// Whether the temp filesystem supports copy-on-write clones. GitHub's ubuntu runners use ext4 (no reflink), where the
/// cache correctly captures nothing, so tests that need a captured snapshot skip there with a note instead of failing.
fn cow_supported() -> bool {
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (d.path().join("a"), d.path().join("b"));
    fs::write(&a, "x").unwrap();
    let flags: &[&str] = if cfg!(target_os = "macos") {
        &["-c"]
    } else {
        &["--reflink=always"]
    };
    std::process::Command::new("cp")
        .args(flags)
        .arg(&a)
        .arg(&b)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

macro_rules! need_cow {
    () => {
        if !cow_supported() {
            eprintln!("skipped: this filesystem has no copy-on-write clones, so no snapshot can be captured");
            return;
        }
    };
}

fn snap_dirs(r: &Repo) -> Vec<PathBuf> {
    let s = r.worktree().join(".vcs/snap");
    match fs::read_dir(&s) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| !p.file_name().unwrap().to_string_lossy().starts_with(".tmp-"))
            .collect(),
        Err(_) => vec![],
    }
}

fn mtime(p: &Path) -> (i64, i64) {
    let m = fs::symlink_metadata(p).unwrap();
    (m.mtime(), m.mtime_nsec())
}

#[test]
fn restore_equals_normal_checkout_and_really_clones() {
    need_cow!();
    let _g = env(&[]);
    let d = tempfile::tempdir().unwrap();
    let a = repo(d.path(), 300);
    // first fresh checkout captures a snapshot, the second restores from it
    let w1 = a.worktree_add(&d.path().join("w1"), Some("b1")).unwrap().path;
    assert_eq!(snap_dirs(&a).len(), 1, "snapshot captured");
    let w2 = a.worktree_add(&d.path().join("w2"), Some("b2")).unwrap().path;
    // a clone keeps mtime; a rewritten file does not: proves w2 came from the snapshot
    assert_eq!(mtime(&w1.join("src/d3/f10.txt")), mtime(&w2.join("src/d3/f10.txt")));
    // the same tree with the cache off
    std::env::set_var("VCS_SNAPSHOT", "0");
    let w3 = a.worktree_add(&d.path().join("w3"), Some("b3")).unwrap().path;
    assert_ne!(mtime(&w1.join("src/d3/f10.txt")), mtime(&w3.join("src/d3/f10.txt")));
    let (t1, t2, t3) = (tree(&w1), tree(&w2), tree(&w3));
    assert_eq!(t2, t3, "restored tree differs from the normal checkout");
    assert_eq!(t1, t3);
    assert!(matches!(t2["link"], Node::Link(_)) && t2["run.sh"] == Node::File(b"#!/bin/sh\n".to_vec(), true));
    for w in [&w1, &w2, &w3] {
        assert_clean(&Repo::discover(w).unwrap(), &w.display().to_string());
    }
    // the restamped index is usable: edit + commit in the restored worktree
    let b = Repo::discover(&w2).unwrap();
    put(&w2, "src/d1/f1.txt", "edited\n");
    let st = b.status().unwrap();
    assert_eq!(st.zones.iter().map(|z| z.unstaged.len()).sum::<usize>(), 1);
    b.add(&["".into()]).unwrap();
    b.commit("e", sig()).unwrap();
    assert_clean(&b, "after commit");
}

#[test]
fn a_different_tree_never_uses_a_stale_snapshot_and_edits_do_not_leak_in() {
    need_cow!();
    let _g = env(&[]);
    let d = tempfile::tempdir().unwrap();
    let a = repo(d.path(), 200);
    let w1 = a.worktree_add(&d.path().join("w1"), Some("b1")).unwrap().path;
    // editing the worktree the snapshot was cloned from must not change the snapshot (CoW)
    put(&w1, "src/d0/f0.txt", "EDITED AFTER CAPTURE\n");
    let w2 = a.worktree_add(&d.path().join("w2"), Some("b2")).unwrap().path;
    assert_eq!(fs::read_to_string(w2.join("src/d0/f0.txt")).unwrap(), "body 0\n");
    assert_clean(&Repo::discover(&w2).unwrap(), "w2");
    // a new commit changes the tree key: the new branch gets the new content
    put(a.worktree(), "src/d0/f0.txt", "v2\n");
    put(a.worktree(), "added.txt", "new\n");
    a.add(&["".into()]).unwrap();
    a.commit("v2", sig()).unwrap();
    let w3 = a.worktree_add(&d.path().join("w3"), Some("b3")).unwrap().path;
    assert_eq!(fs::read_to_string(w3.join("src/d0/f0.txt")).unwrap(), "v2\n");
    assert!(w3.join("added.txt").exists());
    let w4 = a.worktree_add(&d.path().join("w4"), Some("b4")).unwrap().path;
    assert_eq!(tree(&w3), tree(&w4));
    assert_clean(&Repo::discover(&w4).unwrap(), "w4");
    assert_eq!(snap_dirs(&a).len(), 2, "one snapshot per distinct tree");
}

fn damage_then_checkout(damage: impl Fn(&Path)) {
    need_cow!();
    let d = tempfile::tempdir().unwrap();
    let a = repo(d.path(), 200);
    let w1 = a.worktree_add(&d.path().join("w1"), Some("b1")).unwrap().path;
    let want = tree(&w1);
    let snaps = snap_dirs(&a);
    assert_eq!(snaps.len(), 1);
    damage(&snaps[0]);
    let w2 = a.worktree_add(&d.path().join("w2"), Some("b2")).unwrap().path;
    assert_eq!(tree(&w2), want, "fell back to a correct checkout");
    assert_clean(&Repo::discover(&w2).unwrap(), "w2");
    // and the next one works again (snapshot rebuilt or ignored, never half-used)
    let w3 = a.worktree_add(&d.path().join("w3"), Some("b3")).unwrap().path;
    assert_eq!(tree(&w3), want);
    assert_clean(&Repo::discover(&w3).unwrap(), "w3");
}

#[test]
fn corrupt_content_size_is_detected() {
    let _g = env(&[]);
    damage_then_checkout(|s| fs::write(s.join("tree/src/d2/f2.txt"), "much longer than before, different size").unwrap());
}

#[test]
fn missing_file_in_snapshot_is_detected() {
    let _g = env(&[]);
    damage_then_checkout(|s| fs::remove_file(s.join("tree/src/d2/f2.txt")).unwrap());
}

#[test]
fn extra_file_in_snapshot_is_detected() {
    let _g = env(&[]);
    damage_then_checkout(|s| fs::write(s.join("tree/src/d2/stray.txt"), "x").unwrap());
}

#[test]
fn missing_top_level_entry_is_a_partial_snapshot() {
    let _g = env(&[]);
    damage_then_checkout(|s| fs::remove_dir_all(s.join("tree/src")).unwrap());
}

#[test]
fn truncated_or_missing_manifest_is_ignored() {
    let _g = env(&[]);
    damage_then_checkout(|s| {
        let m = fs::read(s.join("manifest")).unwrap();
        fs::write(s.join("manifest"), &m[..m.len() / 2]).unwrap();
    });
    damage_then_checkout(|s| fs::remove_file(s.join("manifest")).unwrap());
}

#[test]
fn interrupted_capture_leaves_no_usable_snapshot() {
    need_cow!();
    let _g = env(&[]);
    let d = tempfile::tempdir().unwrap();
    let a = repo(d.path(), 100);
    // a crashed capture: tmp directory with a tree but no manifest, and a snapshot dir without one
    let tmp = a.worktree().join(".vcs/snap/.tmp-deadbeef-1-1");
    fs::create_dir_all(tmp.join("tree")).unwrap();
    fs::write(tmp.join("tree/x"), "x").unwrap();
    let w1 = a.worktree_add(&d.path().join("w1"), Some("b1")).unwrap().path;
    let want = tree(&w1);
    assert_eq!(snap_dirs(&a).len(), 1);
    let w2 = a.worktree_add(&d.path().join("w2"), Some("b2")).unwrap().path;
    assert_eq!(tree(&w2), want);
    assert_clean(&Repo::discover(&w2).unwrap(), "w2");
}

#[test]
fn off_switch_and_size_threshold_skip_the_cache() {
    let _g = env(&[("VCS_SNAPSHOT", "0")]);
    let d = tempfile::tempdir().unwrap();
    let a = repo(d.path(), 100);
    let w1 = a.worktree_add(&d.path().join("w1"), Some("b1")).unwrap().path;
    assert!(snap_dirs(&a).is_empty(), "disabled: nothing captured");
    assert_clean(&Repo::discover(&w1).unwrap(), "w1");
    std::env::remove_var("VCS_SNAPSHOT");
    std::env::set_var("VCS_SNAPSHOT_MIN_FILES", "100000");
    a.worktree_add(&d.path().join("w2"), Some("b2")).unwrap();
    assert!(snap_dirs(&a).is_empty(), "small tree: nothing captured");
}

#[test]
fn untracked_files_in_the_target_directory_take_the_normal_path() {
    let _g = env(&[]);
    let d = tempfile::tempdir().unwrap();
    let a = repo(d.path(), 100);
    a.worktree_add(&d.path().join("w1"), Some("b1")).unwrap();
    // an existing empty directory is fine; one holding a stray file is refused by worktree add
    // itself, so the clone must be exact in the empty-directory case
    let w2 = d.path().join("w2");
    fs::create_dir(&w2).unwrap();
    a.worktree_add(&w2, Some("b2")).unwrap();
    assert_eq!(tree(&w2), tree(&d.path().join("w1")));
    assert_clean(&Repo::discover(&w2).unwrap(), "w2");
}

#[test]
fn zoned_clone_restores_from_a_shared_store_and_equals_the_normal_clone() {
    need_cow!();
    let d = tempfile::tempdir().unwrap();
    let store = d.path().join("store");
    let _g = env(&[("VCS_SNAP_DIR", store.to_str().unwrap())]);
    let o = repo(d.path(), 300);
    let be = MemBackend::new();
    o.publish(&be, None, sig()).unwrap();
    let c1 = Repo::clone_from(&be, "mem", &d.path().join("c1"), None).unwrap();
    assert!(
        fs::read_dir(&store).unwrap().next().is_some(),
        "first clone captured into VCS_SNAP_DIR"
    );
    let c2 = Repo::clone_from(&be, "mem", &d.path().join("c2"), None).unwrap();
    assert_eq!(
        mtime(&c1.worktree().join("src/d3/f10.txt")),
        mtime(&c2.worktree().join("src/d3/f10.txt")),
        "c2 was cloned"
    );
    std::env::set_var("VCS_SNAPSHOT", "0");
    let c3 = Repo::clone_from(&be, "mem", &d.path().join("c3"), None).unwrap();
    assert_eq!(tree(c2.worktree()), tree(c3.worktree()));
    assert!(tree(c3.worktree()).contains_key("engine/core.rs"), "zone files present");
    for c in [&c1, &c2, &c3] {
        assert_clean(c, "clone");
    }
    // only one zone: a different key, never the full-clone snapshot
    std::env::remove_var("VCS_SNAPSHOT");
    let p = Repo::clone_from(&be, "mem", &d.path().join("p"), Some(&["engine".to_string()])).unwrap();
    let t = tree(p.worktree());
    assert!(t.contains_key("engine/core.rs") && !t.contains_key("src/d0/f0.txt"));
    assert_clean(&p, "partial");
}

#[test]
fn lru_keeps_only_the_configured_number_of_snapshots() {
    need_cow!();
    let _g = env(&[("VCS_SNAPSHOT_KEEP", "1")]);
    let d = tempfile::tempdir().unwrap();
    let a = repo(d.path(), 100);
    a.worktree_add(&d.path().join("w1"), Some("b1")).unwrap();
    put(a.worktree(), "extra.txt", "x\n");
    a.add(&["".into()]).unwrap();
    a.commit("more", sig()).unwrap();
    a.worktree_add(&d.path().join("w2"), Some("b2")).unwrap();
    assert_eq!(snap_dirs(&a).len(), 1);
}
