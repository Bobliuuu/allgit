//! `vcs log` rendering: full / oneline formats, ASCII graph, author and path filters, colour.
use clap::ValueEnum;
use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::path::Path;
use vcs_objects::{Commit, ObjectId};
use vcs_repo::treeops::{self, Flat};
use vcs_repo::{Error, Repo};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorWhen {
    Auto,
    Always,
    Never,
}

impl ColorWhen {
    /// `auto` colours only a terminal stdout and never when `NO_COLOR` is set and non-empty.
    pub fn enabled(self) -> bool {
        match self {
            ColorWhen::Always => true,
            ColorWhen::Never => false,
            ColorWhen::Auto => !std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) && std::io::stdout().is_terminal(),
        }
    }
}

pub struct Opts {
    pub oneline: bool,
    pub graph: bool,
    pub author: Option<String>,
    pub color: bool,
    pub paths: Vec<String>,
    pub x: crate::logopts::LogExtra,
    /// Set by `--show-signature`: verify each commit against these trust settings.
    pub trust: Option<vcs_objects::signing::Trust>,
}

/// The commits `vcs log` would show (filters applied, count applied). The flag is true when the
/// branch has no commits at all.
pub fn select(
    repo: &Repo,
    cwd: &Path,
    zone: &str,
    branch: Option<&str>,
    limit: usize,
    o: &Opts,
) -> Result<(Vec<(ObjectId, Commit)>, bool), Error> {
    let filtering = o.author.is_some() || !o.paths.is_empty() || o.x.filters();
    // Filters need the whole history; the count applies after filtering.
    let mut all = repo.log(zone, branch, if filtering { usize::MAX } else { limit })?;
    let defs = repo.zones()?;
    if all.is_empty() {
        return Ok((Vec::new(), true));
    }
    if o.x.first_parent {
        all = crate::logopts::first_parent_chain(&all);
    }
    let mut shown: Vec<(ObjectId, Commit)> = all.clone();
    if let Some(a) = &o.author {
        shown.retain(|(_, c)| author_matches(c, a));
    }
    if !o.paths.is_empty() && !o.x.follow {
        let mut specs = Vec::new();
        for p in &o.paths {
            specs.push(repo.rel_path(cwd, p)?);
        }
        let prefix = defs.path_of(zone).unwrap_or("").to_owned();
        let by_id: BTreeMap<ObjectId, &Commit> = all.iter().map(|(i, c)| (*i, c)).collect();
        let mut cache: BTreeMap<ObjectId, Flat> = BTreeMap::new();
        let mut keep = Vec::new();
        for (id, c) in &shown {
            if touches(repo, &prefix, &specs, &by_id, &mut cache, *id, c)? {
                keep.push((*id, c.clone()));
            }
        }
        shown = keep;
    }
    if o.x.filters() {
        let prefix = defs.path_of(zone).unwrap_or("").to_owned();
        o.x.filter(repo, &prefix, &all, &mut shown, &o.paths, cwd)?;
    }
    shown.truncate(limit);
    Ok((shown, false))
}

/// Collect, filter and render the log of `zone`.
pub fn run(repo: &Repo, cwd: &Path, zone: &str, branch: Option<&str>, limit: usize, o: &Opts) -> Result<String, Error> {
    let defs = repo.zones()?;
    let (shown, empty) = select(repo, cwd, zone, branch, limit, o)?;
    if empty {
        return Ok(format!("zone {}: no commits yet\n", defs.label(zone)));
    }
    if shown.is_empty() {
        return Ok("no matching commits\n".to_string());
    }
    let notes = o.x.notes(repo, zone, &shown)?;
    Ok(render_with(&shown, |z| defs.label(z).to_string(), o, &notes))
}

pub fn author_matches(c: &Commit, needle: &str) -> bool {
    let n = needle.to_lowercase();
    c.author.name.to_lowercase().contains(&n) || c.author.email.to_lowercase().contains(&n)
}

