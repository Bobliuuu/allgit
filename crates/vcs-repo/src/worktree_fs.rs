//! The working-tree side of the repository behind a trait.
//!
//! [`WorktreeFs`] is every operation checkout and status perform on the working tree: list a
//! directory, `lstat`, read, atomically write, remove, rename, create symlinks. [`RealFs`] is the
//! disk implementation (the default, byte-for-byte what the code did before the trait existed);
//! [`MemFs`] is an in-memory fake so the logic can be tested without touching a filesystem, and a
//! future virtual filesystem (docs/research/virtual-fs.md, stage 3) can plug in by implementing
//! the trait. Paths are worktree-relative, `/`-separated, and `""` names the root. Only the
//! working tree goes through the trait; `.vcs/` state is always on disk.

use crate::repo::{ctime_ns, make_symlink, mode_of, mtime_ns, read_link_bytes};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use vcs_objects::EntryMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// What `lstat` says about a path (symlinks are not followed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsStat {
    pub kind: FileKind,
    pub len: u64,
    pub mtime_ns: u64,
    /// 0 = unknown.
    pub ctime_ns: u64,
    /// `Executable` for a file with an execute bit, else `File` (meaningless for non-files).
    pub mode: EntryMode,
    /// Directory identity for the listing cache; 0 = none (the cache is then not used).
    pub ino: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEnt {
    pub name: String,
    pub kind: FileKind,
}

pub trait WorktreeFs: Send + Sync {
    /// Entries of `dir` (no `.` / `..`), in no particular order. Non-UTF-8 names are lossily
    /// converted, as status always did.
    fn read_dir(&self, dir: &str) -> io::Result<Vec<DirEnt>>;
    fn stat(&self, path: &str) -> io::Result<FsStat>;
    fn read(&self, path: &str) -> io::Result<Vec<u8>>;
    /// Raw target bytes of the symlink at `path`.
    fn read_link(&self, path: &str) -> io::Result<Vec<u8>>;
    /// Atomically replace `path` with a regular file (creating parent directories); never writes
    /// through an existing link. Returns the stat of the new file.
    fn write(&self, path: &str, data: &[u8], mode: EntryMode) -> io::Result<FsStat>;
    /// Create a regular file at a path the caller has verified is absent (creating parent
    /// directories). Fails with `AlreadyExists` instead of replacing anything; a real filesystem
    /// writes it in place (no temporary file + rename), so a crash can leave a partial file at
    /// the path. Callers fall back to [`WorktreeFs::write`] on `AlreadyExists`.
    fn create_new(&self, path: &str, data: &[u8], mode: EntryMode) -> io::Result<FsStat> {
        self.write(path, data, mode)
    }
    /// Atomically replace `path` with a symlink to `target`.
    fn symlink(&self, target: &[u8], path: &str) -> io::Result<FsStat>;
    /// Remove a file or symlink.
    fn remove(&self, path: &str) -> io::Result<()>;
    /// Remove an empty directory (never the root; a real filesystem also keeps the process's cwd).
    fn remove_dir(&self, path: &str) -> io::Result<()>;
    fn create_dir_all(&self, path: &str) -> io::Result<()>;
    fn rename(&self, from: &str, to: &str) -> io::Result<()>;
}

// ---------------------------------------------------------------- disk

pub struct RealFs {
    root: PathBuf,
    /// The worktree root, opened once: `stat` resolves paths relative to it (`fstatat`), so the
    /// kernel walks the 2-3 components of a repository path instead of the whole absolute path.
    #[cfg(unix)]
    root_fd: std::sync::OnceLock<Option<fs::File>>,
}

impl RealFs {
    pub fn new(root: PathBuf) -> Self {
        RealFs {
            root,
            #[cfg(unix)]
            root_fd: std::sync::OnceLock::new(),
        }
    }
    fn abs(&self, p: &str) -> PathBuf {
        if p.is_empty() {
            self.root.clone()
        } else {
            self.root.join(p)
        }
    }
}

