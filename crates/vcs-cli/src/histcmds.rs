//! History plumbing: `merge-base A B` and `diff-tree A B` (changed files between two commits,
//! reading only the directories that differ). Both accept the global `--json`.

use super::{open, zone_of_cwd};
use crate::json::{doc, J};
use clap::Args;
use std::io::Write;
use vcs_repo::treeops;
use vcs_repo::Error;

fn write_out(text: String) -> Result<(), Error> {
    std::io::stdout().lock().write_all(text.as_bytes()).map_err(|source| Error::Io {
        path: "stdout".into(),
        source,
    })
}

#[derive(Args, Clone)]
pub struct MergeBaseArgs {
    /// First revision
    pub a: String,
    /// Second revision
    pub b: String,
    /// Print every best common ancestor, not just the first
    #[arg(long)]
    pub all: bool,
    /// Zone (default: the zone containing the current directory)
    #[arg(long)]
    pub zone: Option<String>,
}

pub fn merge_base(a: &MergeBaseArgs, json: bool) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = repo.resolve_zone(&zone_of_cwd(&repo, &cwd, a.zone.clone())?)?;
    let (x, y) = (repo.resolve_rev(&zone, &a.a)?, repo.resolve_rev(&zone, &a.b)?);
    let mut bases = repo.merge_bases(&x, &y)?;
    if bases.is_empty() {
        return Err(Error::Msg("no common ancestor".into()));
    }
    if !a.all {
        bases.truncate(1);
    }
    let ids: Vec<String> = bases.iter().map(|i| i.to_hex()).collect();
    if json {
        println!(
            "{}",
            doc("merge-base", vec![("bases", J::Arr(ids.into_iter().map(J::Str).collect()))]).render()
        );
        return Ok(());
    }
    write_out(ids.iter().map(|i| format!("{i}\n")).collect())
}

#[derive(Args, Clone)]
pub struct DiffTreeArgs {
    /// Older revision
    pub a: String,
    /// Newer revision
    pub b: String,
    /// Zone (default: the zone containing the current directory)
    #[arg(long)]
    pub zone: Option<String>,
}

/// One line per changed file: `A`, `D` or `M`, a tab, the path (repo-relative, sorted).
pub fn diff_tree(a: &DiffTreeArgs, json: bool) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = repo.resolve_zone(&zone_of_cwd(&repo, &cwd, a.zone.clone())?)?;
    let prefix = repo.zones()?.path_of(&zone).unwrap_or("").to_owned();
    let root = |rev: &str| -> Result<_, Error> { Ok(repo.commit_of(&repo.resolve_rev(&zone, rev)?)?.root) };
    let d = treeops::diff_trees(repo.store(), Some(root(&a.a)?), Some(root(&a.b)?), &prefix)?;
    let mut rows: Vec<(&String, char)> = Vec::new();
    for p in d.old.keys().chain(d.new.keys()) {
        let k = match (d.old.contains_key(p), d.new.contains_key(p)) {
            (true, true) => 'M',
            (true, false) => 'D',
            _ => 'A',
        };
        rows.push((p, k));
    }
    rows.sort();
    rows.dedup();
    if json {
        let items = rows
            .iter()
            .map(|(p, k)| J::Arr(vec![J::Str(k.to_string()), J::Str((*p).clone())]))
            .collect();
        println!("{}", doc("diff-tree", vec![("changes", J::Arr(items))]).render());
        return Ok(());
    }
    write_out(rows.iter().map(|(p, k)| format!("{k}\t{p}\n")).collect())
}