fn in_spec(key: &str, specs: &[String]) -> bool {
    specs
        .iter()
        .any(|s| s.is_empty() || key == s || key.strip_prefix(s.as_str()).is_some_and(|r| r.starts_with('/')))
}

fn filtered(repo: &Repo, prefix: &str, specs: &[String], root: ObjectId) -> Result<Flat, Error> {
    let mut f = treeops::flatten(repo.store(), Some(root), prefix)?;
    f.retain(|k, _| in_spec(k, specs));
    Ok(f)
}

/// A commit touches the paths when its filtered tree differs from every parent's
/// (a root commit touches them if it contains any).
fn touches(
    repo: &Repo,
    prefix: &str,
    specs: &[String],
    by_id: &BTreeMap<ObjectId, &Commit>,
    cache: &mut BTreeMap<ObjectId, Flat>,
    id: ObjectId,
    c: &Commit,
) -> Result<bool, Error> {
    let mine = flat_of(repo, prefix, specs, cache, id, c)?;
    if c.parents.is_empty() {
        return Ok(!mine.is_empty());
    }
    for p in &c.parents {
        let Some(pc) = by_id.get(p) else { continue };
        if flat_of(repo, prefix, specs, cache, *p, pc)? == mine {
            return Ok(false);
        }
    }
    Ok(true)
}

fn flat_of(
    repo: &Repo,
    prefix: &str,
    specs: &[String],
    cache: &mut BTreeMap<ObjectId, Flat>,
    id: ObjectId,
    c: &Commit,
) -> Result<Flat, Error> {
    if let Some(f) = cache.get(&id) {
        return Ok(f.clone());
    }
    let f = filtered(repo, prefix, specs, c.root)?;
    cache.insert(id, f.clone());
    Ok(f)
}

const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const RESET: &str = "\x1b[0m";

fn paint(on: bool, code: &str, s: &str) -> String {
    if on {
        format!("{code}{s}{RESET}")
    } else {
        s.to_string()
    }
}

