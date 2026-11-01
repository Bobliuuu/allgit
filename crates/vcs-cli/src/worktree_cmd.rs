//! `vcs worktree add|list|remove`. Logic and layout: `vcs_repo::worktree`, docs/worktrees.md.

use clap::Subcommand;
use std::path::PathBuf;
use vcs_repo::{Error, Repo};

#[derive(Subcommand, Clone)]
pub enum WorktreeCmd {
    /// Create a linked worktree sharing this repository's objects and refs
    Add {
        path: PathBuf,
        /// Root-zone branch to check out (default: the directory name; created here if new)
        branch: Option<String>,
    },
    /// List worktrees and what each has checked out
    List,
    /// Delete a linked worktree (refuses when it has uncommitted work unless --force)
    Remove {
        path: PathBuf,
        #[arg(long)]
        force: bool,
    },
}

pub fn run(repo: &Repo, cmd: WorktreeCmd) -> Result<(), Error> {
    match cmd {
        WorktreeCmd::Add { path, branch } => {
            let w = repo.worktree_add(&path, branch.as_deref())?;
            let b = w.subs.first().map(|s| s.1.as_str()).unwrap_or("");
            println!("created worktree {} on root/{b}", w.path.display());
            println!("  other zones: `cd` there and run `vcs zone add <name> [branch]`");
        }
        WorktreeCmd::List => {
            for w in repo.worktree_list()? {
                let subs: Vec<String> = w.subs.iter().map(|(z, b)| format!("{z}/{b}")).collect();
                println!(
                    "{}{} {}{}",
                    if w.current { "* " } else { "  " },
                    w.path.display(),
                    if w.main { "(main) " } else { "" },
                    if w.missing {
                        "[MISSING: `vcs worktree remove --force` it]".to_owned()
                    } else {
                        subs.join(" ")
                    }
                );
            }
        }
        WorktreeCmd::Remove { path, force } => {
            repo.worktree_remove(&path, force)?;
            println!("removed worktree {}", path.display());
        }
    }
    Ok(())
}
