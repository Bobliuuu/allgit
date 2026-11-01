//! Client-side hooks: executables in `<common .vcs>/hooks/`. Never cloned, fetched or imported;
//! a repository's content cannot reach this directory (`.vcs` is a reserved name everywhere).
//! See docs/hooks.md.

use clap::Subcommand;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use vcs_repo::{Error, Repo};

/// Every hook name, in the order `hook list` shows them.
pub const HOOKS: &[&str] = &[
    "pre-commit",
    "commit-msg",
    "post-commit",
    "pre-publish",
    "post-publish",
    "pre-merge",
    "post-merge",
    "post-checkout",
    "pre-rebase",
];

pub const DEFAULT_TIMEOUT_SECS: u64 = 60;

static DISABLED: AtomicBool = AtomicBool::new(false);

/// `--no-hooks` on the command line.
pub fn set_disabled(v: bool) {
    DISABLED.store(v, Ordering::Relaxed);
}

fn skipped() -> bool {
    DISABLED.load(Ordering::Relaxed) || std::env::var("VCS_NO_HOOKS").is_ok_and(|v| !v.is_empty() && v != "0")
}

fn msg<T>(s: impl Into<String>) -> Result<T, Error> {
    Err(Error::Msg(s.into()))
}

/// Does a failure of this hook abort the operation?
fn gates(name: &str) -> bool {
    name.starts_with("pre-") || name == "commit-msg"
}

/// The shared `.vcs` directory (the main worktree's; equals the worktree's own `.vcs` outside linked worktrees).
pub fn common_dir(repo: &Repo) -> PathBuf {
    let vcs = repo.worktree().join(".vcs");
    match std::fs::read_to_string(vcs.join("commondir")) {
        Ok(s) if !s.trim().is_empty() => PathBuf::from(s.trim_end_matches('\n')),
        _ => vcs,
    }
}

pub fn hooks_dir(repo: &Repo) -> PathBuf {
    common_dir(repo).join("hooks")
}

#[derive(Debug, PartialEq, Eq)]
pub enum State {
    Missing,
    NotExecutable,
    /// Group- or world-writable file or directory: never run.
    Writable(PathBuf),
    Ready,
}

#[cfg(unix)]
pub fn state(dir: &Path, name: &str) -> State {
    use std::os::unix::fs::PermissionsExt;
    let file = dir.join(name);
    let Ok(m) = std::fs::metadata(&file) else {
        return State::Missing;
    };
    if !m.is_file() {
        return State::Missing;
    }
    if let Ok(dm) = std::fs::metadata(dir) {
        if dm.permissions().mode() & 0o022 != 0 {
            return State::Writable(dir.to_owned());
        }
    }
    if m.permissions().mode() & 0o022 != 0 {
        return State::Writable(file);
    }
    if m.permissions().mode() & 0o111 == 0 {
        return State::NotExecutable;
    }
    State::Ready
}

#[cfg(not(unix))]
pub fn state(dir: &Path, name: &str) -> State {
    if dir.join(name).is_file() {
        State::NotExecutable
    } else {
        State::Missing
    }
}

/// Context for one invocation.
pub struct Run<'a> {
    pub repo: &'a Repo,
    pub zone: &'a str,
    pub branch: &'a str,
    pub args: &'a [String],
    pub stdin: Option<&'a str>,
}

struct Settings {
    timeout: Duration,
    pass_env: Vec<String>,
}

fn settings() -> Result<Settings, Error> {
    let cfg = crate::config::Config::load()?;
    let timeout = match cfg.get("hooks.timeout") {
        None => DEFAULT_TIMEOUT_SECS,
        Some(v) => match v.trim().parse::<u64>() {
            Ok(n) if n > 0 => n,
            _ => return msg(format!("hooks.timeout must be a positive number of seconds, got {v:?}")),
        },
    };
    let pass_env = cfg
        .get("hooks.env")
        .unwrap_or_default()
        .split([',', ' '])
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    Ok(Settings {
        timeout: Duration::from_secs(timeout),
        pass_env,
    })
}

