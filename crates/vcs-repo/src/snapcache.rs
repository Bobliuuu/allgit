//! Materialized-tree snapshot cache (#490).
//!
//! After a fresh checkout of a big tree into an otherwise empty worktree, the tracked files are
//! cloned copy-on-write (macOS `clonefile(2)`, Linux `FICLONE`) into `<common>/snap/<key>/tree`,
//! with a `manifest` of every file's size, mtime and mode. The next fresh checkout of the same
//! tree (another worktree, a re-clone with `VCS_SNAP_DIR` set) clones that copy into place and
//! restamps the index with a parallel `lstat` (the inode change time is new after a clone; size
//! and mtime are kept) instead of writing every file again.
//!
//! Safety rules:
//! * the snapshot itself is never handed out, only a clone of it;
//! * a snapshot is published by one rename after it was verified against its manifest, so a
//!   crash leaves only a `.tmp-*` directory that the next capture deletes;
//! * a restore verifies every file (exists, size, mtime, mode) and the entry count of every
//!   directory against the manifest; any mismatch removes what was cloned and the snapshot, and
//!   the caller takes the normal path;
//! * no reflink support, another volume, a sparse workspace, a non-empty worktree or an index
//!   that already has entries all mean "no snapshot", never an error.
//!
//! `VCS_SNAPSHOT=0` turns it off. `VCS_SNAPSHOT_MIN_FILES` (default 1000) is the smallest checkout
//! it handles; `VCS_SNAPSHOT_KEEP` (default 3) is how many snapshots are kept (least recently
//! used go first); `VCS_SNAP_DIR` moves the store out of the repository (shared by clones);
//! `VCS_SNAPSHOT_LOG=1` says on stderr why a snapshot was not used.

use crate::index::IndexEntry;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use vcs_objects::names::META_DIR;
use vcs_objects::{EntryMode, ObjectId};

pub(crate) type Want<'a> = Vec<(&'a str, EntryMode, ObjectId)>;

pub(crate) fn enabled() -> bool {
    cfg!(unix) && !matches!(std::env::var("VCS_SNAPSHOT").as_deref(), Ok("0") | Ok("") | Ok("off") | Ok("false"))
}

pub(crate) fn min_files() -> usize {
    std::env::var("VCS_SNAPSHOT_MIN_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000)
}

fn keep() -> usize {
    std::env::var("VCS_SNAPSHOT_KEEP").ok().and_then(|v| v.parse().ok()).unwrap_or(3)
}

fn log(why: &str) {
    if std::env::var_os("VCS_SNAPSHOT_LOG").is_some_and(|v| v != "0") {
        eprintln!("vcs: snapshot cache: {why}");
    }
}

fn store_dir(common: &Path) -> PathBuf {
    match std::env::var_os("VCS_SNAP_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => common.join("snap"),
    }
}

/// Identity of a materialization: every path with its mode and blob id.
pub(crate) fn key(want: &Want) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"vcs-snap-1\0");
    for (p, m, id) in want {
        h.update(p.as_bytes());
        h.update(&[0, *m as u8]);
        h.update(id.as_bytes());
    }
    h.finalize().to_hex().to_string()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct St {
    size: u64,
    mtime_ns: u64,
    ctime_ns: u64,
    mode: u32,
}

#[cfg(unix)]
fn lstat(p: &Path) -> Option<St> {
    use std::os::unix::fs::MetadataExt;
    let m = fs::symlink_metadata(p).ok()?;
    Some(St {
        size: m.len(),
        mtime_ns: crate::repo::mtime_ns(&m),
        ctime_ns: crate::repo::ctime_ns(&m),
        mode: m.mode(),
    })
}
#[cfg(not(unix))]
fn lstat(_: &Path) -> Option<St> {
    None
}

fn same(a: &St, b: &St) -> bool {
    a.size == b.size && a.mtime_ns == b.mtime_ns && a.mode == b.mode
}

fn threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get()).min(8)
}

fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let th = threads();
    if th <= 1 || items.len() < 256 {
        return items.iter().map(f).collect();
    }
    let per = items.len().div_ceil(th);
    std::thread::scope(|sc| {
        let f = &f;
        let hs: Vec<_> = items
            .chunks(per)
            .map(|c| sc.spawn(move || c.iter().map(f).collect::<Vec<_>>()))
            .collect();
        hs.into_iter().flat_map(|h| h.join().expect("snapshot worker panicked")).collect()
    })
}

