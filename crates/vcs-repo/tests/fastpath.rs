//! Correctness of the local fast paths (stat-cache `add`, cache-tree `commit`/`status`, racy
//! refresh): after every operation the result must equal an independent model of what a
//! from-scratch implementation would say, under random edits, renames and timestamp tricks.
//!
//! The index "racy" window is shortened to 40 ms so that both sides of it are exercised.

use proptest::prelude::*;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};
use vcs_objects::Signature;
use vcs_repo::Repo;

const RACY_NS: u64 = 40_000_000;
/// 30 tracked-path candidates; `a` and `d/e` hold at least 8 files so cached subtree ids matter.
fn files() -> &'static [String] {
    static F: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    F.get_or_init(|| {
        let mut v = vec!["top".to_owned(), "n1".to_owned()];
        for d in ["a", "a/b", "a/b/c", "d", "d/e"] {
            for n in 0..5 {
                v.push(format!("{d}/f{n}"));
            }
        }
        v.push("a0".into()); // sorts between "a" and "a/..." neighbours
        v.push("a-x".into());
        v
    })
}

fn sig(t: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: t,
    }
}

fn settle() {
    std::thread::sleep(Duration::from_millis(60));
}

fn new_repo(base: &Path) -> Repo {
    let mut r = Repo::init(base).unwrap();
    r.set_index_racy_ns(RACY_NS);
    r
}

type Model = BTreeMap<String, Vec<u8>>;

fn fs_state(r: &Repo) -> Model {
    files()
        .iter()
        .filter_map(|p| fs::read(r.worktree().join(p)).ok().map(|c| (p.to_string(), c)))
        .collect()
}

#[derive(Clone, Debug)]
enum Op {
    /// New file or same-size / different-size rewrite.
    Write(usize, u8, bool),
    Remove(usize),
    Rename(usize, usize),
    /// Set the mtime to a value unique to this step (content unchanged).
    Touch(usize),
    /// Rewrite with the same size and push the mtime far into the past (unique per step).
    OldMtime(usize, u8),
    AddAll,
    AddOne(usize),
    Commit,
    Settle,
}

fn op() -> impl Strategy<Value = Op> {
    let f = 0..files().len();
    prop_oneof![
        4 => (f.clone(), 0u8..3, any::<bool>()).prop_map(|(a, b, c)| Op::Write(a, b, c)),
        1 => f.clone().prop_map(Op::Remove),
        1 => (f.clone(), f.clone()).prop_map(|(a, b)| Op::Rename(a, b)),
        1 => f.clone().prop_map(Op::Touch),
        1 => (f.clone(), 0u8..3).prop_map(|(a, b)| Op::OldMtime(a, b)),
        3 => Just(Op::AddAll),
        2 => f.prop_map(Op::AddOne),
        2 => Just(Op::Commit),
        2 => Just(Op::Settle),
    ]
}

fn put(r: &Repo, p: &str, c: &[u8]) {
    let a = r.worktree().join(p);
    fs::create_dir_all(a.parent().unwrap()).unwrap();
    fs::write(a, c).unwrap();
}

fn set_mtime(p: &Path, t: SystemTime) {
    fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
}

struct Sim {
    r: Repo,
    _d: tempfile::TempDir,
    staged: Model,
    head: Model,
    head_count: u64,
}

impl Sim {
    fn new() -> Sim {
        let d = tempfile::tempdir().unwrap();
        let r = new_repo(&d.path().join("r"));
        Sim {
            r,
            _d: d,
            staged: Model::new(),
            head: Model::new(),
            head_count: 0,
        }
    }