fn kind_of(ft: fs::FileType) -> FileKind {
    if ft.is_symlink() {
        FileKind::Symlink
    } else if ft.is_dir() {
        FileKind::Dir
    } else if ft.is_file() {
        FileKind::File
    } else {
        FileKind::Other
    }
}

fn stat_of(m: &fs::Metadata) -> FsStat {
    #[cfg(unix)]
    let ino = {
        use std::os::unix::fs::MetadataExt;
        m.ino()
    };
    #[cfg(not(unix))]
    let ino = 0;
    FsStat {
        kind: kind_of(m.file_type()),
        len: m.len(),
        mtime_ns: mtime_ns(m),
        ctime_ns: ctime_ns(m),
        mode: mode_of(m),
        ino,
    }
}

/// `lstat` of `path` relative to the open directory `dir`; `None` if the path cannot be passed to
/// the kernel (embedded NUL), in which case the caller uses the std path.
#[cfg(unix)]
fn stat_at(dir: &fs::File, path: &str) -> Option<io::Result<FsStat>> {
    use std::os::unix::io::AsRawFd;
    let c = std::ffi::CString::new(path).ok()?;
    // SAFETY: `st` is a plain-old-data out parameter, `c` is NUL-terminated, `dir` is a valid fd.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstatat(dir.as_raw_fd(), c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
    if rc != 0 {
        return Some(Err(io::Error::last_os_error()));
    }
    let fmt = st.st_mode & libc::S_IFMT;
    let kind = if fmt == libc::S_IFLNK {
        FileKind::Symlink
    } else if fmt == libc::S_IFDIR {
        FileKind::Dir
    } else if fmt == libc::S_IFREG {
        FileKind::File
    } else {
        FileKind::Other
    };
    let ns = |s: i64, n: i64| (s.max(0) as u64).saturating_mul(1_000_000_000).saturating_add(n.max(0) as u64);
    Some(Ok(FsStat {
        kind,
        len: st.st_size as u64,
        // like `mtime_ns`: a time before the epoch reads as 0
        mtime_ns: if st.st_mtime < 0 {
            0
        } else {
            ns(st.st_mtime as i64, st.st_mtime_nsec as i64)
        },
        ctime_ns: ns(st.st_ctime as i64, st.st_ctime_nsec as i64),
        mode: if st.st_mode & 0o111 != 0 {
            EntryMode::Executable
        } else {
            EntryMode::File
        },
        ino: st.st_ino as u64,
    }))
}