/// Does the on-disk kind/permission of `mode` agree with the tree's entry mode?
fn mode_fits(em: EntryMode, mode: u32) -> bool {
    let ty = mode & 0o170000;
    match em {
        EntryMode::Symlink => ty == 0o120000,
        EntryMode::File => ty == 0o100000 && mode & 0o111 == 0,
        EntryMode::Executable => ty == 0o100000 && mode & 0o111 != 0,
        EntryMode::Directory => false,
    }
}

struct Manifest {
    paths: Vec<String>,
    st: Vec<St>,
}

impl Manifest {
    fn encode(&self) -> Vec<u8> {
        let mut s = format!("vcs-snap-manifest 1\n{}\n", self.paths.len());
        for (p, t) in self.paths.iter().zip(&self.st) {
            s.push_str(&format!("{:o}\t{}\t{}\t{}\n", t.mode, t.size, t.mtime_ns, p));
        }
        s.push_str("end\n");
        s.into_bytes()
    }
    fn decode(b: &[u8]) -> Option<Manifest> {
        let s = std::str::from_utf8(b).ok()?;
        let mut it = s.lines();
        if it.next()? != "vcs-snap-manifest 1" {
            return None;
        }
        let n: usize = it.next()?.parse().ok()?;
        let (mut paths, mut st) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for _ in 0..n {
            let mut f = it.next()?.splitn(4, '\t');
            let mode = u32::from_str_radix(f.next()?, 8).ok()?;
            let size = f.next()?.parse().ok()?;
            let mtime_ns = f.next()?.parse().ok()?;
            paths.push(f.next()?.to_owned());
            st.push(St {
                size,
                mtime_ns,
                ctime_ns: 0,
                mode,
            });
        }
        (it.next()? == "end" && it.next().is_none()).then_some(Manifest { paths, st })
    }
}

/// Check `root` holds exactly the manifest's files (size, mtime, mode) and nothing else
/// (`root_extra` entries are allowed at the top: the `.vcs` directory of a worktree). Returns the
/// stat of every file in manifest order, ctime included: the restamp.
fn verify(root: &Path, man: &Manifest, root_extra: usize) -> Option<Vec<St>> {
    let actual: Vec<Option<St>> = par_map(&man.paths, |p| lstat(&root.join(p)));
    let mut out = Vec::with_capacity(actual.len());
    for (a, want) in actual.into_iter().zip(&man.st) {
        let a = a?;
        if !same(&a, want) {
            return None;
        }
        out.push(a);
    }
    // Every listed file exists; equal entry counts per directory then rule out extra entries.
    let mut cnt: HashMap<&str, usize> = HashMap::new();
    for p in &man.paths {
        let mut child = p.as_str();
        loop {
            let parent = child.rsplit_once('/').map_or("", |(d, _)| d);
            let new = !cnt.contains_key(parent);
            *cnt.entry(parent).or_insert(0) += 1;
            if !new || parent.is_empty() {
                break;
            }
            child = parent;
        }
    }
    let dirs: Vec<(&str, usize)> = cnt.into_iter().collect();
    let ok = par_map(&dirs, |(d, n)| {
        let dir = if d.is_empty() { root.to_owned() } else { root.join(d) };
        let want = n + if d.is_empty() { root_extra } else { 0 };
        match fs::read_dir(&dir) {
            Ok(rd) => {
                let mut c = 0usize;
                for e in rd {
                    if e.is_err() {
                        return false;
                    }
                    c += 1;
                }
                c == want
            }
            Err(_) => false,
        }
    });
    ok.into_iter().all(|b| b).then_some(out)
}

fn top_names(want: &Want) -> Vec<String> {
    let mut v: Vec<String> = want.iter().map(|(p, _, _)| p.split('/').next().unwrap_or("").to_owned()).collect();
    v.dedup();
    v.sort();
    v.dedup();
    v
}

