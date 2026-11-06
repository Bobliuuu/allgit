//! `vcs rerere [status|remaining|diff|forget <path>...|clear|gc]` (docs/rerere.md).
//!
//! With no subcommand it does what `git rerere` does: record the resolution of every tracked file
//! whose markers are gone and replay recorded resolutions into files that still have them.

use super::open;
use crate::config::Config;
use clap::{Args, Subcommand};
use vcs_repo::Error;

#[derive(Args, Clone)]
pub struct RerereArgs {
    #[command(subcommand)]
    pub cmd: Option<RerereCmd>,
}

#[derive(Subcommand, Clone)]
pub enum RerereCmd {
    /// List the paths of the conflict in progress that rerere is tracking
    Status,
    /// List the conflicted paths rerere did not resolve (still need your edit)
    Remaining,
    /// Show what you changed in the tracked files since the conflict was written
    Diff,
    /// Forget the recorded resolution for these paths of the conflict in progress, so the next one is recorded afresh
    Forget {
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Drop the unresolved preimages of the conflict in progress and stop tracking it
    Clear,
    /// Delete resolutions unused for gc.rerereResolved days (60) and conflicts unresolved for gc.rerereUnresolved days (15)
    Gc,
}

pub fn run(a: &RerereArgs) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    match &a.cmd {
        None => {
            for l in repo.rerere_update()? {
                println!("{l}");
            }
        }
        Some(RerereCmd::Status) => {
            for p in repo.rerere_status()? {
                println!("{p}");
            }
        }
        Some(RerereCmd::Remaining) => {
            for p in repo.rerere_remaining()? {
                println!("{p}");
            }
        }
        Some(RerereCmd::Diff) => print!("{}", repo.rerere_diff()?),
        Some(RerereCmd::Forget { paths }) => {
            let rel = paths.iter().map(|p| repo.rel_path(&cwd, p)).collect::<Result<Vec<_>, _>>()?;
            for l in repo.rerere_forget(&rel)? {
                println!("{l}");
            }
        }
        Some(RerereCmd::Clear) => repo.rerere_clear()?,
        Some(RerereCmd::Gc) => {
            let (res, unres) = Config::load()?.rerere_gc_days()?;
            let r = repo.rerere_gc(res, unres)?;
            println!("removed {} resolution(s) and {} unresolved conflict(s)", r.resolved, r.unresolved);
        }
    }
    Ok(())
}
