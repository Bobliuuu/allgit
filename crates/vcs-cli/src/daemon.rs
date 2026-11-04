//! `vcs daemon`: an opt-in resident process per repository that runs small commands without paying
//! process start (dyld, runtime init, clap tree) on every invocation. Design and numbers:
//! docs/daemon.md.
//!
//! The daemon keeps **no semantic state**. For each request it `fork`s a copy of itself and the copy
//! runs exactly the code the plain `vcs` binary runs (`real_main`), with the client's stdin, stdout
//! and stderr passed over the socket (`SCM_RIGHTS`), its working directory, umask, environment and
//! argv. Because it is the same code reading the same files, output is byte-identical to the
//! in-process path, status is as fresh as the in-process status (the fsmonitor journal and the index
//! stat checks are consulted by the command itself), a `kill -9` of the daemon or of a request child
//! leaves exactly what a `kill -9` of a normal `vcs` would leave, and every repo lock (`flock`) is
//! taken and released inside the short-lived child, never by the idle daemon.
//!
//! Client: `try_client`, called first thing in `main`. It only handles an allowlist of argv shapes
//! and any failure before the daemon has acknowledged the request (no socket, stale socket, wrong
//! uid, version mismatch, unsupported command) falls back to the in-process path.
//!
//! Wire protocol (stream socket `.vcs/daemon/sock`, mode 0600 in a 0700 directory):
//! request = one `sendmsg` carrying fds `[0, 1, 2]` plus `u32 len` + body (magic, build id, cwd,
//! umask, argv, env). Reply bytes: `V` (refused, nothing ran), or `S` + `u32 pid` (child started),
//! later `X` + `i32 exit code` (also sent when the child died from a signal, as 128 + signo).
//! EOF after `S` without `X` means the daemon died mid-command: the client reports it and never
//! re-runs a command that may have had effects.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{OsStr, OsString};

/// `VCS_DAEMON=0` disables the client path; `VCS_DAEMON=1` makes a miss start a daemon.
pub const ENV: &str = "VCS_DAEMON";
/// Idle seconds before the daemon exits (default 300).
pub const IDLE_ENV: &str = "VCS_DAEMON_IDLE";

/// Whether `argv` (including argv[0]) is a command shape the daemon may run.
///
/// Excluded on purpose: global options before the command, anything that reads the terminal
/// (`-p`, `-i`, editors), network commands, long-running ones (`watch`, `daemon`, `gc`).
pub fn allowed(argv: &[OsString]) -> bool {
    let strs: Vec<&str> = match argv.iter().skip(1).map(|a| a.to_str()).collect::<Option<_>>() {
        Some(v) => v,
        None => return false,
    };
    let Some((&cmd, rest)) = strs.split_first() else {
        return false;
    };
    if rest
        .iter()
        .any(|a| matches!(*a, "-p" | "--patch" | "-i" | "--interactive" | "-e" | "--edit" | "--stdin" | "-"))
    {
        return false;
    }
    match cmd {
        "status" | "diff" | "log" | "show" | "branch" | "add" | "rm" | "mv" | "restore" | "ls-files" | "cat-file" => true,
        "commit" => rest.iter().any(|a| matches!(*a, "-m" | "--message") || a.starts_with("--message=")),
        "p4" => matches!(rest.first().copied(), Some("opened" | "revert" | "add" | "edit" | "delete")),
        _ => false,
    }
}

#[cfg(not(unix))]
pub fn try_client(_argv: &[OsString]) -> Option<i32> {
    None
}

#[cfg(unix)]
pub use imp::{run_server, try_client};

#[cfg(unix)]
pub use imp::{daemon_status, start, stop};

#[cfg(not(unix))]
mod nonunix {
    use vcs_repo::error::{Error, Result};
    fn no() -> Error {
        Error::Msg("vcs daemon is only available on unix".into())
    }
    pub fn run_server(_: &std::path::Path) -> Result<()> {
        Err(no())
    }
    pub fn start(_: &std::path::Path) -> Result<()> {
        Err(no())
    }
    pub fn stop(_: &std::path::Path) -> Result<()> {
        Err(no())
    }
    pub fn daemon_status(_: &std::path::Path) -> Result<()> {
        Err(no())
    }
}
#[cfg(not(unix))]
pub use nonunix::{daemon_status, run_server, start, stop};