/// Environment variables every hook may see besides the VCS_* ones set by us.
const BASE_ENV: &[&str] = &["PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "LC_CTYPE", "TERM", "TMPDIR", "TZ"];

/// Run `name` if installed. Gating hooks (pre-*, commit-msg) turn a failure into an error;
/// post hooks only warn. Skipped by `--no-hooks` / `VCS_NO_HOOKS=1` and when not installed.
pub fn fire(r: &Run, name: &str) -> Result<(), Error> {
    if skipped() {
        return Ok(());
    }
    let dir = hooks_dir(r.repo);
    match state(&dir, name) {
        State::Missing => return Ok(()),
        State::NotExecutable => {
            eprintln!("warning: hook {name} is not executable; skipped (chmod 755 {})", dir.join(name).display());
            return Ok(());
        }
        State::Writable(p) => {
            let m = format!(
                "refusing to run hook {name}: {} is group- or world-writable (chmod go-w it, or skip hooks with --no-hooks)",
                p.display()
            );
            return if gates(name) { msg(m) } else { warn(Error::Msg(m)) };
        }
        State::Ready => {}
    }
    match execute(r, name, &dir) {
        Ok(()) => Ok(()),
        Err(e) if gates(name) => Err(e),
        Err(e) => warn(e),
    }
}

fn warn(e: Error) -> Result<(), Error> {
    eprintln!("warning: {e}");
    Ok(())
}

/// Run unconditionally (for `vcs hook run`): still checks permissions, ignores the skip switches.
pub fn run_explicit(r: &Run, name: &str) -> Result<(), Error> {
    let dir = hooks_dir(r.repo);
    match state(&dir, name) {
        State::Missing => msg(format!("hook {name} is not installed")),
        State::NotExecutable => msg(format!("hook {name} is not executable")),
        State::Writable(p) => msg(format!("refusing to run hook {name}: {} is group- or world-writable", p.display())),
        State::Ready => execute(r, name, &dir),
    }
}

fn execute(r: &Run, name: &str, dir: &Path) -> Result<(), Error> {
    let st = settings()?;
    let mut cmd = Command::new(dir.join(name));
    cmd.args(r.args)
        .current_dir(r.repo.worktree())
        .env_clear()
        .stdin(if r.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for k in BASE_ENV {
        if let Ok(v) = std::env::var(k) {
            cmd.env(k, v);
        }
    }
    for k in &st.pass_env {
        if let Ok(v) = std::env::var(k) {
            cmd.env(k, v);
        }
    }
    cmd.env("VCS_DIR", r.repo.worktree().join(".vcs"))
        .env("VCS_COMMON_DIR", common_dir(r.repo))
        .env("VCS_ZONE", r.zone)
        .env("VCS_BRANCH", r.branch)
        .env("VCS_HOOK", name);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // own group so a timeout can kill the whole tree
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Msg(format!("cannot run hook {name}: {e}")))?;
    if let (Some(data), Some(mut si)) = (r.stdin, child.stdin.take()) {
        let data = data.to_owned();
        std::thread::spawn(move || {
            let _ = si.write_all(data.as_bytes()); // a hook that does not read stdin is fine
        });
    }
    let reader = |mut s: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = s.read_to_end(&mut b);
            b
        })
    };
    let out = reader(Box::new(child.stdout.take().unwrap()));
    let err = reader(Box::new(child.stderr.take().unwrap()));
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() >= st.timeout => {
                timed_out = true;
                kill_group(&mut child);
                break child.wait().map_err(|e| Error::Msg(format!("hook {name}: {e}")))?;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return msg(format!("hook {name}: {e}")),
        }
    };
    // Descendants that outlive the hook keep the pipes open; do not wait for them forever.
    kill_group_by_pid(child.id());
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    let _ = std::io::stdout().write_all(&out);
    let err_s = String::from_utf8_lossy(&err).into_owned();
    if timed_out {
        return msg(format!(
            "hook {name} timed out after {}s and was killed (config hooks.timeout){}",
            st.timeout.as_secs(),
            tail(&err_s)
        ));
    }
    if status.success() {
        let _ = std::io::stderr().write_all(&err);
        return Ok(());
    }
    let how = match status.code() {
        Some(c) => format!("exited with status {c}"),
        None => signal_text(&status),
    };
    msg(format!("hook {name} {how}{}", tail(&err_s)))
}

fn tail(stderr: &str) -> String {
    let t = stderr.trim_end();
    if t.is_empty() {
        String::new()
    } else {
        format!("\n{t}")
    }
}

#[cfg(unix)]
fn signal_text(s: &std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match s.signal() {
        Some(n) => format!("was terminated by signal {n}"),
        None => "failed".into(),
    }
}
#[cfg(not(unix))]
fn signal_text(_: &std::process::ExitStatus) -> String {
    "failed".into()
}

#[cfg(unix)]
fn kill_group_by_pid(pid: u32) {
    // SAFETY: plain syscall on a process group we created (process_group(0) makes pgid == pid).
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}
#[cfg(not(unix))]
fn kill_group_by_pid(_: u32) {}

fn kill_group(child: &mut std::process::Child) {
    kill_group_by_pid(child.id());
    let _ = child.kill();
}

/// Convenience: branch checked out in `zone` ("" when unknown).
pub fn branch_of(repo: &Repo, zone: &str) -> String {
    repo.current_branch(zone).ok().flatten().unwrap_or_default()
}

pub fn fire_simple(repo: &Repo, zone: &str, name: &str, args: &[String]) -> Result<(), Error> {
    let branch = branch_of(repo, zone);
    fire(
        &Run {
            repo,
            zone,
            branch: &branch,
            args,
            stdin: None,
        },
        name,
    )
}

/// `commit-msg`: write `message` to a file, let the hook rewrite it, return the (possibly new) text.
pub fn commit_msg(repo: &Repo, zone: &str, message: String) -> Result<String, Error> {
    if skipped() || state(&hooks_dir(repo), "commit-msg") == State::Missing {
        return Ok(message);
    }
    let path = repo.worktree().join(".vcs").join("COMMIT_EDITMSG");
    std::fs::write(&path, &message).map_err(|source| Error::Io { path: path.clone(), source })?;
    let r = fire_simple(repo, zone, "commit-msg", &[path.to_string_lossy().into_owned()]);
    let new = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    r?;
    match new {
        Ok(m) if !m.trim().is_empty() => Ok(m),
        Ok(_) => msg("commit-msg hook left an empty message"),
        Err(source) => Err(Error::Io { path, source }),
    }
}

