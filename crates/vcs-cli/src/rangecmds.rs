//! `range-diff` (compare two commit ranges by patch) and `show-branch` (which commits each
//! revision has). The engines are in `vcs_repo::rangediff` and `vcs_repo::showbranch`.

use super::{open, zone_of_cwd};
use clap::Args;
use std::io::Write;
use vcs_repo::rangediff::{PairKind, CREATION_FACTOR};
use vcs_repo::Error;

fn write_out(text: String) -> Result<(), Error> {
    std::io::stdout().lock().write_all(text.as_bytes()).map_err(|source| Error::Io {
        path: "stdout".into(),
        source,
    })
}

#[derive(Args, Clone)]
pub struct RangeDiffArgs {
    /// `OLD_BASE..OLD_TIP NEW_BASE..NEW_TIP`, or `BASE OLD_TIP NEW_TIP`, or `A...B` (= `B..A A..B`)
    #[arg(required = true, num_args = 1..=3)]
    pub ranges: Vec<String>,
    /// How much a commit that has no partner is penalised, in percent of its patch size (higher pairs more commits)
    #[arg(long, default_value_t = CREATION_FACTOR)]
    pub creation_factor: u32,
    /// List the pairs only, without the diff between the two patches
    #[arg(short = 's', long = "no-patch")]
    pub no_patch: bool,
    /// Zone (default: the zone containing the current directory)
    #[arg(long)]
    pub zone: Option<String>,
}

pub fn range_diff(a: &RangeDiffArgs) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = repo.resolve_zone(&zone_of_cwd(&repo, &cwd, a.zone.clone())?)?;
    let pairs = repo.range_diff_args(&zone, &a.ranges, a.creation_factor)?;
    let width = pairs
        .iter()
        .flat_map(|p| p.old.iter().chain(p.new.iter()))
        .map(|(n, _)| n.to_string().len())
        .max()
        .unwrap_or(1);
    let side = |x: &Option<(usize, vcs_objects::ObjectId)>| match x {
        Some((n, id)) => format!("{n:>width$}:  {}", &id.to_hex()[..7]),
        None => format!("{:>width$}:  {}", "-", "-------"),
    };
    let mut out = String::new();
    for p in &pairs {
        out.push_str(&format!("{} {} {} {}\n", side(&p.old), p.kind.sign(), side(&p.new), p.subject));
        if p.kind == PairKind::Changed && !a.no_patch {
            for l in p.inter.lines() {
                out.push_str(&format!("    {l}\n"));
            }
        }
    }
    write_out(out)
}

#[derive(Args, Clone)]
pub struct ShowBranchArgs {
    /// Revisions to compare (default: every local branch of the zone)
    pub revs: Vec<String>,
    /// Show the heads only, no commit rows
    #[arg(long)]
    pub list: bool,
    /// Go N commits past the first common ancestor
    #[arg(long, value_name = "N")]
    pub more: Option<usize>,
    /// Print the best common ancestor(s) of the revisions instead of the matrix
    #[arg(long, conflicts_with = "independent")]
    pub merge_base: bool,
    /// Print the revisions that are not an ancestor of another one
    #[arg(long)]
    pub independent: bool,
    /// Also include the checked-out branch
    #[arg(long)]
    pub current: bool,
    /// Name commits by short id instead of `branch~N`
    #[arg(long)]
    pub sha1_name: bool,
    /// Leave the `[name]` out of the rows
    #[arg(long)]
    pub no_name: bool,
    /// Zone (default: the zone containing the current directory)
    #[arg(long)]
    pub zone: Option<String>,
}

pub fn show_branch(a: &ShowBranchArgs) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = repo.resolve_zone(&zone_of_cwd(&repo, &cwd, a.zone.clone())?)?;
    let cur = repo.current_branch(&zone)?;
    let mut revs = a.revs.clone();
    if revs.is_empty() {
        revs = repo.show_branch_default_revs(&zone)?;
    }
    if a.current {
        if let Some(c) = &cur {
            if !revs.contains(c) {
                revs.push(c.clone());
            }
        }
    }
    if revs.is_empty() {
        return Err(Error::Msg("show-branch: no branches".into()));
    }
    let heads = repo.show_branch_heads(&zone, &revs)?;
    if a.independent {
        return write_out(repo.show_branch_independent(&heads)?.iter().map(|i| format!("{}\n", i.to_hex())).collect());
    }
    let n = heads.len();
    let cur_idx = cur.as_ref().and_then(|c| heads.iter().position(|h| &h.name == c));
    if a.list {
        let mut out = String::new();
        for (i, h) in heads.iter().enumerate() {
            out.push_str(&format!("{} [{}] {}\n", if Some(i) == cur_idx { '*' } else { ' ' }, h.name, h.subject));
        }
        return write_out(out);
    }
    let sb = repo.show_branch(heads, a.more.unwrap_or(0))?;
    if a.merge_base {
        if sb.merge_bases.is_empty() {
            std::process::exit(1);
        }
        return write_out(sb.merge_bases.iter().map(|i| format!("{}\n", i.to_hex())).collect());
    }
    let mut out = String::new();
    if n > 1 {
        for (i, h) in sb.heads.iter().enumerate() {
            out.push_str(&format!(
                "{}{} [{}] {}\n",
                " ".repeat(i),
                if Some(i) == cur_idx { '*' } else { '!' },
                h.name,
                h.subject
            ));
        }
        out.push_str(&"-".repeat(n));
        out.push('\n');
    }
    for r in &sb.rows {
        if n > 1 {
            for i in 0..n {
                out.push(if r.mask & (1 << i) == 0 {
                    ' '
                } else if r.is_merge {
                    '-'
                } else if Some(i) == cur_idx {
                    '*'
                } else {
                    '+'
                });
            }
            out.push(' ');
        }
        if !a.no_name {
            let name = if a.sha1_name { r.id.short() } else { r.name.clone() };
            out.push_str(&format!("[{name}] "));
        }
        out.push_str(&r.subject);
        out.push('\n');
    }
    write_out(out)
}
