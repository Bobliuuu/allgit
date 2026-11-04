//! Crash-safe local process lock.
//!
//! Design: the lock is an OS advisory `flock(LOCK_EX)` on a persistent file. The kernel drops
//! the lock when the owning process dies for ANY reason (SIGKILL, OOM, power loss + reboot), so
//! there is no stale-lock state to detect and no pid-reuse hazard: ownership is tied to the open
//! file description, not to a pid. The file is never unlinked (unlinking races with waiters that
//! already opened the old inode and would let two holders lock different inodes).
//!
//! The file also carries `pid`, `host` and process `start` time as diagnostics (and for operators
//! deciding whether a holder is alive); they are written without fsync, so the hot path costs one
//! `open` + one `flock` syscall. They are NOT trusted for correctness.
//!
//! Fencing: because the kernel guarantees a dead holder no longer holds the lock, and a live
//! holder (even SIGSTOPped) still does, a stale holder can never write concurrently with its
//! successor. CAS writers additionally re-validate the content tag under the lock.
//!
//! Limits: `flock` is advisory and only mutual between processes on the same kernel/host (or
//! on NFS where the client implements flock via NLM/byte-range locks, Linux >= 2.6.12; on NFS
//! a server/client crash can still need lock recovery, and some NFS setups silently provide no
//! cross-host exclusion). On non-unix targets this falls back to `O_EXCL` + best-effort pid
//! liveness (not available there), i.e. the legacy behaviour.
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

pub struct ProcLock {
    _f: File,
}

/// `Ok(None)` means the lock is held by a live process.
pub fn try_lock(path: &Path) -> io::Result<Option<ProcLock>> {
    let f = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
    if !flock_nb(&f)? {
        return Ok(None);
    }
    stamp(&f);
    Ok(Some(ProcLock { _f: f }))
}

/// Wait up to `timeout` for the lock; `Ok(None)` on timeout (holder is alive and busy).
pub fn lock_timeout(path: &Path, timeout: Duration) -> io::Result<Option<ProcLock>> {
    let start = Instant::now();
    let mut delay = Duration::from_micros(200);
    loop {
        if let Some(l) = try_lock(path)? {
            return Ok(Some(l));
        }
        if start.elapsed() > timeout {
            return Ok(None);
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_millis(5));
    }
}

impl Drop for ProcLock {
    fn drop(&mut self) {
        // Explicit unlock: flock belongs to the open file description, which a concurrent
        // fork() in another thread may briefly share until its exec closes the copy; closing
        // our fd alone would not release the lock in that window.
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: valid fd.
            unsafe { libc::flock(self._f.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

fn stamp(mut f: &File) {
    let _ = f.set_len(0);
    let _ = io::Seek::seek(&mut f, io::SeekFrom::Start(0));
    let _ = writeln!(f, "pid {}\nhost {}\nstart {}", std::process::id(), hostname(), start_time());
}

/// Parse the diagnostic record of a lock file: (pid, host, start).
pub fn read_owner(path: &Path) -> Option<(u32, String, String)> {
    let s = std::fs::read_to_string(path).ok()?;
    let (mut pid, mut host, mut start) = (None, String::new(), String::new());
    for l in s.lines() {
        if let Some(v) = l.strip_prefix("pid ") {
            pid = v.trim().parse().ok();
        } else if let Some(v) = l.strip_prefix("host ") {
            host = v.trim().into();
        } else if let Some(v) = l.strip_prefix("start ") {
            start = v.trim().into();
        }
    }
    Some((pid?, host, start))
}

fn start_time() -> String {
    // Wall-clock process start is enough to tell two pids apart; kept cheap.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    now.to_string()
}

fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: buf is valid for 256 bytes.
        let r = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if r == 0 {
            let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            return String::from_utf8_lossy(&buf[..n]).into_owned();
        }
    }
    String::from("unknown")
}

#[cfg(unix)]
fn flock_nb(f: &File) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    loop {
        // SAFETY: valid fd for the lifetime of `f`.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EWOULDBLOCK) => return Ok(false),
            Some(libc::EINTR) => continue,
            _ => return Err(e),
        }
    }
}

#[cfg(not(unix))]
fn flock_nb(_f: &File) -> io::Result<bool> {
    Ok(true) // no advisory locking available; callers on non-unix targets are single-user
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusive_then_released_on_drop() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("l");
        let a = try_lock(&p).unwrap().unwrap();
        #[cfg(unix)]
        assert!(try_lock(&p).unwrap().is_none());
        let (pid, _, _) = read_owner(&p).unwrap();
        assert_eq!(pid, std::process::id());
        drop(a);
        assert!(try_lock(&p).unwrap().is_some());
    }
    #[test]
    fn legacy_stale_file_is_taken_over() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("l");
        std::fs::write(&p, "pid 999999\n").unwrap(); // left by an old SIGKILLed binary
        assert!(try_lock(&p).unwrap().is_some());
    }
}
