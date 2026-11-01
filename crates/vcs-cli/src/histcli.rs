//! CLI side of detached HEAD, `branch -m`, `branch <name> <rev>` and `vcs rev-list`.
//! The mechanics live in `vcs_repo::detach` and `vcs_repo::revrange`.

use super::{open, zone_of_cwd};
use crate::json::{doc, J};
use clap::Args;
use vcs_objects::names::DETACHED_BRANCH;
use vcs_objects::ObjectId;
use vcs_repo::{Error, Repo, RevSpec};

/// How a zone's checked-out ref reads in `vcs status` and `vcs commit`.
pub fn head_line(branch: &str, head: Option<ObjectId>) -> String {
    let at = head.map_or_else(|| "(none)".to_owned(), |h| h.short());
    if branch == DETACHED_BRANCH {
        format!("HEAD detached at {at}  (commits made here belong to no branch; keep them with `vcs switch -c <name>`)")
    } else {
        format!("branch {branch} @ {at}")
    }
}

/// The ref name to print in `[zone/<name> id]` after a commit.
pub fn ref_label(branch: &str) -> &str {
    if branch == DETACHED_BRANCH {
        "HEAD detached"
    } else {
        branch
    }
}

/// `vcs branch` listing: the detached HEAD first, then the branches.
pub fn print_branches(repo: &Repo, zone: &str) -> Result<(), Error> {
    if let Some(d) = repo.detached_head(zone)? {
        println!("* {zone}/(HEAD detached at {})", d.short());
    }
    for (b, id, cur) in repo.branches(zone)? {
        println!(
            "{} {zone}/{b:<24} {}",
            if cur { "*" } else { " " },
            id.map_or_else(|| "(none)".to_owned(), |i| i.short())
        );
    }
    Ok(())
}

fn subject_of(repo: &Repo, id: &ObjectId) -> String {
    repo.commit_of(id)
        .map(|c| c.message.lines().next().unwrap_or("").to_owned())
        .unwrap_or_default()
}

/// `vcs switch [-c] [--detach] <branch|rev>`.
pub fn switch(branch: &str, zone: Option<String>, create: bool, detach: bool) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = zone_of_cwd(&repo, &cwd, zone)?;
    let old = crate::hooks::head_arg(&repo, &zone);
    if detach {
        let id = repo.checkout_detached(&zone, branch)?;
        let subject = subject_of(&repo, &id);
        println!("zone {zone}: HEAD is now at {} {subject} (detached)", id.short());
        eprintln!(
            "note: this is a detached HEAD: commits made here are on no branch. Keep them with `vcs switch -c <name>` \
             (or `vcs branch <name>`); `vcs switch <branch>` leaves it."
        );
        crate::hooks::fire_simple(&repo, &zone, "post-checkout", &[old, crate::hooks::head_arg(&repo, &zone)])?;
        return Ok(());
    }
    if create {
        let h = repo.create_branch(&zone, branch)?;
        println!("created {zone}/{branch} at {}", h.short());
    }
    let was = repo.detached_head(&zone)?;
    if let Err(e) = repo.checkout(&zone, branch) {
        // a tag, commit id or HEAD~2 is not a branch: say how to get there
        if repo.refs()?.head(&repo.resolve_zone(&zone)?, branch).is_none() && repo.resolve_rev(&zone, branch).is_ok() {
            return Err(Error::Msg(format!(
                "{branch:?} is not a branch; `vcs switch --detach {branch}` checks it out as a detached HEAD"
            )));
        }
        return Err(e);
    }
    println!("zone {zone}: switched to {branch}");
    if let Some(d) = was {
        let lost = repo.commits_only_in(&zone, d)?;
        if !lost.is_empty() {
            eprintln!(
                "warning: leaving {} commit(s) behind that no branch or tag reaches (newest first):",
                lost.len()
            );
            for id in lost.iter().take(10) {
                eprintln!("  {} {}", id.short(), subject_of(&repo, id));
            }
            if lost.len() > 10 {
                eprintln!("  ... and {} more", lost.len() - 10);
            }
            eprintln!(
                "keep them: `vcs branch <name> {}`; or put the detached HEAD back with `vcs op undo`",
                lost[0].short()
            );
        }
    }
    crate::hooks::fire_simple(&repo, &zone, "post-checkout", &[old, crate::hooks::head_arg(&repo, &zone)])?;
    Ok(())
}

