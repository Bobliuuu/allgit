//! Single-object and listing read paths that must not walk the whole tree: `show`, `<rev>:<path>`,
//! and listings.
use std::fs;
use vcs_objects::{EntryMode, Signature};
use vcs_repo::Repo;

fn sig(t: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: t,
    }
}

fn setup() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    (d, r)
}

fn put(r: &Repo, p: &str, content: &str) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, content).unwrap();
}

fn commit_all(r: &Repo, msg: &str, t: u64) {
    r.add(&["".into()]).unwrap();
    r.commit(msg, sig(t)).unwrap();
}

/// Two commits over a tree with nested directories, names that sort differently as paths than as
/// tree entries (`a` the directory next to `a.txt`), and a second zone.
fn two_commits() -> (tempfile::TempDir, Repo) {
    let (d, r) = setup();
    put(&r, "a/x.txt", "x\n");
    put(&r, "a.txt", "dot\n");
    put(&r, "a/deep/er/f.txt", "deep\n");
    put(&r, "same/keep.txt", "keep\n");
    put(&r, "gone.txt", "bye\n");
    commit_all(&r, "one", 1);
    r.create_zone("lib", "libs").unwrap();
    put(&r, "libs/l.txt", "l\n");
    put(&r, "a/x.txt", "x2\n");
    put(&r, "a/deep/er/new.txt", "new\n");
    fs::remove_file(r.worktree().join("gone.txt")).unwrap();
    commit_all(&r, "two", 2);
    (d, r)
}

#[test]
fn show_diff_equals_the_diff_of_the_two_full_listings() {
    let (_d, r) = two_commits();
    let rep = r.show("root", "HEAD").unwrap();
    let old = r.rev_files("HEAD~1").unwrap();
    let new = r.rev_files("HEAD").unwrap();
    // the root zone owns everything except libs/
    let in_root = |p: &String| !p.starts_with("libs/");
    let mut paths: std::collections::BTreeSet<&String> = old.keys().filter(|p| in_root(p)).collect();
    paths.extend(new.keys().filter(|p| in_root(p)));
    let mut want = String::new();
    for p in paths {
        let (o, n) = (old.get(p), new.get(p));
        if o == n {
            continue;
        }
        let ob = o.map(|(_, i)| r.content(i).unwrap());
        let nb = n.map(|(_, i)| r.content(i).unwrap());
        want.push_str(&vcs_repo::diff::unified(p, ob.as_deref(), nb.as_deref()));
    }
    assert!(!want.is_empty());
    assert_eq!(rep.diff, want);
    assert!(rep.diff.contains("a/deep/er/new.txt") && rep.diff.contains("gone.txt") && rep.diff.contains("a/x.txt"));
    assert!(!rep.diff.contains("keep.txt"), "an unchanged subtree must not appear");
}

#[test]
fn rev_file_entry_matches_the_full_listing_for_every_path_and_for_misses() {
    let (_d, r) = two_commits();
    for rev in ["HEAD", "HEAD~1"] {
        let all = r.rev_files(rev).unwrap();
        for (p, v) in &all {
            assert_eq!(r.rev_file_entry(rev, p).unwrap(), Some(*v), "{rev}:{p}");
        }
        for miss in [
            "",
            "a",
            "a/deep",
            "a/x.txt/inside",
            "nope",
            "a/nope.txt",
            "libs",
            "libs/none.txt",
            "a.txt/x",
            "same",
        ] {
            assert_eq!(r.rev_file_entry(rev, miss).unwrap(), all.get(miss).copied(), "{rev}:{miss:?}");
        }
    }
    assert!(r.rev_file_entry("no-such-rev", "a.txt").is_err());
    let (m, _) = r.rev_file_entry("HEAD", "a.txt").unwrap().unwrap();
    assert_eq!(m, EntryMode::File);
}

#[test]
fn rev_file_list_is_the_sorted_full_listing() {
    let (_d, r) = two_commits();
    for rev in ["HEAD", "HEAD~1"] {
        let want: Vec<_> = r.rev_files(rev).unwrap().into_iter().map(|(p, (m, i))| (p, m, i)).collect();
        assert_eq!(r.rev_file_list(rev).unwrap(), want, "{rev}");
    }
    let paths: Vec<String> = r.rev_file_list("HEAD").unwrap().into_iter().map(|t| t.0).collect();
    assert!(paths.windows(2).all(|w| w[0] < w[1]));
    // "a.txt" sorts before "a/x.txt" as a path although the tree lists the directory "a" first
    let pos = |s: &str| paths.iter().position(|p| p == s).unwrap();
    assert!(pos("a.txt") < pos("a/x.txt"));
}

#[test]
fn tracked_files_lists_the_index_in_path_order() {
    let (_d, r) = two_commits();
    let t = r.tracked_files().unwrap();
    let head: Vec<_> = r.rev_files("HEAD").unwrap().into_iter().map(|(p, (m, i))| (p, m, i)).collect();
    assert_eq!(t, head);
}

#[test]
fn index_decode_list_equals_decode() {
    use vcs_repo::index::Index;
    let (_d, r) = two_commits();
    let raw = fs::read(r.worktree().join(".vcs/index")).unwrap();
    let list = Index::decode_list(&raw).unwrap();
    let map = Index::decode(&raw).unwrap();
    assert_eq!(list.len(), map.entries.len());
    for ((p, e), (p2, e2)) in list.iter().zip(map.entries.iter()) {
        assert_eq!((p, e.id, e.mode, e.size), (p2, e2.id, e2.mode, e2.size));
    }
    // damage is refused by both
    let mut bad = raw.clone();
    let n = bad.len();
    bad[n / 2] ^= 0xff;
    assert_eq!(Index::decode(&bad).is_err(), Index::decode_list(&bad).is_err());
}
