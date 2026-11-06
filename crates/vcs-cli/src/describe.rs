//! `vcs describe [rev] [--tags] [--always]`: name a commit after the nearest tag behind it.
//!
//! Output is `<tag>` when the commit is tagged, else `<tag>-<n>-g<short id>` where `n` is the
//! number of commits reachable from the revision but not from the tag. Only annotated tags are
//! used unless `--tags`. The nearest tag is the first one met walking parents breadth-first from
//! the revision; when several tags sit on one commit the annotated one with the newest tagger
//! time wins, then the name.

use super::{open, zone_of_cwd};
use clap::Args;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use vcs_objects::ObjectId;
use vcs_repo::{Error, Repo};

#[derive(Args, Clone)]
pub struct DescribeArgs {
    /// Revision to describe (default HEAD)
    pub rev: Option<String>,
    /// Use lightweight tags too (default: annotated tags only)
    #[arg(long)]
    pub tags: bool,
    /// Show the abbreviated commit id when no tag is found
    #[arg(long)]
    pub always: bool,
    #[arg(long)]
    pub zone: Option<String>,
    /// Append MARK (default `-dirty`) when tracked files differ from HEAD (staged or unstaged); HEAD only
    #[arg(long, value_name = "MARK", num_args = 0..=1, require_equals = true, default_missing_value = "-dirty")]
    pub dirty: Option<String>,
}

/// `(tag name, distance)` of the nearest tag, or `None`.
pub fn nearest(repo: &Repo, zone: &str, start: ObjectId, tags: bool) -> Result<Option<(String, usize)>, Error> {
    // commit -> best tag name on it
    let mut by_commit: BTreeMap<ObjectId, (u64, String)> = BTreeMap::new();
    for t in repo.tag_list(zone)? {
        let rank = match &t.annotation {
            Some(a) => a.tagger.timestamp.saturating_add(1),
            None if tags => 0,
            None => continue,
        };
        let better = by_commit
            .get(&t.commit)
            .is_none_or(|(r, n)| (rank, std::cmp::Reverse(&t.name)) > (*r, std::cmp::Reverse(n)));
        if better {
            by_commit.insert(t.commit, (rank, t.name));
        }
    }
    let graph = repo.parent_index(&[start]);
    let mut seen = BTreeSet::new();
    let mut q = VecDeque::from([start]);
    while let Some(id) = q.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        if let Some((_, name)) = by_commit.get(&id) {
            let dist = graph.count_not_reachable_from(start, id)?;
            return Ok(Some((name.clone(), dist)));
        }
        q.extend(graph.parents(&id)?);
    }
    Ok(None)
}

pub fn run(a: &DescribeArgs) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = repo.resolve_zone(&zone_of_cwd(&repo, &cwd, a.zone.clone())?)?;
    let id = repo.resolve_rev(&zone, a.rev.as_deref().unwrap_or("HEAD"))?;
    let mark = match &a.dirty {
        Some(m) => {
            if a.rev.is_some() {
                return Err(Error::Msg("--dirty is incompatible with a revision; it describes HEAD".into()));
            }
            let st = repo.status()?;
            if st.zones.iter().any(|z| !z.staged.is_empty() || !z.unstaged.is_empty()) {
                m.as_str()
            } else {
                ""
            }
        }
        None => "",
    };
    match nearest(&repo, &zone, id, a.tags)? {
        Some((name, 0)) => println!("{name}{mark}"),
        Some((name, n)) => println!("{name}-{n}-g{}{mark}", id.short()),
        None if a.always => println!("{}{mark}", id.short()),
        None => {
            return Err(Error::Msg(if a.tags {
                "no tags can describe this commit; try --always".into()
            } else {
                "no annotated tags can describe this commit; try --tags or --always".into()
            }))
        }
    }
    Ok(())
}