/// The worktree holds nothing but its metadata directory.
fn only_meta(worktree: &Path) -> bool {
    match fs::read_dir(worktree) {
        Ok(rd) => {
            let mut meta = false;
            for e in rd {
                match e {
                    Ok(e) if e.file_name() == META_DIR => meta = true,
                    _ => return false,
                }
            }
            meta
        }
        Err(_) => false,
    }
}

fn remove_any(p: &Path) {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => {
            let _ = fs::remove_dir_all(p);
        }
        Ok(_) => {
            let _ = fs::remove_file(p);
        }
        Err(_) => {}
    }
}

// ---------------------------------------------------------------- copy-on-write clone

#[cfg(target_os = "macos")]
fn clone_entry(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL));
    let (s, d) = (c(src)?, c(dst)?);
    const CLONE_NOFOLLOW: u32 = 1; // not in the libc crate
                                   // SAFETY: both are NUL-terminated paths. CLONE_NOFOLLOW: a symlink is cloned as a link.
    let rc = unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), CLONE_NOFOLLOW) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn clone_entry(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::os::unix::io::AsRawFd;
    const FICLONE: libc::c_ulong = 0x4004_9409;
    let m = fs::symlink_metadata(src)?;
    if m.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)
    } else if m.is_dir() {
        fs::create_dir(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            clone_entry(&e.path(), &dst.join(e.file_name()))?;
        }
        fs::set_permissions(dst, m.permissions())
    } else {
        let from = fs::File::open(src)?;
        let to = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(m.mode() & 0o7777)
            .open(dst)?;
        // SAFETY: both descriptors are open and owned for the call.
        let rc = unsafe { libc::ioctl(to.as_raw_fd(), FICLONE as _, from.as_raw_fd()) };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            drop(to);
            let _ = fs::remove_file(dst);
            return Err(e);
        }
        to.set_modified(m.modified()?)?;
        fs::set_permissions(dst, m.permissions())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn clone_entry(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no copy-on-write clone on this platform",
    ))
}

/// Clone the named top-level entries of `src` into `dst` (which exists), several at a time: the
/// kernel clones one directory tree on one thread, so independent subtrees run in parallel. With
/// few top-level entries the real directories among them are created here and their children
/// become the units. On error, whatever was made is the caller's to remove.
fn clone_top(src: &Path, dst: &Path, names: &[String]) -> std::io::Result<()> {
    let mut units: Vec<(PathBuf, PathBuf)> = Vec::new();
    for n in names {
        let (s, d) = (src.join(n), dst.join(n));
        let m = fs::symlink_metadata(&s)?;
        if m.is_dir() && names.len() < 16 {
            fs::create_dir(&d)?;
            for e in fs::read_dir(&s)? {
                let e = e?.file_name();
                units.push((s.join(&e), d.join(&e)));
            }
            fs::set_permissions(&d, m.permissions())?;
        } else {
            units.push((s, d));
        }
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let err: Mutex<Option<std::io::Error>> = Mutex::new(None);
    let th = threads().min(units.len()).max(1);
    std::thread::scope(|sc| {
        for _ in 0..th {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= units.len() || err.lock().unwrap().is_some() {
                    return;
                }
                if let Err(e) = clone_entry(&units[i].0, &units[i].1) {
                    err.lock().unwrap().get_or_insert(e);
                    return;
                }
            });
        }
    });
    match err.into_inner().unwrap() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------- restore