impl WorktreeFs for RealFs {
    fn read_dir(&self, dir: &str) -> io::Result<Vec<DirEnt>> {
        let mut v = Vec::new();
        for e in fs::read_dir(self.abs(dir))? {
            let e = e?;
            v.push(DirEnt {
                name: e.file_name().to_string_lossy().into_owned(),
                kind: kind_of(e.file_type()?),
            });
        }
        Ok(v)
    }
    fn stat(&self, path: &str) -> io::Result<FsStat> {
        #[cfg(unix)]
        if !path.is_empty() {
            if let Some(fd) = self.root_fd.get_or_init(|| fs::File::open(&self.root).ok()) {
                if let Some(r) = stat_at(fd, path) {
                    return r;
                }
            }
        }
        fs::symlink_metadata(self.abs(path)).map(|m| stat_of(&m))
    }
    fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        fs::read(self.abs(path))
    }
    fn read_link(&self, path: &str) -> io::Result<Vec<u8>> {
        read_link_bytes(&self.abs(path))
    }
    fn write(&self, path: &str, data: &[u8], mode: EntryMode) -> io::Result<FsStat> {
        let abs = self.abs(path);
        let parent = abs.parent().expect("worktree file has a parent");
        let tmp = parent.join(tmp_name());
        // The directory almost always exists: create it only when the first attempt says it does not
        // (saves a mkdir + stat per file on a big checkout).
        let mut f = match create_tmp(&tmp, mode) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(parent)?;
                create_tmp(&tmp, mode)?
            }
            r => r?,
        };
        {
            use std::io::Write;
            f.write_all(data)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // The creation mode was filtered by the umask: set the exact bits.
            if umask_filters(mode) {
                f.set_permissions(fs::Permissions::from_mode(want_bits(mode)))?;
            }
        }
        drop(f);
        // rename replaces the path itself (including an existing link); it never writes through it
        fs::rename(&tmp, &abs)?;
        self.stat(path)
    }
    fn create_new(&self, path: &str, data: &[u8], mode: EntryMode) -> io::Result<FsStat> {
        let abs = self.abs(path);
        let parent = abs.parent().expect("worktree file has a parent");
        let mut f = match create_file(&abs, mode) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(parent)?;
                create_file(&abs, mode)?
            }
            r => r?,
        };
        let res = (|| {
            use std::io::Write;
            f.write_all(data)?;
            #[cfg(unix)]
            if umask_filters(mode) {
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(fs::Permissions::from_mode(want_bits(mode)))?;
            }
            // Stat the open file: the result is the final state (nothing changes it after this).
            f.metadata().map(|m| stat_of(&m))
        })();
        if res.is_err() {
            drop(f);
            let _ = fs::remove_file(&abs); // our own half-written file: never leave it behind
        }
        res
    }
    fn symlink(&self, target: &[u8], path: &str) -> io::Result<FsStat> {
        let abs = self.abs(path);
        let parent = abs.parent().expect("worktree file has a parent");
        fs::create_dir_all(parent)?;
        let tmp = parent.join(tmp_name());
        let _ = fs::remove_file(&tmp);
        make_symlink(target, &tmp).map_err(|e| io::Error::other(e.to_string()))?;
        fs::rename(&tmp, &abs)?;
        self.stat(path)
    }
    fn remove(&self, path: &str) -> io::Result<()> {
        fs::remove_file(self.abs(path))
    }
    fn remove_dir(&self, path: &str) -> io::Result<()> {
        let abs = self.abs(path);
        if path.is_empty() {
            return Err(io::Error::other("refusing to remove the worktree root"));
        }
        // Never remove the process's current directory: the caller's next command would fail.
        if let (Ok(cwd), Ok(d)) = (std::env::current_dir().and_then(|c| c.canonicalize()), abs.canonicalize()) {
            if cwd == d {
                return Err(io::Error::other("refusing to remove the current directory"));
            }
        }
        fs::remove_dir(abs)
    }
    fn create_dir_all(&self, path: &str) -> io::Result<()> {
        fs::create_dir_all(self.abs(path))
    }
    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        fs::rename(self.abs(from), self.abs(to))
    }
}

fn want_bits(mode: EntryMode) -> u32 {
    if mode == EntryMode::Executable {
        0o755
    } else {
        0o644
    }
}

/// Whether the process umask would change the creation mode of a file with `mode`, so an explicit
/// chmod is needed. Read once from `/proc/self/status` (Linux); elsewhere, always chmod.
#[cfg(unix)]
fn umask_filters(mode: EntryMode) -> bool {
    static UMASK: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    let m = *UMASK.get_or_init(|| {
        let s = fs::read_to_string("/proc/self/status").ok()?;
        let l = s.lines().find_map(|l| l.strip_prefix("Umask:"))?;
        u32::from_str_radix(l.trim(), 8).ok()
    });
    m.is_none_or(|m| want_bits(mode) & m != 0)
}

fn create_file(path: &Path, mode: EntryMode) -> io::Result<fs::File> {
    create_tmp(path, mode)
}

fn create_tmp(tmp: &Path, mode: EntryMode) -> io::Result<fs::File> {
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(want_bits(mode));
    }
    #[cfg(not(unix))]
    let _ = mode;
    o.open(tmp)
}

