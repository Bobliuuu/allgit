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
    /// Also delete files matched by `.vcsignore` (ignored directories need -d as well)
    #[arg(short = 'x')]
    pub ignored: bool,
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

/// Remove the empty directories below (and including) `dir`.
fn prune_empty(dir: &std::path::Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                prune_empty(&e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

pub fn clean(a: &CleanArgs) -> Result<(), Error> {
    if !a.force && !a.dry_run {
        return Err(Error::Msg(
            "refusing to clean without -f (delete) or -n (list what would be deleted)".into(),
        ));
    }
    let (repo, cwd) = open()?;
    // Only the untracked walk: a clean does not need every tracked file stat-ed.
    let (mut untracked, _unsupported) = repo.untracked_only(Default::default())?;
    // file-level view, only when it is needed: -d deletes inside directories, -x adds the ignored files
    let (untracked_files, ignored_all) = if a.dirs || a.ignored { repo.others()? } else { (vec![], vec![]) };
    let ignored_dirs: Vec<String> = if a.ignored {
        ignored_all.iter().filter(|f| f.ends_with('/')).cloned().collect()
    } else {
        vec![]
    };
    let ignored_files: Vec<String> = if a.ignored { ignored_all.clone() } else { vec![] };
    if a.ignored {
        // a listed entry per ignored file/dir that is not inside an untracked directory entry already listed
        let dirs_listed: Vec<String> = untracked.iter().filter(|u| u.ends_with('/')).cloned().collect();
        // without -d, git does not go into a directory that holds no tracked file
        let tracked_dirs: std::collections::BTreeSet<String> = if a.dirs {
            Default::default()
        } else {
            repo.tracked_files()?
                .iter()
                .flat_map(|(p, _, _)| p.match_indices('/').map(|(i, _)| p[..i].to_owned()).collect::<Vec<_>>())
                .collect()
        };
        for f in &ignored_all {
            if dirs_listed.iter().any(|d| f.starts_with(d.as_str())) {
                continue;
            }
            let parent = f.trim_end_matches('/').rsplit_once('/').map(|(p, _)| p);
            if !a.dirs && parent.is_some_and(|p| !tracked_dirs.contains(p)) {
                continue;
            }
            untracked.push(f.clone());
        }
        untracked.sort();
        untracked.dedup();
    }
    let filters: Vec<String> = a.paths.iter().map(|p| repo.rel_path(&cwd, p)).collect::<Result<_, _>>()?;
    let mut dirs = std::collections::BTreeSet::new();
    let mut n = 0;
    for u in &untracked {
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
            if ignored_dirs.iter().any(|d| d == u) {
                // an ignored directory with nothing tracked below (listed only with -x)
                std::fs::remove_dir_all(&abs).map_err(|source| Error::Io { path: abs.clone(), source })?;
            } else {
                // an untracked directory: remove its untracked files (and ignored ones with -x), never the
                // ignored files of a run without -x; the directory itself goes when nothing is left in it
                let pre = format!("{rel}/");
                for f in untracked_files.iter().chain(ignored_files.iter()).filter(|f| f.starts_with(&pre)) {
                    if f.ends_with('/') {
                        std::fs::remove_dir_all(repo.worktree().join(f.trim_end_matches('/')))
                    } else {
                        std::fs::remove_file(repo.worktree().join(f))
                    }
                    .map_err(|source| Error::Io {
                        path: repo.worktree().join(f),
                        source,
                    })?;
                }
                prune_empty(&abs);
            }
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