/// Fill the empty `worktree` with a clone of the snapshot of `want`, if one exists and checks out.
/// `None`: nothing was changed (or what was cloned has been removed again); take the normal path.
pub(crate) fn try_restore(common: &Path, worktree: &Path, want: &Want) -> Option<Vec<IndexEntry>> {
    if !only_meta(worktree) {
        return None;
    }
    let k = key(want);
    let snap = store_dir(common).join(&k);
    let Ok(raw) = fs::read(snap.join("manifest")) else {
        return None;
    };
    let ok = (|| {
        let man = Manifest::decode(&raw)?;
        let fits = man.paths.len() == want.len()
            && want
                .iter()
                .zip(&man.paths)
                .zip(&man.st)
                .all(|(((p, em, _), mp), st)| *p == mp && mode_fits(*em, st.mode));
        fits.then_some(man)
    })();
    let Some(man) = ok else {
        log("corrupt manifest, snapshot dropped");
        remove_any(&snap);
        return None;
    };
    let tree = snap.join("tree");
    let t0 = std::time::Instant::now();
    let names = top_names(want);
    let undo = || names.iter().for_each(|n| remove_any(&worktree.join(n)));
    if let Err(e) = clone_top(&tree, worktree, &names) {
        log(&format!("clone failed ({e}); normal checkout"));
        undo();
        if e.kind() == std::io::ErrorKind::NotFound {
            remove_any(&snap); // partial snapshot
        }
        return None;
    }
    let t_clone = t0.elapsed();
    let verified = verify(worktree, &man, 1);
    log(&format!(
        "restored {} files: clone {:?}, verify+restamp {:?}",
        want.len(),
        t_clone,
        t0.elapsed() - t_clone
    ));
    let Some(stamps) = verified else {
        log("snapshot does not match its manifest, dropped");
        undo();
        remove_any(&snap);
        return None;
    };
    // LRU: a use refreshes the manifest's mtime.
    if let Ok(f) = fs::OpenOptions::new().write(true).open(snap.join("manifest")) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
    Some(
        want.iter()
            .zip(stamps)
            .map(|((_, em, id), s)| IndexEntry {
                id: *id,
                mode: *em,
                size: s.size,
                mtime_ns: s.mtime_ns,
                ctime_ns: s.ctime_ns,
            })
            .collect(),
    )
}

// ---------------------------------------------------------------- capture

/// Best effort: clone the freshly written tracked files of `worktree` into the store. Never fails
/// the checkout.
pub(crate) fn capture(common: &Path, worktree: &Path, want: &Want) {
    let k = key(want);
    let store = store_dir(common);
    if store.join(&k).join("manifest").exists() {
        return;
    }
    if want.iter().any(|(p, _, _)| p.contains(['\t', '\n'])) {
        return;
    }
    // The worktree must hold the checkout and nothing else (so the clone is exactly the tree).
    let names = top_names(want);
    let mut seen = Vec::new();
    match fs::read_dir(worktree) {
        Ok(rd) => {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n != META_DIR {
                    seen.push(n);
                }
            }
        }
        Err(_) => return,
    }
    seen.sort();
    if seen != names {
        return;
    }
    let paths: Vec<String> = want.iter().map(|(p, _, _)| (*p).to_owned()).collect();
    let live: Vec<Option<St>> = par_map(&paths, |p| lstat(&worktree.join(p)));
    let mut st = Vec::with_capacity(live.len());
    for (l, (_, em, _)) in live.into_iter().zip(want) {
        match l {
            Some(s) if mode_fits(*em, s.mode) => st.push(s),
            _ => return,
        }
    }
    let man = Manifest { paths, st };
    if fs::create_dir_all(&store).is_err() {
        return;
    }
    sweep(&store);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = store.join(format!(".tmp-{}-{}-{nanos}", &k[..8], std::process::id()));
    let built = (|| -> Option<()> {
        fs::create_dir_all(tmp.join("tree")).ok()?;
        if let Err(e) = clone_top(worktree, &tmp.join("tree"), &names) {
            log(&format!("no copy-on-write clone here ({e}); snapshot skipped"));
            return None;
        }
        verify(&tmp.join("tree"), &man, 0)?;
        fs::write(tmp.join("manifest"), man.encode()).ok()?;
        fs::rename(&tmp, store.join(&k)).ok()
    })();
    if built.is_none() {
        remove_any(&tmp);
    } else {
        prune(&store);
    }
}

/// Delete leftovers of interrupted captures.
fn sweep(store: &Path) {
    let Ok(rd) = fs::read_dir(store) else { return };
    for e in rd.flatten() {
        if e.file_name().to_string_lossy().starts_with(".tmp-") {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_none_or(|d| d.as_secs() > 600);
            if old {
                remove_any(&e.path());
            }
        }
    }
}

/// Keep the `VCS_SNAPSHOT_KEEP` most recently used snapshots.
fn prune(store: &Path) {
    let Ok(rd) = fs::read_dir(store) else { return };
    let mut all: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with(".tmp-"))
        .map(|e| {
            let t = fs::metadata(e.path().join("manifest"))
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (t, e.path())
        })
        .collect();
    all.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (_, p) in all.into_iter().skip(keep().max(1)) {
        remove_any(&p);
    }
}