fn tmp_name() -> String {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    // `getpid` is a real syscall on current glibc: ask once.
    static PID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    format!(
        ".vcs-tmp-{}-{}",
        PID.get_or_init(std::process::id),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

// ---------------------------------------------------------------- memory

#[derive(Clone, Debug)]
enum Node {
    File {
        data: Vec<u8>,
        mode: EntryMode,
        mtime: u64,
        ctime: u64,
    },
    Link {
        target: Vec<u8>,
        mtime: u64,
    },
}

#[derive(Default)]
struct MemInner {
    nodes: BTreeMap<String, Node>,
    /// Directories that exist even when empty (all others exist implicitly above a node).
    dirs: BTreeSet<String>,
}

/// In-memory working tree. Every mutation advances a logical clock that stamps mtime/ctime, so
/// two writes never share a timestamp (the stat cache can always tell them apart). Directories
/// have no identity, so the listing cache is bypassed.
pub struct MemFs {
    inner: Mutex<MemInner>,
    clock: AtomicU64,
}

impl Default for MemFs {
    fn default() -> Self {
        MemFs {
            inner: Mutex::new(MemInner::default()),
            clock: AtomicU64::new(1_000_000_000),
        }
    }
}

fn not_found() -> io::Error {
    io::Error::from(io::ErrorKind::NotFound)
}
fn parent_of(p: &str) -> &str {
    p.rfind('/').map_or("", |i| &p[..i])
}

impl MemInner {
    fn is_dir(&self, p: &str) -> bool {
        if p.is_empty() || self.dirs.contains(p) {
            return true;
        }
        let lo = format!("{p}/");
        self.nodes.range(lo.clone()..).next().is_some_and(|(k, _)| k.starts_with(&lo))
            || self.dirs.range(lo.clone()..).next().is_some_and(|k| k.starts_with(&lo))
    }
    /// A path component above `p` that is a file/link (ENOTDIR on a real filesystem).
    fn file_ancestor(&self, p: &str) -> bool {
        let mut end = p.len();
        while let Some(i) = p[..end].rfind('/') {
            end = i;
            if self.nodes.contains_key(&p[..end]) {
                return true;
            }
        }
        false
    }
    fn mk_parents(&mut self, p: &str) -> io::Result<()> {
        if self.file_ancestor(p) {
            return Err(io::Error::other("not a directory"));
        }
        let mut d = parent_of(p);
        while !d.is_empty() {
            self.dirs.insert(d.to_owned());
            d = parent_of(d);
        }
        Ok(())
    }
}

impl MemFs {
    pub fn new() -> Self {
        Self::default()
    }
    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed)
    }
    /// Test helper: put a file (parents created), as a user editing the tree would.
    pub fn put(&self, path: &str, data: &[u8]) {
        self.write(path, data, EntryMode::File).expect("MemFs::put");
    }
    pub fn put_exec(&self, path: &str, data: &[u8]) {
        self.write(path, data, EntryMode::Executable).expect("MemFs::put_exec");
    }
    /// Test helper: delete a file or link if present.
    pub fn remove_for_test(&self, path: &str) {
        self.inner.lock().unwrap().nodes.remove(path);
    }
    pub fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.read(path).ok()
    }
    /// All file and link paths, sorted.
    pub fn paths(&self) -> Vec<String> {
        self.inner.lock().unwrap().nodes.keys().cloned().collect()
    }
}

