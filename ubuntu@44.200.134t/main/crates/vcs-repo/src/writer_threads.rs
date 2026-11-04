//! How many threads write the files of a big checkout.
//!
//! On ext4, a tree with very many small directories is written faster by ONE thread than by a pool: each
//! directory costs an inode and a 4 KiB block, the per-CPU allocation groups spread the blocks of what the
//! writers create over one region per writer, and on an IOPS-capped cloud volume the 64 MiB journal fills and
//! every writer blocks in `jbd2` (their CPU stays at a few percent). Measured on a c6i.4xlarge (16 vCPU), ext4 on
//! an EBS gp3 volume, page cache and journal quiet before each run, clone of a 100k-file tree with 87k
//! directories (1.5 files per leaf directory) from a local server: 6.4 s with one writer, 8.6 s with two, 15 s
//! with four, 19 s with eight (the old fixed pool). The same box with 100k files in 1000 directories (100 per
//! directory) is the opposite: 3.7 s with the pool, 7.4 s with one writer. So only a directory-heavy batch on
//! ext4 gets one writer. Other filesystems (tmpfs, and APFS/xfs/btrfs, which are unmeasured) keep the pool of up
//! to 8, and so does a batch with at least [`FILES_PER_DIR`] files per directory.

use std::path::Path;

/// Below this many files per (leaf) directory a batch counts as directory-heavy.
const FILES_PER_DIR: usize = 8;

/// Upper bound on writer threads for a checkout of `paths` (sorted) into `dir`. `VCS_CHECKOUT_THREADS=N` overrides it.
pub(crate) fn for_dir<'a>(dir: &Path, paths: impl Iterator<Item = &'a str>) -> usize {
    if let Some(n) = std::env::var("VCS_CHECKOUT_THREADS").ok().and_then(|v| v.parse::<usize>().ok()) {
        return n.max(1);
    }
    let pool = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
    if pool > 1 && is_ext4(dir) && dir_heavy(paths) {
        1
    } else {
        pool
    }
}

/// True when `VCS_CHECKOUT_ATOMIC=1` asks checkout to write every file through a temporary file and a
/// rename, even a path known to be absent (the default creates those in place: no rename, one `fstat`).
/// An escape hatch and the "before" arm of measurements. Read once.
pub(crate) fn atomic_only() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VCS_CHECKOUT_ATOMIC").is_ok_and(|v| v == "1"))
}

/// Threads for the first-scan `lstat` / content check of tracked files (at most 16).
/// `VCS_STATUS_THREADS=N` overrides it (1 = the old single-threaded scan).
pub(crate) fn status_threads() -> usize {
    if let Some(n) = std::env::var("VCS_STATUS_THREADS").ok().and_then(|v| v.parse::<usize>().ok()) {
        return n.max(1);
    }
    std::thread::available_parallelism().map_or(1, |n| n.get()).min(16)
}

/// True when `paths` (sorted) spread over more than one directory per `FILES_PER_DIR` files.
fn dir_heavy<'a>(paths: impl Iterator<Item = &'a str>) -> bool {
    let (mut files, mut dirs, mut last) = (0usize, 0usize, None::<&str>);
    for p in paths {
        let d = p.rsplit_once('/').map_or("", |(d, _)| d);
        if last != Some(d) {
            dirs += 1;
            last = Some(d);
        }
        files += 1;
    }
    files > 0 && files < FILES_PER_DIR * dirs
}

#[cfg(target_os = "linux")]
fn is_ext4(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    const EXT4_SUPER_MAGIC: i64 = 0xEF53; // also ext2 and ext3
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: statfs fills the struct we pass for a valid NUL-terminated path.
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    unsafe { libc::statfs(c.as_ptr(), &mut s) == 0 && s.f_type as i64 == EXT4_SUPER_MAGIC }
}

#[cfg(not(target_os = "linux"))]
fn is_ext4(_dir: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_heavy_means_few_files_per_directory() {
        let deep: Vec<String> = (0..100).map(|i| format!("a/d{i}/f")).collect();
        assert!(dir_heavy(deep.iter().map(String::as_str)));
        let flat: Vec<String> = (0..100).map(|i| format!("a/f{i}")).collect();
        assert!(!dir_heavy(flat.iter().map(String::as_str)));
        assert!(!dir_heavy(std::iter::empty()));
    }

    #[test]
    fn default_is_between_one_and_eight() {
        let n = for_dir(&std::env::temp_dir(), ["a/b"].into_iter());
        assert!((1..=8).contains(&n));
    }
}
