//! `vcs-watchd`: the filesystem watcher daemon behind `vcs watch` and the automatic watcher.
//!
//! A separate binary on purpose: the notify/FSEvents backend links CoreFoundation and CoreServices on
//! macOS, and dyld loading those costs about 2 ms of every `vcs` process, watcher or not.
//! Usage: `vcs-watchd [watch] [--idle-exit SECS]`, run inside the working tree.

use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

fn main() -> ExitCode {
    let mut idle: Option<u64> = None;
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().is_some_and(|a| a == "watch") {
        args.next();
    }
    while let Some(a) = args.next() {
        match a.as_str() {
            "--idle-exit" => idle = args.next().and_then(|v| v.parse().ok()),
            other => {
                eprintln!("vcs-watchd: unexpected argument {other}");
                return ExitCode::from(2);
            }
        }
    }
    let run = || -> Result<(), vcs_repo::error::Error> {
        let cwd = std::env::current_dir().map_err(|source| vcs_repo::error::Error::Io { path: ".".into(), source })?;
        let repo = vcs_repo::Repo::discover(&cwd)?;
        eprintln!(
            "watching {} (Ctrl-C to stop); `vcs status` uses the journal while this runs",
            repo.worktree().display()
        );
        vcs_watchd::run_idle(&repo, &AtomicBool::new(false), idle.map(Duration::from_secs))
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