impl WorktreeFs for MemFs {
    fn read_dir(&self, dir: &str) -> io::Result<Vec<DirEnt>> {
        let g = self.inner.lock().unwrap();
        if g.nodes.contains_key(dir) {
            return Err(io::Error::other("not a directory"));
        }
        if !g.is_dir(dir) {
            return Err(not_found());
        }
        let lo = if dir.is_empty() { String::new() } else { format!("{dir}/") };
        let mut out: BTreeMap<String, FileKind> = BTreeMap::new();
        for (k, n) in g.nodes.range(lo.clone()..).take_while(|(k, _)| k.starts_with(&lo)) {
            let rest = &k[lo.len()..];
            match rest.split_once('/') {
                None => {
                    out.insert(
                        rest.to_owned(),
                        if matches!(n, Node::Link { .. }) {
                            FileKind::Symlink
                        } else {
                            FileKind::File
                        },
                    );
                }
                Some((first, _)) => {
                    out.insert(first.to_owned(), FileKind::Dir);
                }
            }
        }
        for k in g.dirs.range(lo.clone()..).take_while(|k| k.starts_with(&lo)) {
            let first = k[lo.len()..].split('/').next().unwrap();
            out.entry(first.to_owned()).or_insert(FileKind::Dir);
        }
        Ok(out.into_iter().map(|(name, kind)| DirEnt { name, kind }).collect())
    }
    fn stat(&self, path: &str) -> io::Result<FsStat> {
        let g = self.inner.lock().unwrap();
        if g.file_ancestor(path) {
            return Err(io::Error::other("not a directory"));
        }
        match g.nodes.get(path) {
            Some(Node::File { data, mode, mtime, ctime }) => Ok(FsStat {
                kind: FileKind::File,
                len: data.len() as u64,
                mtime_ns: *mtime,
                ctime_ns: *ctime,
                mode: *mode,
                ino: 0,
            }),
            Some(Node::Link { target, mtime }) => Ok(FsStat {
                kind: FileKind::Symlink,
                len: target.len() as u64,
                mtime_ns: *mtime,
                ctime_ns: *mtime,
                mode: EntryMode::File,
                ino: 0,
            }),
            None if g.is_dir(path) => Ok(FsStat {
                kind: FileKind::Dir,
                len: 0,
                mtime_ns: 0,
                ctime_ns: 0,
                mode: EntryMode::File,
                ino: 0,
            }),
            None => Err(not_found()),
        }
    }
    fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        match self.inner.lock().unwrap().nodes.get(path) {
            Some(Node::File { data, .. }) => Ok(data.clone()),
            Some(Node::Link { .. }) => Err(io::Error::other("is a symlink")),
            None => Err(not_found()),
        }
    }
    fn read_link(&self, path: &str) -> io::Result<Vec<u8>> {
        match self.inner.lock().unwrap().nodes.get(path) {
            Some(Node::Link { target, .. }) => Ok(target.clone()),
            Some(_) => Err(io::Error::other("not a symlink")),
            None => Err(not_found()),
        }
    }
    fn write(&self, path: &str, data: &[u8], mode: EntryMode) -> io::Result<FsStat> {
        let t = self.tick();
        {
            let mut g = self.inner.lock().unwrap();
            g.mk_parents(path)?;
            if g.is_dir(path) {
                return Err(io::Error::other("is a directory"));
            }
            g.nodes.insert(
                path.to_owned(),
                Node::File {
                    data: data.to_vec(),
                    mode: if mode == EntryMode::Executable { mode } else { EntryMode::File },
                    mtime: t,
                    ctime: t,
                },
            );
        }
        self.stat(path)
    }
    fn symlink(&self, target: &[u8], path: &str) -> io::Result<FsStat> {
        let t = self.tick();
        {
            let mut g = self.inner.lock().unwrap();
            g.mk_parents(path)?;
            if g.is_dir(path) {
                return Err(io::Error::other("is a directory"));
            }
            g.nodes.insert(
                path.to_owned(),
                Node::Link {
                    target: target.to_vec(),
                    mtime: t,
                },
            );
        }
        self.stat(path)
    }
    fn remove(&self, path: &str) -> io::Result<()> {
        self.tick();
        let mut g = self.inner.lock().unwrap();
        if g.file_ancestor(path) {
            return Err(io::Error::other("not a directory"));
        }
        g.nodes.remove(path).map(|_| ()).ok_or_else(|| {
            if g.is_dir(path) {
                io::Error::other("is a directory")
            } else {
                not_found()
            }
        })
    }
    fn remove_dir(&self, path: &str) -> io::Result<()> {
        let mut g = self.inner.lock().unwrap();
        if path.is_empty() {
            return Err(io::Error::other("refusing to remove the worktree root"));
        }
        if g.nodes.contains_key(path) {
            return Err(io::Error::other("not a directory"));
        }
        let lo = format!("{path}/");
        let has_nodes = g.nodes.range(lo.clone()..).next().is_some_and(|(k, _)| k.starts_with(&lo));
        let has_dirs = g.dirs.range(lo.clone()..).next().is_some_and(|k| k.starts_with(&lo));
        if has_nodes || has_dirs {
            return Err(io::Error::other("directory not empty"));
        }
        if g.dirs.remove(path) {
            Ok(())
        } else {
            Err(not_found())
        }
    }
    fn create_dir_all(&self, path: &str) -> io::Result<()> {
        let mut g = self.inner.lock().unwrap();
        if path.is_empty() {
            return Ok(());
        }
        let probe = format!("{path}/_");
        g.mk_parents(&probe)?;
        if g.nodes.contains_key(path) {
            return Err(io::Error::other("file exists"));
        }
        g.dirs.insert(path.to_owned());
        Ok(())
    }
    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        self.tick();
        let mut g = self.inner.lock().unwrap();
        let Some(n) = g.nodes.remove(from) else {
            return Err(not_found());
        };
        if let Err(e) = g.mk_parents(to) {
            g.nodes.insert(from.to_owned(), n);
            return Err(e);
        }
        g.nodes.insert(to.to_owned(), n);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same behavioural contract, run against the disk and the memory implementation.
    fn contract(fs: &dyn WorktreeFs) {
        assert!(fs.read_dir("").unwrap().is_empty());
        assert_eq!(fs.stat("nope").unwrap_err().kind(), io::ErrorKind::NotFound);
        let st = fs.write("a/b/c.txt", b"hello", EntryMode::File).unwrap();
        assert_eq!((st.kind, st.len, st.mode), (FileKind::File, 5, EntryMode::File));
        assert_eq!(fs.read("a/b/c.txt").unwrap(), b"hello");
        assert_eq!(fs.stat("a/b").unwrap().kind, FileKind::Dir);
        let mut root = fs.read_dir("").unwrap();
        root.sort_by(|x, y| x.name.cmp(&y.name));
        assert_eq!(
            root,
            vec![DirEnt {
                name: "a".into(),
                kind: FileKind::Dir
            }]
        );
        // replace keeps one file; exec bit follows the requested mode
        let st = fs.write("a/b/c.txt", b"bye!!", EntryMode::Executable).unwrap();
        #[cfg(unix)]
        assert_eq!(st.mode, EntryMode::Executable);
        let _ = st;
        assert_eq!(fs.read("a/b/c.txt").unwrap(), b"bye!!");
        // a changed file never keeps the old (mtime, size) pair when content changed in place
        let s1 = fs.stat("a/b/c.txt").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        fs.write("a/b/c.txt", b"other", EntryMode::File).unwrap();
        let s2 = fs.stat("a/b/c.txt").unwrap();
        assert!(s2.mtime_ns > s1.mtime_ns || s2.ctime_ns > s1.ctime_ns);
        // symlinks are links, not their target
        #[cfg(unix)]
        {
            let st = fs.symlink(b"c.txt", "a/b/l").unwrap();
            assert_eq!(st.kind, FileKind::Symlink);
            assert_eq!(fs.read_link("a/b/l").unwrap(), b"c.txt");
            assert_eq!(fs.stat("a/b/l").unwrap().kind, FileKind::Symlink);
            // writing over a link replaces the link, never the file it points at
            fs.write("a/b/l", b"plain", EntryMode::File).unwrap();
            assert_eq!(fs.stat("a/b/l").unwrap().kind, FileKind::File);
            assert_eq!(fs.read("a/b/c.txt").unwrap(), b"other");
            fs.remove("a/b/l").unwrap();
        }
        // a file in the way of a directory is an error, not a silent replace
        assert!(fs.write("a/b/c.txt/x", b"", EntryMode::File).is_err());
        assert!(fs.read_dir("a/b/c.txt").is_err());
        // rename, remove, prune
        fs.rename("a/b/c.txt", "a/d.txt").unwrap();
        assert_eq!(fs.stat("a/b/c.txt").unwrap_err().kind(), io::ErrorKind::NotFound);
        fs.create_dir_all("e/f").unwrap();
        assert_eq!(fs.stat("e/f").unwrap().kind, FileKind::Dir);
        assert!(fs.remove_dir("a").is_err(), "non-empty");
        fs.remove_dir("e/f").unwrap();
        fs.remove_dir("e").unwrap();
        fs.remove("a/d.txt").unwrap();
        assert_eq!(fs.remove("a/d.txt").unwrap_err().kind(), io::ErrorKind::NotFound);
        let _ = fs.remove_dir("a/b");
        let _ = fs.remove_dir("a");
        assert!(fs.remove_dir("").is_err(), "never the root");
    }

    /// The `fstatat` fast path must report exactly what `symlink_metadata` does.
    #[cfg(unix)]
    #[test]
    fn real_fs_stat_matches_std() {
        let d = tempfile::tempdir().unwrap();
        let fs_ = RealFs::new(d.path().to_path_buf());
        fs_.write("a/b/file.txt", b"hello", EntryMode::File).unwrap();
        fs_.write("a/run.sh", b"#!/bin/sh\n", EntryMode::Executable).unwrap();
        fs_.symlink(b"b/file.txt", "a/link").unwrap();
        for p in [
            "a",
            "a/b",
            "a/b/file.txt",
            "a/run.sh",
            "a/link",
            "a/missing",
            "a/b/file.txt/under",
            "nope/deeper",
        ] {
            let want = fs::symlink_metadata(d.path().join(p));
            let got = fs_.stat(p);
            match (want, got) {
                (Ok(m), Ok(g)) => assert_eq!(stat_of(&m), g, "{p}"),
                (Err(a), Err(b)) => assert_eq!(a.kind(), b.kind(), "{p}"),
                (a, b) => panic!("{p}: std {a:?} vs fast {b:?}"),
            }
        }
    }

    /// `create_new` makes parents, sets exact bits, reports the final stat, and never replaces.
    #[cfg(unix)]
    #[test]
    fn real_fs_create_new_never_replaces() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let fs_ = RealFs::new(d.path().to_path_buf());
        let st = fs_.create_new("x/y/a.txt", b"one", EntryMode::File).unwrap();
        assert_eq!((st.kind, st.len), (FileKind::File, 3));
        assert_eq!(st, fs_.stat("x/y/a.txt").unwrap(), "returned stat is the final state");
        let st = fs_.create_new("x/run.sh", b"#!/bin/sh\n", EntryMode::Executable).unwrap();
        assert_eq!(st.mode, EntryMode::Executable);
        assert_eq!(fs::metadata(d.path().join("x/run.sh")).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(
            fs::metadata(d.path().join("x/y/a.txt")).unwrap().permissions().mode() & 0o777,
            0o644
        );
        // an existing file and a link are left alone
        let e = fs_.create_new("x/y/a.txt", b"two", EntryMode::File).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(d.path().join("x/y/a.txt")).unwrap(), b"one");
        fs_.symlink(b"y/a.txt", "x/link").unwrap();
        let e = fs_.create_new("x/link", b"three", EntryMode::File).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(d.path().join("x/y/a.txt")).unwrap(), b"one");
    }

    #[test]
    fn real_fs_contract() {
        let d = tempfile::tempdir().unwrap();
        contract(&RealFs::new(d.path().to_path_buf()));
    }

    #[test]
    fn mem_fs_contract() {
        contract(&MemFs::new());
    }

    #[test]
    fn mem_fs_lists_implicit_directories_once() {
        let m = MemFs::new();
        m.put("x/a", b"1");
        m.put("x/b/c", b"2");
        m.put("y", b"3");
        let names: Vec<_> = m.read_dir("x").unwrap().into_iter().map(|e| (e.name, e.kind)).collect();
        assert_eq!(names, vec![("a".into(), FileKind::File), ("b".into(), FileKind::Dir)]);
        assert_eq!(m.paths(), vec!["x/a", "x/b/c", "y"]);
    }
}