/// Render `commits` (newest first). Pure; the graph follows only parents present in the slice.
/// Lines in `notes` (file lists) are printed under their commit.
pub fn render_with(
    commits: &[(ObjectId, Commit)],
    label: impl Fn(&str) -> String,
    o: &Opts,
    notes: &BTreeMap<ObjectId, Vec<String>>,
) -> String {
    let ordered;
    let commits = if o.graph {
        ordered = children_first(commits);
        &ordered[..]
    } else {
        commits
    };
    let present: BTreeSet<ObjectId> = commits.iter().map(|(i, _)| *i).collect();
    let mut lanes: Vec<Option<ObjectId>> = Vec::new();
    let mut out = String::new();
    for (id, c) in commits {
        let mut body = body_lines(id, c, &label, o);
        if let Some(n) = notes.get(id).filter(|n| !n.is_empty()) {
            // keep the blank separator after the message, then the list, then another blank
            let blank = body.last().is_some_and(String::is_empty);
            if !blank && !o.graph && !o.oneline && o.x.format.is_none() {
                body.push(String::new());
            }
            body.extend(n.iter().cloned());
            if !o.oneline && o.x.format.is_none() {
                body.push(String::new());
            }
        }
        if !o.graph {
            for l in body {
                out.push_str(&l);
                out.push('\n');
            }
            continue;
        }
        // Lane of this commit: first lane expecting it, else a free/new lane.
        let col = match lanes.iter().position(|l| *l == Some(*id)) {
            Some(i) => i,
            None => place(&mut lanes, *id),
        };
        let converging: Vec<usize> = (0..lanes.len()).filter(|&i| i != col && lanes[i] == Some(*id)).collect();
        for (n, l) in body.iter().enumerate() {
            let mut prefix = String::new();
            for (i, lane) in lanes.iter().enumerate() {
                let cell = if n == 0 && i == col {
                    "*"
                } else if lane.is_some() {
                    "|"
                } else {
                    " "
                };
                prefix.push_str(&paint(o.color, CYAN, cell));
                prefix.push(' ');
            }
            out.push_str(format!("{prefix}{l}").trim_end());
            out.push('\n');
        }
        // Advance lanes to the parents.
        let parents: Vec<ObjectId> = c.parents.iter().copied().filter(|p| present.contains(p)).collect();
        let mut merged_into_other = false;
        for &i in &converging {
            lanes[i] = None;
        }
        let mut new_cols = Vec::new();
        match parents.first() {
            None => lanes[col] = None,
            Some(p) => {
                if lanes.iter().enumerate().any(|(i, l)| i != col && *l == Some(*p)) {
                    lanes[col] = None;
                    merged_into_other = true;
                } else {
                    lanes[col] = Some(*p);
                }
            }
        }
        for p in parents.iter().skip(1) {
            if !lanes.contains(&Some(*p)) {
                new_cols.push(place(&mut lanes, *p));
            }
        }
        if !converging.is_empty() || merged_into_other || !new_cols.is_empty() {
            let width = lanes.len().max(col + 1);
            let mut line = String::new();
            for i in 0..width {
                let cell = if converging.contains(&i) || (merged_into_other && i == col) {
                    '/'
                } else if new_cols.contains(&i) {
                    '\\'
                } else if lanes.get(i).copied().flatten().is_some() {
                    '|'
                } else {
                    ' '
                };
                line.push_str(&paint(o.color, CYAN, &cell.to_string()));
                line.push(' ');
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        while matches!(lanes.last(), Some(None)) {
            lanes.pop();
        }
    }
    out
}

/// Stable topological order (children before parents, otherwise the given order); timestamp
/// ties in `Repo::log` can list a parent before its child, which a graph cannot draw.
fn children_first(commits: &[(ObjectId, Commit)]) -> Vec<(ObjectId, Commit)> {
    use std::cmp::Reverse;
    let idx: BTreeMap<ObjectId, usize> = commits.iter().enumerate().map(|(i, (id, _))| (*id, i)).collect();
    let mut pending = vec![0usize; commits.len()];
    for (_, c) in commits {
        for p in &c.parents {
            if let Some(&j) = idx.get(p) {
                pending[j] += 1;
            }
        }
    }
    let mut ready: std::collections::BinaryHeap<Reverse<usize>> = (0..commits.len()).filter(|&i| pending[i] == 0).map(Reverse).collect();
    let mut out = Vec::with_capacity(commits.len());
    while let Some(Reverse(i)) = ready.pop() {
        out.push(commits[i].clone());
        for p in &commits[i].1.parents {
            if let Some(&j) = idx.get(p) {
                pending[j] -= 1;
                if pending[j] == 0 {
                    ready.push(Reverse(j));
                }
            }
        }
    }
    out
}

fn place(lanes: &mut Vec<Option<ObjectId>>, id: ObjectId) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(i) => {
            lanes[i] = Some(id);
            i
        }
        None => {
            lanes.push(Some(id));
            lanes.len() - 1
        }
    }
}

fn body_lines(id: &ObjectId, c: &Commit, label: &impl Fn(&str) -> String, o: &Opts) -> Vec<String> {
    if let Some(f) = &o.x.format {
        return crate::logopts::expand(f, id, c, &label(&c.zone))
            .split('\n')
            .map(str::to_owned)
            .collect();
    }
    if o.oneline {
        let subject = c.message.lines().next().unwrap_or("");
        return vec![format!("{} {}", paint(o.color, YELLOW, &id.short()), subject)];
    }
    let mut v = vec![
        format!("commit {}", paint(o.color, YELLOW, &id.to_string())),
        format!("change {}   zone {}", c.change_id, paint(o.color, GREEN, &label(&c.zone))),
        format!("Author: {} <{}>", c.author.name, c.author.email),
        format!("Date:   {}", c.author.timestamp),
    ];
    if let Some(t) = &o.trust {
        v.push(format!(
            "Signature: {}",
            crate::signing_cmd::describe(&vcs_objects::signing::verify_commit(c, t))
        ));
    }
    for l in vcs_objects::signing::strip_commit_signature(&c.message).lines() {
        v.push(format!("    {l}"));
    }
    v.push(String::new());
    v
}
