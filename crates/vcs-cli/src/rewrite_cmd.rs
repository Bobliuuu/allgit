//! `vcs rewrite`: rewrite the history of one zone (the `git filter-branch` / `git filter-repo`
//! core). The engine is `Repo::rewrite_history`; this module only parses flags and prints.
//! One run is one operation: `vcs op undo` puts every branch and tag back. Contract and limits:
//! docs/history-rewrite.md.

use crate::json::{doc, J};
use crate::{open, zone_of_cwd};
use clap::Args;
use vcs_repo::{Error, RewriteSpec};

#[derive(Args, Clone)]
pub struct RewriteArgs {
    /// Make this directory the root of every commit; commits without it vanish (filter-repo --subdirectory-filter)
    #[arg(long, value_name = "DIR")]
    pub subdirectory_filter: Option<String>,
    /// Keep only this file or directory (repeatable; filter-repo --path)
    #[arg(long, value_name = "PATH")]
    pub keep: Vec<String>,
    /// Remove this file or directory from every commit (repeatable; filter-repo --path with --invert-paths)
    #[arg(long, value_name = "PATH")]
    pub remove: Vec<String>,
    /// Shell command: old message on stdin, new message on stdout
    #[arg(long, value_name = "CMD")]
    pub msg_filter: Option<String>,
    /// Shell script that may set GIT_AUTHOR_NAME / GIT_AUTHOR_EMAIL / GIT_AUTHOR_DATE ('@SECONDS'); GIT_COMMITTER_* are
    /// readable but changes to them are ignored (a commit has one signature).
    #[arg(long, value_name = "SCRIPT")]
    pub env_filter: Option<String>,
    /// Shell command run in a checkout of each commit's tree (SLOW: one checkout and one re-hash per commit)
    #[arg(long, value_name = "CMD")]
    pub tree_filter: Option<String>,
    /// Shell command: tag name on stdin, new tag name on stdout (`cat` keeps the names; tags are moved either way)
    #[arg(long, value_name = "CMD")]
    pub tag_name_filter: Option<String>,
    /// Rewrite every branch of the zone, not only the checked-out one
    #[arg(long)]
    pub all_branches: bool,
    /// Keep commits that the filters left empty (default: drop them)
    #[arg(long)]
    pub keep_empty: bool,
    /// Rewrite although the changed commits are already published (held by origin as last fetched)
    #[arg(short = 'f', long)]
    pub force: bool,
    /// Show what would change; move nothing
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Zone (default: the zone containing the current directory)
    #[arg(long)]
    pub zone: Option<String>,
}

pub fn run(a: &RewriteArgs, json: bool) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = repo.resolve_zone(&zone_of_cwd(&repo, &cwd, a.zone.clone())?)?;
    let spec = RewriteSpec {
        subdirectory: a.subdirectory_filter.clone(),
        keep: a.keep.clone(),
        remove: a.remove.clone(),
        msg_filter: a.msg_filter.clone(),
        env_filter: a.env_filter.clone(),
        tree_filter: a.tree_filter.clone(),
        tag_name_filter: a.tag_name_filter.clone(),
        all_branches: a.all_branches,
        prune_empty: !a.keep_empty,
        force: a.force,
        dry_run: a.dry_run,
    };
    let r = repo.rewrite_history(&zone, &spec)?;
    if json {
        let s = |x: &str| J::Str(x.to_owned());
        let branches = r
            .branches
            .iter()
            .map(|(b, o, n)| J::Arr(vec![s(b), J::Str(o.to_hex()), J::Str(n.to_hex())]))
            .collect();
        let tags = r
            .tags
            .iter()
            .map(|(o, n, t)| J::Arr(vec![s(o), s(n), J::Str(t.to_hex())]))
            .collect();
        println!(
            "{}",
            doc(
                "rewrite",
                vec![
                    ("zone", s(&r.zone)),
                    ("dry_run", J::Bool(r.dry_run)),
                    ("seen", J::Num(r.seen as u64)),
                    ("rewritten", J::Num(r.rewritten as u64)),
                    ("dropped", J::Num(r.dropped as u64)),
                    ("branches", J::Arr(branches)),
                    ("tags", J::Arr(tags)),
                ]
            )
            .render()
        );
        return Ok(());
    }
    let verb = if r.dry_run { "would rewrite" } else { "rewrote" };
    println!(
        "{verb} {} of {} commit(s) of zone {} ({} dropped as empty)",
        r.rewritten, r.seen, r.zone, r.dropped
    );
    for (b, o, n) in &r.branches {
        println!("  branch {b}: {} -> {}", o.short(), n.short());
    }
    for (o, n, t) in &r.tags {
        if o == n {
            println!("  tag {o}: moved to {}", t.short());
        } else {
            println!("  tag {o} -> {n}: at {}", t.short());
        }
    }
    for t in &r.tags_deleted {
        println!("  tag {t}: deleted (its commit became empty)");
    }
    if r.committer_changes_ignored > 0 {
        eprintln!(
            "warning: the env filter changed GIT_COMMITTER_* on {} commit(s); a vcs commit has one signature (the author), those changes were ignored",
            r.committer_changes_ignored
        );
    }
    if r.published_changed > 0 {
        eprintln!(
            "warning: {} published commit(s) were changed (--force); replace the remote with `vcs publish --force`",
            r.published_changed
        );
    }
    if r.dry_run {
        println!("nothing was changed (dry run)");
    } else if r.rewritten + r.dropped + r.tags.len() + r.tags_deleted.len() > 0 {
        println!("undo with `vcs op undo`; the old commits stay in the store until `vcs gc`");
    } else {
        println!("nothing to rewrite: the filters changed no commit");
    }
    Ok(())
}