/// The new forms of `vcs branch`: `-m old new`, `-M old new`, `<name> <rev>`. Returns false when
/// the arguments are the old forms (the caller handles them).
pub fn branch_ext(
    name: &Option<String>,
    start: &Option<String>,
    zone: Option<String>,
    rename: bool,
    force_rename: bool,
) -> Result<bool, Error> {
    if !rename && !force_rename && start.is_none() {
        return Ok(false);
    }
    let (repo, cwd) = open()?;
    let zone = zone_of_cwd(&repo, &cwd, zone)?;
    if rename || force_rename {
        let (old, new) = match (name, start) {
            (Some(o), Some(n)) => (o.clone(), n.clone()),
            (Some(n), None) => {
                // `git branch -m new` renames the checked-out branch
                match repo.current_branch(&zone)? {
                    Some(cur) => (cur, n.clone()),
                    None => {
                        return Err(Error::Msg(
                            "detached HEAD: name the branch to rename (`vcs branch -m <old> <new>`)".into(),
                        ))
                    }
                }
            }
            _ => return Err(Error::Msg("usage: vcs branch -m [<old>] <new>".into())),
        };
        match repo.branch_rename(&zone, &old, &new, force_rename)? {
            Some(h) => println!("renamed {zone}/{old} -> {zone}/{new} (at {})", h.short()),
            None => println!("renamed {zone}/{old} -> {zone}/{new} (no commits yet)"),
        }
        return Ok(true);
    }
    let (Some(n), Some(rev)) = (name, start) else { return Ok(false) };
    let h = repo.create_branch_from(&zone, n, Some(rev))?;
    println!("created {zone}/{n} at {}", h.short());
    Ok(true)
}

// ---------- rev-list ----------

#[derive(Args, Clone)]
pub struct RevListArgs {
    /// Revisions: `A`, `^A`, `A..B`, `A...B` (default: HEAD)
    pub revs: Vec<String>,
    /// Also start from every branch and tag of the zone (and a detached HEAD)
    #[arg(long)]
    pub all: bool,
    /// Leave out the history of this revision (repeatable; same as `^REV`)
    #[arg(long = "not", value_name = "REV")]
    pub not: Vec<String>,
    /// Print only the number of commits
    #[arg(long)]
    pub count: bool,
    /// At most this many commits
    #[arg(short = 'n', long = "max-count", value_name = "N")]
    pub max_count: Option<usize>,
    /// Oldest first (after the limit)
    #[arg(long)]
    pub reverse: bool,
    /// Zone (default: the zone containing the current directory)
    #[arg(long)]
    pub zone: Option<String>,
}

/// `vcs rev-list`: commit ids of a revision list, newest first, in `log` order.
pub fn rev_list(a: &RevListArgs, json: bool) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = repo.resolve_zone(&zone_of_cwd(&repo, &cwd, a.zone.clone())?)?;
    let mut spec = RevSpec::parse(&a.revs)?;
    spec.exclude.extend(a.not.iter().cloned());
    spec.all = a.all;
    let mut ids = repo.rev_list_ids(&zone, &spec, a.max_count.unwrap_or(usize::MAX))?;
    if a.reverse {
        ids.reverse();
    }
    if json {
        println!(
            "{}",
            doc(
                "rev-list",
                vec![
                    ("zone", J::Str(zone.clone())),
                    ("count", J::Num(ids.len() as u64)),
                    ("ids", J::Arr(ids.iter().map(|i| J::Str(i.to_hex())).collect())),
                ],
            )
            .render()
        );
        return Ok(());
    }
    if a.count {
        println!("{}", ids.len());
        return Ok(());
    }
    use std::io::Write;
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for i in &ids {
        writeln!(out, "{}", i.to_hex()).map_err(|source| Error::Io {
            path: "stdout".into(),
            source,
        })?;
    }
    Ok(())
}