#[cfg(unix)]
mod imp {
    use super::*;
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, RawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use std::time::{Duration, Instant};
    use vcs_repo::error::{Error, Result};

    const MAGIC: &[u8; 8] = b"VCSD0001";
    const MAX_REQ: usize = 8 << 20;
    const SUBDIR: &str = "daemon";
    const SOCK: &str = "sock";

    fn ioerr(path: &Path, source: std::io::Error) -> Error {
        Error::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    /// Identity of this binary: the daemon only serves clients that are the very same executable
    /// (path, size, mtime), so an upgraded `vcs` never talks to a daemon running old code.
    fn build_id() -> Vec<u8> {
        if let Some(v) = std::env::var_os("VCS_DAEMON_TEST_BUILD_ID") {
            return v.into_vec();
        }
        let mut out = env!("CARGO_PKG_VERSION").as_bytes().to_vec();
        if let Ok(exe) = std::env::current_exe() {
            if let Ok(m) = std::fs::metadata(&exe) {
                out.extend_from_slice(format!("|{}|{}|{}", m.len(), m.mtime(), m.mtime_nsec()).as_bytes());
            }
            out.push(b'|');
            out.extend_from_slice(exe.as_os_str().as_bytes());
        }
        out
    }

    // ------------------------------------------------------------------ fd passing

    fn cmsg_space(n: usize) -> usize {
        unsafe { libc::CMSG_SPACE((n * std::mem::size_of::<RawFd>()) as u32) as usize }
    }

    /// One `sendmsg` with `data` and the descriptors in `fds`.
    fn send_with_fds(sock: RawFd, data: &[u8], fds: &[RawFd]) -> std::io::Result<usize> {
        unsafe {
            let mut iov = libc::iovec {
                iov_base: data.as_ptr() as *mut _,
                iov_len: data.len(),
            };
            let mut buf = vec![0u8; cmsg_space(fds.len())];
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = buf.as_mut_ptr() as *mut _;
            msg.msg_controllen = buf.len() as _;
            let c = libc::CMSG_FIRSTHDR(&msg);
            (*c).cmsg_level = libc::SOL_SOCKET;
            (*c).cmsg_type = libc::SCM_RIGHTS;
            (*c).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as _;
            std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(c) as *mut RawFd, fds.len());
            loop {
                let n = libc::sendmsg(sock, &msg, 0);
                if n >= 0 {
                    return Ok(n as usize);
                }
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::Interrupted {
                    return Err(e);
                }
            }
        }
    }

    /// One `recvmsg` into `buf`; returns the byte count and any descriptors received.
    fn recv_with_fds(sock: RawFd, buf: &mut [u8], max_fds: usize) -> std::io::Result<(usize, Vec<RawFd>)> {
        unsafe {
            let mut iov = libc::iovec {
                iov_base: buf.as_mut_ptr() as *mut _,
                iov_len: buf.len(),
            };
            let mut cbuf = vec![0u8; cmsg_space(max_fds)];
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = cbuf.as_mut_ptr() as *mut _;
            msg.msg_controllen = cbuf.len() as _;
            let n = loop {
                let n = libc::recvmsg(sock, &mut msg, 0);
                if n >= 0 {
                    break n as usize;
                }
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::Interrupted {
                    return Err(e);
                }
            };
            let mut fds = Vec::new();
            let mut c = libc::CMSG_FIRSTHDR(&msg);
            while !c.is_null() {
                if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                    let len = (*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                    let p = libc::CMSG_DATA(c) as *const RawFd;
                    for i in 0..len / std::mem::size_of::<RawFd>() {
                        fds.push(std::ptr::read_unaligned(p.add(i)));
                    }
                }
                c = libc::CMSG_NXTHDR(&msg, c);
            }
            Ok((n, fds))
        }
    }

