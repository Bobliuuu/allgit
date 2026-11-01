//! Two everyday commands vcs lacked: `reset` and `clean`. See docs/git-to-vcs.md.
//!
//! `vcs reset [--soft|--mixed|--hard] [REV] [-- PATHS]`
//!   Like git, with one difference: it acts on ONE zone (the zone of the current directory, or
//!   `--zone`), because every zone has its own branch head. With paths it only resets index
//!   entries to HEAD (same as `restore --staged`).
//! `vcs clean -f|-n [-d] [PATHS]`
//!   Delete untracked files. Refuses to run without `-f` or `-n` (as git does by default).

use super::{open, zone_of_cwd};
use clap::Args;
use vcs_repo::{Error, ResetMode};

#[derive(Args, Clone)]
pub struct ResetArgs {
    /// Move only the branch head; keep the index and the files (changes show as staged)
    #[arg(long, conflicts_with_all = ["mixed", "hard"])]
    pub soft: bool,
    /// Move the head and reset the index to it; keep the files (the default)
    #[arg(long, conflicts_with_all = ["soft", "hard"])]
    pub mixed: bool,
    /// Move the head and reset the index AND the files of the zone; uncommitted work in the zone is lost
    #[arg(long, conflicts_with_all = ["soft", "mixed"])]
    pub hard: bool,
    /// Zone (default: the zone containing the current directory)
    #[arg(long)]
    pub zone: Option<String>,
    /// Revision to move to (default HEAD): a branch, tag, commit id prefix, with ~N or ^
    pub rev: Option<String>,
    /// Only unstage these paths (index entries return to HEAD); the head does not move
    #[arg(last = true)]
    pub paths: Vec<String>,
}

#[derive(Args, Clone)]
pub struct CleanArgs {
    /// Delete (required unless --dry-run, like git's clean.requireForce)
    #[arg(short = 'f', long)]
    pub force: bool,
    /// Only list what would be deleted
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Also delete untracked directories (here: untracked files inside them are always listed, so this only removes the emptied directories)
    #[arg(short = 'd')]
    pub dirs: bool,
    /// Only below these paths
    pub paths: Vec<String>,
}

pub fn reset(a: &ResetArgs) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let mode = if a.hard {
        ResetMode::Hard
    } else if a.soft {
        ResetMode::Soft
    } else {
        ResetMode::Mixed
    };
    let zone = zone_of_cwd(&repo, &cwd, a.zone.clone())?;
    // `reset path...` without `--`: a first argument that is not a revision but names a file is a path.
    let (rev, paths) = match &a.rev {
        Some(r) if a.paths.is_empty() && repo.resolve_rev(&zone, r).is_err() && cwd.join(r).exists() => (None, vec![r.clone()]),
        r => (r.clone(), a.paths.clone()),
    };
    if !paths.is_empty() {
        if a.soft || a.hard {
            return Err(Error::Msg("cannot do a --soft or --hard reset with paths".into()));
        }
        if rev.as_deref().is_some_and(|r| r != "HEAD") {
            return Err(Error::Msg(
                "reset <rev> -- <paths> (index entries from another revision) has no vcs equivalent;\n\
                 use `vcs restore --staged <paths>` for HEAD, or `vcs cat-file -p <rev>:<path>` and `vcs add`"
                    .into(),
            ));
        }
        let rel: Vec<String> = paths.iter().map(|p| repo.rel_path(&cwd, p)).collect::<Result<_, _>>()?;
        let rep = repo.restore(&rel, true)?;
        println!("unstaged {} index entr(ies)", rep.restored);
        return Ok(());
    }
    let target = repo.resolve_rev(&zone, rev.as_deref().unwrap_or("HEAD"))?;
    let rep = repo.reset(&zone, &target, mode)?;
    let subject = repo.commit_of(&target)?.message.lines().next().unwrap_or("").to_owned();
    println!("zone {}/{} is now at {} {subject}", rep.zone, rep.branch, rep.new.short());
    if mode == ResetMode::Hard {
        println!(
            "  {} file(s) written or removed, {} index entr(ies) changed",
            rep.files_changed, rep.index_changed
        );
    } else if mode == ResetMode::Mixed && rep.index_changed > 0 {
        println!("  {} index entr(ies) reset; the files were left alone", rep.index_changed);
    }
    Ok(())
}

pub fn clean(a: &CleanArgs) -> Result<(), Error> {
    if !a.force && !a.dry_run {
        return Err(Error::Msg(
            "refusing to clean without -f (delete) or -n (list what would be deleted)".into(),
        ));
    }
    let (repo, cwd) = open()?;
    let st = repo.status_auto(Default::default())?;
    let filters: Vec<String> = a.paths.iter().map(|p| repo.rel_path(&cwd, p)).collect::<Result<_, _>>()?;
    let mut dirs = std::collections::BTreeSet::new();
    let mut n = 0;
    for u in &st.untracked {
        let rel = u.trim_end_matches('/');
        if !filters.is_empty()
            && !filters
                .iter()
                .any(|f| f.is_empty() || rel == f || rel.starts_with(&format!("{f}/")))
        {
            continue;
        }
        let abs = repo.worktree().join(rel);
        n += 1;
        if a.dry_run {
            println!("Would remove {u}");
            continue;
        }
        let md = std::fs::symlink_metadata(&abs).map_err(|source| Error::Io { path: abs.clone(), source })?;
        if md.is_dir() {
            if !a.dirs {
                println!("Skipping directory {u} (use -d)");
                n -= 1;
                continue;
            }
            std::fs::remove_dir_all(&abs).map_err(|source| Error::Io { path: abs.clone(), source })?;
        } else {
            std::fs::remove_file(&abs).map_err(|source| Error::Io { path: abs.clone(), source })?;
            if let Some(p) = abs.parent() {
                dirs.insert(p.to_owned());
            }
        }
        println!("Removing {u}");
    }
    if a.dirs {
        for mut d in dirs {
            while d != repo.worktree() && d.starts_with(repo.worktree()) && std::fs::remove_dir(&d).is_ok() {
                if !d.pop() {
                    break;
                }
            }
        }
    }
    if n == 0 {
        println!("nothing to clean");
    }
    Ok(())
}
