//! Fresh checkout writes new paths in place (`create_new`: O_EXCL + fstat, no temporary file and
//! rename) and keeps the atomic write for overwrites. The index is saved last, so a crash in the
//! middle leaves the previous state: stray files are untracked, nothing names a partial file, and
//! removing the strays lets the same checkout run to completion.
use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use vcs_objects::{EntryMode, Signature};
use vcs_repo::worktree_fs::{DirEnt, FsStat, RealFs, StatCursor, WorktreeFs};
use vcs_repo::Repo;

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}

fn body(i: usize) -> String {
    format!("file {i}\n").repeat(1 + i % 7)
}

fn path_of(i: usize) -> String {
    format!("wide/d{}/s{}/f{i}.txt", i % 13, i % 5)
}

/// `RealFs` that counts the two ways of writing a file and can "crash" on the Nth in-place create:
/// half of the bytes reach the final path, then the process "dies" (a panic that unwinds out of the
/// checkout, so nothing after that point runs).
struct Spy {
    inner: RealFs,
    creates: AtomicUsize,
    atomic_writes: AtomicUsize,
    crash_at: usize, // 0 = never
}

impl WorktreeFs for Spy {
    fn stat_seq(&self, c: &mut StatCursor, p: &str) -> io::Result<FsStat> {
        self.inner.stat_seq(c, p)
    }
    fn read_dir(&self, d: &str) -> io::Result<Vec<DirEnt>> {
        self.inner.read_dir(d)
    }
    fn stat(&self, p: &str) -> io::Result<FsStat> {
        self.inner.stat(p)
    }
    fn read(&self, p: &str) -> io::Result<Vec<u8>> {
        self.inner.read(p)
    }
    fn read_link(&self, p: &str) -> io::Result<Vec<u8>> {
        self.inner.read_link(p)
    }
    fn write(&self, p: &str, d: &[u8], m: EntryMode) -> io::Result<FsStat> {
        self.atomic_writes.fetch_add(1, Ordering::SeqCst);
        self.inner.write(p, d, m)
    }
    fn create_new(&self, p: &str, d: &[u8], m: EntryMode) -> io::Result<FsStat> {
        let n = self.creates.fetch_add(1, Ordering::SeqCst) + 1;
        if self.crash_at != 0 && n == self.crash_at {
            self.inner.create_new(p, &d[..d.len() / 2], m)?;
            panic!("simulated crash while writing {p}");
        }
        self.inner.create_new(p, d, m)
    }
    fn open_read(&self, p: &str) -> io::Result<Option<fs::File>> {
        self.inner.open_read(p)
    }
    fn symlink(&self, t: &[u8], p: &str) -> io::Result<FsStat> {
        self.inner.symlink(t, p)
    }
    fn remove(&self, p: &str) -> io::Result<()> {
        self.inner.remove(p)
    }
    fn remove_dir(&self, p: &str) -> io::Result<()> {
        self.inner.remove_dir(p)
    }
    fn create_dir_all(&self, p: &str) -> io::Result<()> {
        self.inner.create_dir_all(p)
    }
    fn rename(&self, a: &str, b: &str) -> io::Result<()> {
        self.inner.rename(a, b)
    }
}

fn spy(root: &Path, crash_at: usize) -> Arc<Spy> {
    Arc::new(Spy {
        inner: RealFs::new(root.to_path_buf()),
        creates: AtomicUsize::new(0),
        atomic_writes: AtomicUsize::new(0),
        crash_at,
    })
}

/// main: 40 files. wide: those, 10 of them rewritten, plus 600 new files in directories main lacks.
fn setup() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("w")).unwrap();
    let root = r.worktree().to_path_buf();
    for i in 0..40 {
        let p = root.join(format!("base/d{}/b{i}.txt", i % 4));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body(i)).unwrap();
    }
    r.add(&["".into()]).unwrap();
    r.commit("base", sig()).unwrap();
    r.create_branch("root", "wide").unwrap();
    r.checkout("root", "wide").unwrap();
    for i in 0..10 {
        fs::write(root.join(format!("base/d{}/b{i}.txt", i % 4)), format!("rewritten {i}\n")).unwrap();
    }
    for i in 0..600 {
        let p = root.join(path_of(i));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, body(i)).unwrap();
        #[cfg(unix)]
        if i % 50 == 0 {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    r.add(&["".into()]).unwrap();
    r.commit("wide", sig()).unwrap();
    r.checkout("root", "main").unwrap();
    (d, r)
}