    fn peer_uid_ok(s: &UnixStream) -> bool {
        unsafe {
            #[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
            {
                let (mut uid, mut gid): (libc::uid_t, libc::gid_t) = (0, 0);
                libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) == 0 && uid == libc::geteuid()
            }
            #[cfg(not(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd")))]
            {
                let mut cred: libc::ucred = std::mem::zeroed();
                let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
                libc::getsockopt(
                    s.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    &mut cred as *mut _ as *mut _,
                    &mut len,
                ) == 0
                    && cred.uid == libc::geteuid()
            }
        }
    }

    // ------------------------------------------------------------------ request encoding

    fn put(out: &mut Vec<u8>, b: &[u8]) {
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        out.extend_from_slice(b);
    }

    struct Cur<'a>(&'a [u8]);
    impl<'a> Cur<'a> {
        fn u32(&mut self) -> Option<u32> {
            let (a, b) = self.0.split_at_checked(4)?;
            self.0 = b;
            Some(u32::from_le_bytes(a.try_into().ok()?))
        }
        fn bytes(&mut self) -> Option<&'a [u8]> {
            let n = self.u32()? as usize;
            let (a, b) = self.0.split_at_checked(n)?;
            self.0 = b;
            Some(a)
        }
    }

    struct Request {
        build: Vec<u8>,
        cwd: PathBuf,
        umask: u32,
        argv: Vec<OsString>,
        env: Vec<(OsString, OsString)>,
    }

    fn encode(argv: &[OsString], cwd: &Path) -> Vec<u8> {
        let mut b = Vec::with_capacity(4096);
        b.extend_from_slice(MAGIC);
        put(&mut b, &build_id());
        put(&mut b, cwd.as_os_str().as_bytes());
        let um = unsafe {
            let m = libc::umask(0o022);
            libc::umask(m);
            m as u32
        };
        b.extend_from_slice(&um.to_le_bytes());
        b.extend_from_slice(&(argv.len() as u32).to_le_bytes());
        for a in argv {
            put(&mut b, a.as_bytes());
        }
        let env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        b.extend_from_slice(&(env.len() as u32).to_le_bytes());
        for (k, v) in &env {
            put(&mut b, k.as_bytes());
            put(&mut b, v.as_bytes());
        }
        b
    }

    fn decode(body: &[u8]) -> Option<Request> {
        let rest = body.strip_prefix(MAGIC.as_slice())?;
        let mut c = Cur(rest);
        let build = c.bytes()?.to_vec();
        let cwd = PathBuf::from(OsStr::from_bytes(c.bytes()?));
        let umask = c.u32()?;
        let n = c.u32()?;
        let mut argv = Vec::new();
        for _ in 0..n {
            argv.push(OsString::from_vec(c.bytes()?.to_vec()));
        }
        let n = c.u32()?;
        let mut env = Vec::new();
        for _ in 0..n {
            let k = OsString::from_vec(c.bytes()?.to_vec());
            let v = OsString::from_vec(c.bytes()?.to_vec());
            env.push((k, v));
        }
        Some(Request { build, cwd, umask, argv, env })
    }

    // ------------------------------------------------------------------ client

    static CHILD_PID: AtomicI32 = AtomicI32::new(0);

    extern "C" fn forward_signal(sig: libc::c_int) {
        let pid = CHILD_PID.load(Ordering::Relaxed);
        unsafe {
            if pid > 0 {
                libc::kill(pid, sig);
            }
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }

    /// Connect to the nearest daemon socket above `cwd`.
    fn connect_up(cwd: &Path) -> (Option<UnixStream>, Option<PathBuf>) {
        let mut dir = Some(cwd);
        let mut stale_root = None;
        while let Some(d) = dir {
            let sock = d.join(".vcs").join(SUBDIR).join(SOCK);
            match UnixStream::connect(&sock) {
                Ok(s) => return (Some(s), None),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return (None, None), // refused (stale), path too long, permission: in-process
            }
            if stale_root.is_none() && d.join(".vcs").is_dir() {
                stale_root = Some(d.to_path_buf());
            }
            dir = d.parent();
        }
        (None, stale_root)
    }

    /// Run `argv` through the repository's daemon if one is running; `None` means "not handled,
    /// run in-process" (nothing was executed).
    pub fn try_client(argv: &[OsString]) -> Option<i32> {
        let mode = std::env::var_os(ENV);
        if mode.as_deref() == Some(OsStr::new("0")) || !allowed(argv) {
            return None;
        }
        let cwd = std::env::current_dir().ok()?;
        let (conn, root) = connect_up(&cwd);
        let Some(mut conn) = conn else {
            if mode.as_deref() == Some(OsStr::new("1")) {
                if let Some(root) = root {
                    spawn_detached(&root);
                }
            }
            return None;
        };
        let body = encode(argv, &cwd);
        let mut msg = Vec::with_capacity(body.len() + 4);
        msg.extend_from_slice(&(body.len() as u32).to_le_bytes());
        msg.extend_from_slice(&body);
        let first = send_with_fds(conn.as_raw_fd(), &msg, &[0, 1, 2]).ok()?;
        if first < msg.len() {
            conn.write_all(&msg[first..]).ok()?;
        }
        // From here on the daemon may have started the command: never fall back after 'S'. A
        // generous timeout only guards a wedged daemon that has not accepted the request.
        let _ = conn.set_read_timeout(Some(Duration::from_secs(60)));
        let mut tag = [0u8; 1];
        conn.read_exact(&mut tag).ok()?;
        if tag[0] != b'S' {
            return None;
        }
        let mut p = [0u8; 4];
        if conn.read_exact(&mut p).is_ok() {
            CHILD_PID.store(u32::from_le_bytes(p) as i32, Ordering::Relaxed);
        }
        // Ignore errors: macOS refuses setsockopt (EINVAL) once the peer has closed, which happens
        // when the command finished instantly; returning here would re-run it in-process.
        let _ = conn.set_read_timeout(None);
        unsafe {
            for s in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                libc::signal(s, forward_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
            }
        }
        let mut x = [0u8; 5];
        match conn.read_exact(&mut x) {
            Ok(()) if x[0] == b'X' => Some(i32::from_le_bytes([x[1], x[2], x[3], x[4]])),
            _ => {
                eprintln!("error: vcs daemon ended before the command finished; the command may have partly run");
                Some(1)
            }
        }
    }

    // ------------------------------------------------------------------ server

    fn dir_of(root: &Path) -> PathBuf {
        root.join(".vcs").join(SUBDIR)
    }

    static STOP: AtomicBool = AtomicBool::new(false);
    static PIPE_W: AtomicI32 = AtomicI32::new(-1);

    extern "C" fn wake(sig: libc::c_int) {
        if sig == libc::SIGTERM || sig == libc::SIGINT || sig == libc::SIGHUP {
            STOP.store(true, Ordering::Relaxed);
        }
        let w = PIPE_W.load(Ordering::Relaxed);
        if w >= 0 {
            unsafe {
                let _ = libc::write(w, b"x".as_ptr() as *const _, 1);
            }
        }
    }

    fn live(root: &Path) -> bool {
        UnixStream::connect(dir_of(root).join(SOCK)).is_ok()
    }

    /// Start a detached daemon for the repository at `root` (no wait).
    fn spawn_detached(root: &Path) {
        use std::os::unix::process::CommandExt;
        let Ok(exe) = std::env::current_exe() else { return };
        let mut c = std::process::Command::new(exe);
        c.args(["daemon", "run"])
            .current_dir(root)
            .env(ENV, "0")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let _ = c.spawn();
    }

    pub fn start(root: &Path) -> Result<()> {
        if live(root) {
            println!("vcs daemon already running for {}", root.display());
            return Ok(());
        }
        spawn_detached(root);
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(10) {
            if live(root) {
                println!("vcs daemon started for {}", root.display());
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(Error::Msg("vcs daemon did not start (socket path too long, or the binary changed?)".into()))
    }

    fn read_pid(root: &Path) -> Option<i32> {
        std::fs::read_to_string(dir_of(root).join("pid")).ok()?.trim().parse().ok()
    }

    pub fn stop(root: &Path) -> Result<()> {
        let Some(pid) = read_pid(root) else {
            println!("vcs daemon not running");
            return Ok(());
        };
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(10) && live(root) {
            std::thread::sleep(Duration::from_millis(5));
        }
        println!("vcs daemon stopped");
        Ok(())
    }

    pub fn daemon_status(root: &Path) -> Result<()> {
        if live(root) {
            println!("vcs daemon running (pid {}) for {}", read_pid(root).unwrap_or(0), root.display());
        } else {
            println!("vcs daemon not running for {}", root.display());
        }
        Ok(())
    }

    fn reply(conn: &mut UnixStream, b: &[u8]) {
        let _ = conn.write_all(b);
    }

    /// The daemon main loop for the repository at `root` (`vcs daemon run`, run in that directory).
    pub fn run_server(root: &Path) -> Result<()> {
        let dir = dir_of(root);
        unsafe {
            libc::umask(0o077);
        }
        std::fs::create_dir_all(&dir).map_err(|e| ioerr(&dir, e))?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| ioerr(&dir, e))?;
        if std::fs::metadata(&dir).map_err(|e| ioerr(&dir, e))?.uid() != unsafe { libc::geteuid() } {
            return Err(Error::Msg(format!("{} is not owned by the current user", dir.display())));
        }
        // Singleton: one daemon per repository. This flock guards only the daemon's own identity; it
        // is not a repository lock and nothing else waits on it.
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))
            .map_err(|e| ioerr(&dir, e))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Ok(()); // another daemon owns this repository
        }
        let sock = dir.join(SOCK);
        std::fs::write(dir.join("pid"), std::process::id().to_string()).map_err(|e| ioerr(&dir, e))?;
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).map_err(|e| ioerr(&sock, e))?;
        std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600)).map_err(|e| ioerr(&sock, e))?;
        let sock_ino = std::fs::metadata(&sock).map(|m| m.ino()).unwrap_or(0);
        listener.set_nonblocking(true).map_err(|e| ioerr(&sock, e))?;

        let mut pipe = [0 as RawFd; 2];
        unsafe {
            if libc::pipe(pipe.as_mut_ptr()) != 0 {
                return Err(ioerr(&dir, std::io::Error::last_os_error()));
            }
            for fd in pipe {
                libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            PIPE_W.store(pipe[1], Ordering::Relaxed);
            for s in [libc::SIGCHLD, libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                libc::signal(s, wake as extern "C" fn(libc::c_int) as libc::sighandler_t);
            }
        }
        let idle = std::env::var(IDLE_ENV)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map_or(Duration::from_secs(300), Duration::from_secs);
        let mut last = Instant::now();
        let mut children: HashMap<i32, UnixStream> = HashMap::new();
        let mut quit = false;

        while !STOP.load(Ordering::Relaxed) && !quit {
            let mut fds = [
                libc::pollfd {
                    fd: listener.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: pipe[0],
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            unsafe {
                libc::poll(fds.as_mut_ptr(), 2, 500);
            }
            // Reap finished children and report their status to their clients.
            let mut buf = [0u8; 64];
            while unsafe { libc::read(pipe[0], buf.as_mut_ptr() as *mut _, buf.len()) } > 0 {}
            loop {
                let mut st: libc::c_int = 0;
                let pid = unsafe { libc::waitpid(-1, &mut st, libc::WNOHANG) };
                if pid <= 0 {
                    break;
                }
                let code = if libc::WIFEXITED(st) {
                    libc::WEXITSTATUS(st)
                } else {
                    128 + libc::WTERMSIG(st)
                };
                if let Some(mut c) = children.remove(&pid) {
                    let mut m = [b'X', 0, 0, 0, 0];
                    m[1..].copy_from_slice(&code.to_le_bytes());
                    reply(&mut c, &m);
                }
                last = Instant::now();
            }
            if fds[0].revents & libc::POLLIN != 0 {
                while let Ok((conn, _)) = listener.accept() {
                    last = Instant::now();
                    serve_one(conn, &listener, &pipe, &mut children, &mut quit);
                }
            }
            // Exit when idle, when the socket was replaced or removed, or the repo is gone.
            let gone = std::fs::metadata(&sock).map(|m| m.ino() != sock_ino).unwrap_or(true) || !root.join(".vcs").is_dir();
            if children.is_empty() && (last.elapsed() > idle || gone) {
                break;
            }
        }
        // Only remove our own socket.
        if std::fs::metadata(&sock).map(|m| m.ino() == sock_ino).unwrap_or(false) {
            let _ = std::fs::remove_file(&sock);
            let _ = std::fs::remove_file(dir.join("pid"));
        }
        Ok(())
    }

    /// Handle one connection: validate, fork, and in the child run the command. Returns in the parent.
    fn serve_one(
        mut conn: UnixStream,
        listener: &UnixListener,
        pipe: &[RawFd; 2],
        children: &mut HashMap<i32, UnixStream>,
        quit: &mut bool,
    ) {
        let _ = conn.set_nonblocking(false);
        if !peer_uid_ok(&conn) {
            return;
        }
        let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
        let mut first = vec![0u8; 64 << 10];
        let Ok((n, fds)) = recv_with_fds(conn.as_raw_fd(), &mut first, 3) else {
            return;
        };
        let close_all = |fds: &[RawFd]| {
            for &f in fds {
                unsafe {
                    libc::close(f);
                }
            }
        };
        if n < 4 || fds.len() != 3 {
            close_all(&fds);
            return;
        }
        let len = u32::from_le_bytes([first[0], first[1], first[2], first[3]]) as usize;
        if len > MAX_REQ {
            close_all(&fds);
            return;
        }
        let mut body = first[4..n].to_vec();
        if body.len() < len {
            let mut rest = vec![0u8; len - body.len()];
            if conn.read_exact(&mut rest).is_err() {
                close_all(&fds);
                return;
            }
            body.extend_from_slice(&rest);
        }
        body.truncate(len);
        let req = match decode(&body) {
            Some(r) => r,
            None => {
                close_all(&fds);
                reply(&mut conn, b"V");
                return;
            }
        };
        if req.build != build_id() {
            // The client is a different build than this daemon: refuse, and retire so the next
            // start runs the new code.
            close_all(&fds);
            reply(&mut conn, b"V");
            *quit = true;
            return;
        }
        if !allowed(&req.argv) {
            close_all(&fds);
            reply(&mut conn, b"V");
            return;
        }
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            close_all(&fds);
            reply(&mut conn, b"V");
            return;
        }
        if pid == 0 {
            // Child: become the client's command. Drop every other client's socket so their EOF
            // detection is not delayed by this process.
            children.clear();
            drop(conn);
            unsafe {
                libc::close(listener.as_raw_fd());
                libc::close(pipe[0]);
                libc::close(pipe[1]);
                for s in [libc::SIGCHLD, libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                    libc::signal(s, libc::SIG_DFL);
                }
                for (i, &f) in fds.iter().enumerate() {
                    libc::dup2(f, i as RawFd);
                    if f > 2 {
                        libc::close(f);
                    }
                }
                libc::umask(req.umask as libc::mode_t);
            }
            for (k, _) in std::env::vars_os().collect::<Vec<_>>() {
                std::env::remove_var(k);
            }
            for (k, v) in &req.env {
                std::env::set_var(k, v);
            }
            std::env::set_var(ENV, "0");
            let code = if let Err(e) = std::env::set_current_dir(&req.cwd) {
                eprintln!("error: cannot change to directory {}: {e}", req.cwd.display());
                1
            } else {
                crate::real_main(req.argv)
            };
            std::process::exit(code);
        }
        close_all(&fds);
        let mut m = [b'S', 0, 0, 0, 0];
        m[1..].copy_from_slice(&(pid as u32).to_le_bytes());
        reply(&mut conn, &m);
        let _ = conn.set_read_timeout(None);
        children.insert(pid, conn);
    }
}