#[derive(Subcommand, Clone)]
pub enum HookCmd {
    /// Show which hooks are installed and whether they would run
    List,
    /// Run a hook by hand with the documented environment (ignores --no-hooks); extra arguments are passed through
    Run {
        name: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Copy an executable into .vcs/hooks/<name> (mode 755)
    Install {
        name: String,
        file: PathBuf,
        /// Replace an installed hook
        #[arg(short, long)]
        force: bool,
    },
}

fn known(name: &str) -> Result<(), Error> {
    if HOOKS.contains(&name) {
        Ok(())
    } else {
        msg(format!("unknown hook {name:?}\n\nKnown hooks: {}", HOOKS.join(", ")))
    }
}

pub fn run(cmd: &HookCmd) -> Result<(), Error> {
    let cwd = std::env::current_dir().map_err(|source| Error::Io { path: ".".into(), source })?;
    let repo = Repo::discover(&cwd)?;
    let dir = hooks_dir(&repo);
    match cmd {
        HookCmd::List => {
            for n in HOOKS {
                let s = match state(&dir, n) {
                    State::Missing => "not installed".to_owned(),
                    State::NotExecutable => "installed, NOT executable (skipped)".into(),
                    State::Writable(p) => format!("installed, REFUSED: {} is group/world-writable", p.display()),
                    State::Ready => "installed".into(),
                };
                println!("{n:<14} {s}");
            }
            if skipped() {
                println!("(hooks are currently disabled by --no-hooks / VCS_NO_HOOKS)");
            }
        }
        HookCmd::Run { name, args } => {
            known(name)?;
            let zone = repo.zones()?.owner("").to_owned();
            let branch = branch_of(&repo, &zone);
            let mut input = String::new();
            if name == "pre-publish" || name == "post-publish" {
                let _ = std::io::stdin().read_to_string(&mut input);
            }
            run_explicit(
                &Run {
                    repo: &repo,
                    zone: &zone,
                    branch: &branch,
                    args,
                    stdin: (!input.is_empty()).then_some(&input),
                },
                name,
            )?;
        }
        HookCmd::Install { name, file, force } => {
            known(name)?;
            let dest = dir.join(name);
            if !force && std::fs::symlink_metadata(&dest).is_ok() {
                return msg(format!("hook {name} is already installed; use --force to replace it"));
            }
            let data = std::fs::read(file).map_err(|source| Error::Io { path: file.clone(), source })?;
            std::fs::create_dir_all(&dir).map_err(|source| Error::Io { path: dir.clone(), source })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
            }
            let tmp = dir.join(format!(".{name}.tmp{}", std::process::id()));
            std::fs::write(&tmp, data).map_err(|source| Error::Io { path: tmp.clone(), source })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
                    .map_err(|source| Error::Io { path: tmp.clone(), source })?;
            }
            std::fs::rename(&tmp, &dest).map_err(|source| Error::Io { path: dest.clone(), source })?;
            println!("installed {}", dest.display());
        }
    }
    Ok(())
}

/// Head commit of `zone` for hook arguments ("" = none).
pub fn head_arg(repo: &Repo, zone: &str) -> String {
    repo.head_commit(zone).ok().flatten().map(|h| h.to_string()).unwrap_or_default()
}

const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// One stdin line of the publish hooks: `<zone> <branch> <old-id|0...0> <new-id>`.
pub fn line(zone: &str, branch: &str, old: Option<vcs_objects::ObjectId>, new: vcs_objects::ObjectId) -> String {
    let old = old.map(|o| o.to_string()).unwrap_or_else(|| ZERO.to_owned());
    format!("{zone} {branch} {old} {new}\n")
}

/// What `publish` would send: every selected checked-out branch whose head differs from the remote-tracking head.
pub fn publish_lines(repo: &Repo, want: Option<&[String]>) -> String {
    let (Ok(ws), Ok(remote)) = (repo.workspace(), repo.remote_refs()) else {
        return String::new();
    };
    let mut out = String::new();
    for (zone, branch) in &ws.subs {
        if want.is_some_and(|w| !w.iter().any(|z| z == zone)) {
            continue;
        }
        let Ok(Some(head)) = repo.head_commit(zone) else { continue };
        let old = remote.head(zone, branch);
        if old != Some(head) {
            out.push_str(&line(zone, branch, old, head));
        }
    }
    out
}

pub fn fire_with_stdin(repo: &Repo, name: &str, stdin: &str) -> Result<(), Error> {
    let zone = repo.zones()?.owner("").to_owned();
    let branch = branch_of(repo, &zone);
    fire(
        &Run {
            repo,
            zone: &zone,
            branch: &branch,
            args: &[],
            stdin: Some(stdin),
        },
        name,
    )
}