fn tmp_leftovers(root: &Path) -> Vec<String> {
    fn go(dir: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let n = e.file_name().to_string_lossy().into_owned();
            if n == ".vcs" {
                continue;
            }
            if n.starts_with(".vcs-tmp") {
                out.push(n);
            }
            if e.file_type().unwrap().is_dir() {
                go(&e.path(), out);
            }
        }
    }
    let mut out = Vec::new();
    go(root, &mut out);
    out
}

fn assert_wide_tree(root: &Path) {
    for i in 0..600 {
        let p = root.join(path_of(i));
        assert_eq!(fs::read_to_string(&p).unwrap(), body(i), "{p:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let want = if i % 50 == 0 { 0o755 } else { 0o644 };
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, want, "{p:?}");
        }
    }
    for i in 0..10 {
        assert_eq!(
            fs::read_to_string(root.join(format!("base/d{}/b{i}.txt", i % 4))).unwrap(),
            format!("rewritten {i}\n")
        );
    }
}

fn assert_clean(r: &Repo) {
    let st = r.status().unwrap();
    assert!(
        st.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()),
        "status not clean"
    );
    assert!(st.untracked.is_empty(), "{:?}", st.untracked);
}

#[test]
fn new_paths_are_created_in_place_and_overwrites_stay_atomic() {
    let (_d, r) = setup();
    let root = r.worktree().to_path_buf();
    let s = spy(&root, 0);
    let r = r.with_worktree_fs(s.clone());
    r.checkout("root", "wide").unwrap();
    assert_eq!(s.creates.load(Ordering::SeqCst), 600, "every new path is created directly");
    assert_eq!(
        s.atomic_writes.load(Ordering::SeqCst),
        10,
        "only the 10 overwrites use tmp + rename"
    );
    assert_wide_tree(&root);
    assert!(tmp_leftovers(&root).is_empty());
    assert_clean(&r);
    assert!(r.fsck().unwrap().problems.is_empty());
}

#[test]
fn crash_mid_checkout_leaves_the_old_state_and_a_retry_completes() {
    let (d, r) = setup();
    let root = r.worktree().to_path_buf();
    let before = fs::read(root.join(".vcs/index")).unwrap();
    let r = r.with_worktree_fs(spy(&root, 250));
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.checkout("root", "wide")));
    assert!(died.is_err(), "the simulated crash must abort the checkout");
    drop(r);

    // The index is written last: after the crash it is byte-for-byte the old one.
    assert_eq!(fs::read(root.join(".vcs/index")).unwrap(), before);
    let r = Repo::discover(&root).unwrap();
    assert!(r.fsck().unwrap().problems.is_empty());
    // Still on main with nothing staged. The only tracked files that may differ are the overwrites
    // (atomic: a whole old or a whole new file, never half), which sort before `wide/`.
    let st = r.status().unwrap();
    assert!(st.zones.iter().all(|z| z.staged.is_empty()), "nothing may be staged");
    for z in &st.zones {
        for c in &z.unstaged {
            let i = (0..10)
                .find(|i| c.path == format!("base/d{}/b{i}.txt", i % 4))
                .expect("only overwritten files differ");
            let got = fs::read_to_string(root.join(&c.path)).unwrap();
            assert!(got == body(i) || got == format!("rewritten {i}\n"), "{}: {got:?}", c.path);
        }
    }
    assert!(!st.untracked.is_empty(), "the files written before the crash show as untracked");
    assert!(st.untracked.iter().all(|u| u.starts_with("wide")), "{:?}", st.untracked);
    assert!(tmp_leftovers(&root).is_empty());
    // A retry refuses to overwrite untracked files (they are not known to be ours) ...
    assert!(r.checkout("root", "wide").is_err());
    assert!(fs::read(root.join(".vcs/index")).unwrap() == before);
    // ... and after the strays are removed (and the overwrites restored) it runs to completion with the exact tree.
    fs::remove_dir_all(root.join("wide")).unwrap();
    r.restore(&["".into()], false).unwrap();
    r.checkout("root", "wide").unwrap();
    assert_wide_tree(&root);
    assert_clean(&r);
    assert!(r.fsck().unwrap().problems.is_empty());
    drop(d);
}