    fn apply(&mut self, o: &Op, step: u64) {
        let root = self.r.worktree().to_owned();
        match o {
            Op::Write(f, c, same_size) => {
                let body: Vec<u8> = if *same_size {
                    vec![b'a' + c]
                } else {
                    vec![b'a' + c; 1 + step as usize % 3]
                };
                put(&self.r, files()[*f].as_str(), &body);
            }
            Op::Remove(f) => {
                let _ = fs::remove_file(root.join(files()[*f].as_str()));
            }
            Op::Rename(a, b) => {
                let (pa, pb) = (root.join(&files()[*a]), root.join(&files()[*b]));
                if pa.is_file() && !pb.exists() {
                    fs::create_dir_all(pb.parent().unwrap()).unwrap();
                    fs::rename(pa, pb).unwrap();
                }
            }
            Op::Touch(f) => {
                let p = root.join(files()[*f].as_str());
                if p.is_file() {
                    set_mtime(&p, SystemTime::now() - Duration::from_millis(step));
                }
            }
            Op::OldMtime(f, c) => {
                let p = root.join(files()[*f].as_str());
                if p.is_file() {
                    let len = fs::metadata(&p).unwrap().len() as usize;
                    fs::write(&p, vec![b'a' + c; len.max(1)]).unwrap();
                    set_mtime(&p, SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + step));
                }
            }
            Op::AddAll => {
                self.r.add(&[String::new()]).unwrap();
                self.staged = fs_state(&self.r);
            }
            Op::AddOne(f) => {
                let p = files()[*f].as_str();
                let exists = root.join(p).is_file();
                if exists || self.staged.contains_key(p) {
                    self.r.add(&[p.to_owned()]).unwrap();
                    match fs::read(root.join(p)) {
                        Ok(c) => self.staged.insert(p.to_owned(), c),
                        Err(_) => self.staged.remove(p),
                    };
                }
            }
            Op::Commit => {
                self.head_count += 1;
                let res = self.r.commit("m", sig(100 + self.head_count));
                if self.staged == self.head {
                    assert!(res.is_err(), "commit of nothing must fail");
                } else {
                    res.unwrap();
                    self.head = self.staged.clone();
                    self.check_head_tree();
                }
            }
            Op::Settle => settle(),
        }
    }

    /// The committed root tree must equal the one an independent fresh repo builds.
    fn check_head_tree(&self) {
        let d = tempfile::tempdir().unwrap();
        let o = new_repo(&d.path().join("o"));
        for (p, c) in &self.head {
            put(&o, p, c);
        }
        o.add(&[String::new()]).unwrap();
        o.commit("m", sig(1)).unwrap();
        let mine = self.r.log("root", None, 1).unwrap()[0].1.root;
        let theirs = o.log("root", None, 1).unwrap()[0].1.root;
        assert_eq!(mine, theirs, "root tree differs from a from-scratch build");
    }

    fn check_status(&self, ctx: &str) {
        let st = self.r.status().unwrap();
        let z = &st.zones[0];
        let got_staged: Vec<String> = z.staged.iter().map(|c| format!("{} {:?}", c.path, c.kind)).collect();
        let got_unstaged: Vec<String> = z.unstaged.iter().map(|c| format!("{} {:?}", c.path, c.kind)).collect();
        let mut want_staged = Vec::new();
        for p in self
            .staged
            .keys()
            .chain(self.head.keys())
            .collect::<std::collections::BTreeSet<_>>()
        {
            match (self.head.get(p), self.staged.get(p)) {
                (None, Some(_)) => want_staged.push(format!("{p} Added")),
                (Some(_), None) => want_staged.push(format!("{p} Deleted")),
                (Some(a), Some(b)) if a != b => want_staged.push(format!("{p} Modified")),
                _ => {}
            }
        }
        let mut want_unstaged = Vec::new();
        for (p, c) in &self.staged {
            match fs::read(self.r.worktree().join(p)) {
                Err(_) => want_unstaged.push(format!("{p} Deleted")),
                Ok(now) if &now != c => want_unstaged.push(format!("{p} Modified")),
                _ => {}
            }
        }
        assert_eq!(got_staged, want_staged, "staged after {ctx}");
        assert_eq!(got_unstaged, want_unstaged, "unstaged after {ctx}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 40, .. ProptestConfig::default() })]
    #[test]
    fn fast_paths_match_from_scratch_model(ops in proptest::collection::vec(op(), 1..40)) {
        let mut s = Sim::new();
        // start from a committed full tree so cached subtree ids exist from the first op
        for (i, f) in files().iter().enumerate() {
            put(&s.r, f, format!("v{}", i % 3).as_bytes());
        }
        s.apply(&Op::AddAll, 0);
        s.apply(&Op::Commit, 0);
        settle();
        for (i, o) in ops.iter().enumerate() {
            s.apply(o, i as u64);
            // twice: the first run may refresh racy entries, the second must still agree
            s.check_status(&format!("{o:?} (1)"));
            s.check_status(&format!("{o:?} (2)"));
        }
        let rep = s.r.fsck().unwrap();
        prop_assert!(rep.problems.is_empty(), "{:?}", rep.problems);
    }
}

fn write_many(r: &Repo, n: usize) {
    for i in 0..n {
        put(r, &format!("d{}/s{}/f{i}.txt", i % 4, i % 3), format!("content {i}").as_bytes());
    }
}

#[test]
fn add_of_unchanged_files_reads_nothing() {
    let d = tempfile::tempdir().unwrap();
    let r = new_repo(&d.path().join("r"));
    write_many(&r, 40);
    r.add(&[String::new()]).unwrap();
    let c0 = r.counters().content_reads;
    assert_eq!(c0, 40);
    settle(); // let the racy window close
    r.add(&[String::new()]).unwrap(); // re-verifies the (racy) entries once
    r.add(&[String::new()]).unwrap();
    let before = r.counters().content_reads;
    r.add(&[String::new()]).unwrap();
    assert_eq!(r.counters().content_reads, before, "settled unchanged files must not be read");
}

#[test]
fn unchanged_add_does_not_rewrite_the_index() {
    let d = tempfile::tempdir().unwrap();
    let r = new_repo(&d.path().join("r"));
    write_many(&r, 10);
    r.add(&[String::new()]).unwrap();
    settle();
    r.add(&[String::new()]).unwrap(); // settles racy entries (may rewrite)
    let ix = r.worktree().join(".vcs/index");
    let m0 = fs::metadata(&ix).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    r.add(&[String::new()]).unwrap();
    assert_eq!(fs::metadata(&ix).unwrap().modified().unwrap(), m0);
}

#[test]
fn edit_inside_racy_window_is_still_seen_by_add_and_status() {
    let d = tempfile::tempdir().unwrap();
    let r = new_repo(&d.path().join("r"));
    put(&r, "f", b"a");
    r.add(&[String::new()]).unwrap();
    put(&r, "f", b"b"); // same size, probably the same mtime tick
    let st = r.status().unwrap();
    assert_eq!(st.zones[0].unstaged.len(), 1);
    r.add(&[String::new()]).unwrap();
    r.commit("m", sig(1)).unwrap();
    let st = r.status().unwrap();
    assert!(st.zones[0].unstaged.is_empty() && st.zones[0].staged.is_empty());
}

#[test]
fn racy_entries_are_refreshed_by_status() {
    let d = tempfile::tempdir().unwrap();
    let r = new_repo(&d.path().join("r"));
    write_many(&r, 30);
    r.add(&[String::new()]).unwrap();
    r.commit("m", sig(1)).unwrap();
    settle(); // window over, but the index still says racy
    let c0 = r.counters().content_reads;
    r.status().unwrap();
    let c1 = r.counters().content_reads;
    assert!(c1 > c0, "first status reads the racy entries");
    assert!(c1 - c0 <= 30);
    r.status().unwrap();
    assert_eq!(r.counters().content_reads, c1, "second status reads nothing");
}

#[test]
fn refresh_never_marks_a_modified_file_clean() {
    let d = tempfile::tempdir().unwrap();
    let r = new_repo(&d.path().join("r"));
    put(&r, "keep", b"k");
    put(&r, "f", b"a");
    r.add(&[String::new()]).unwrap();
    put(&r, "f", b"b"); // same size inside the window
    settle();
    for _ in 0..3 {
        let st = r.status().unwrap();
        let un: Vec<_> = st.zones[0].unstaged.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(un, ["f"], "modified file must stay modified");
    }
}

#[test]
fn commit_of_one_edit_writes_only_the_changed_path() {
    let d = tempfile::tempdir().unwrap();
    let r = new_repo(&d.path().join("r"));
    for i in 0..60 {
        put(&r, &format!("a{}/b{}/c{}/f{i}", i % 3, i % 4, i % 5), format!("v{i}").as_bytes());
    }
    r.add(&[String::new()]).unwrap();
    r.commit("one", sig(1)).unwrap();
    let t0 = r.counters().tree_puts;
    assert!(t0 > 10);
    // unchanged index: nothing to commit, and no tree is written
    settle();
    put(&r, "a0/b0/c0/f0", b"changed!");
    r.add(&["a0/b0/c0/f0".into()]).unwrap();
    r.commit("two", sig(2)).unwrap();
    assert_eq!(r.counters().tree_puts - t0, 4, "root, a0, a0/b0, a0/b0/c0");
    // a second one-file commit now also uses the cache saved by the first
    put(&r, "a1/b1/c1/f1", b"again!");
    r.add(&["a1/b1/c1/f1".into()]).unwrap();
    let t1 = r.counters().tree_puts;
    r.commit("three", sig(3)).unwrap();
    assert_eq!(r.counters().tree_puts - t1, 4);
    assert!(r.fsck().unwrap().problems.is_empty());
}

#[test]
fn zone_layout_change_invalidates_cached_trees() {
    let d = tempfile::tempdir().unwrap();
    let r = new_repo(&d.path().join("r"));
    put(&r, "src/a", b"1");
    put(&r, "top", b"3");
    r.add(&[String::new()]).unwrap();
    r.commit("one", sig(1)).unwrap(); // caches the root and src trees under the old layout
    r.create_zone("libs", "libs").unwrap();
    put(&r, "libs/x", b"2");
    put(&r, "libs/sub/y", b"4");
    r.add(&[String::new()]).unwrap();
    let st = r.status().unwrap();
    for z in &st.zones {
        assert!(z.unstaged.is_empty(), "{z:?}");
    }
    r.commit("two", sig(2)).unwrap();
    let st = r.status().unwrap();
    assert!(st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()), "{st:?}");
    put(&r, "libs/x", b"changed");
    r.add(&["libs/x".into()]).unwrap();
    r.commit("three", sig(3)).unwrap();
    let st = r.status().unwrap();
    assert!(st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()), "{st:?}");
    assert!(r.fsck().unwrap().problems.is_empty());
}

/// A later partial `add` must not push the racy boundary past an entry it did not re-read: the
/// edit below has the same size and (forced) the same mtime as the recorded entry.
#[test]
fn partial_add_does_not_hide_a_same_tick_edit() {
    let d = tempfile::tempdir().unwrap();
    let r = new_repo(&d.path().join("r"));
    put(&r, "f", b"a");
    put(&r, "g", b"1");
    r.add(&["f".into()]).unwrap();
    let f = r.worktree().join("f");
    let m = fs::metadata(&f).unwrap().modified().unwrap();
    fs::write(&f, b"b").unwrap();
    set_mtime(&f, m); // as if the edit landed in the same timestamp tick
    settle();
    r.add(&["g".into()]).unwrap(); // would move the boundary past f's window if it were unsound
    settle();
    for _ in 0..2 {
        let st = r.status().unwrap();
        let un: Vec<_> = st.zones[0].unstaged.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(un, ["f"], "same-tick edit hidden");
    }
    r.add(&[String::new()]).unwrap();
    let st = r.status().unwrap();
    assert!(st.zones[0].unstaged.is_empty());
    assert_eq!(st.zones[0].staged.len(), 2);
}
