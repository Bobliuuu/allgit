//! `range-diff`: compare two commit ranges by the patches they contain (git semantics).
//!
//! Each range is its non-merge commits, oldest first. A commit is turned into a patch text:
//! author, message, and per file the diff with the `@@` line numbers removed. Commits of the two
//! ranges whose diffs hash equal are paired first. The rest are paired by a minimum-cost
//! assignment (Hungarian algorithm) where pairing two commits costs the number of lines their
//! patch texts differ by, and leaving a commit unpaired costs its patch size times the creation
//! factor (default 60 %). Paired commits are `=` when the whole patch text is identical, `!`
//! when it changed; unpaired ones are `<` (only in the old range) or `>` (only in the new).

use crate::diff;
use crate::error::{msg, Result};
use crate::repo::Repo;
use crate::revrange::RevSpec;
use crate::treeops;
use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use vcs_objects::ObjectId;
use vcs_store::ObjectStore;

/// Default `--creation-factor` (percent).
pub const CREATION_FACTOR: u32 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairKind {
    /// `=`: same patch and message.
    Same,
    /// `!`: matched but different.
    Changed,
    /// `<`: only in the old range.
    Dropped,
    /// `>`: only in the new range.
    Added,
}

impl PairKind {
    pub fn sign(self) -> char {
        match self {
            PairKind::Same => '=',
            PairKind::Changed => '!',
            PairKind::Dropped => '<',
            PairKind::Added => '>',
        }
    }
}

/// One output line of `range-diff`.
#[derive(Clone, Debug)]
pub struct RangePair {
    pub kind: PairKind,
    /// 1-based position and id in the old range.
    pub old: Option<(usize, ObjectId)>,
    /// 1-based position and id in the new range.
    pub new: Option<(usize, ObjectId)>,
    /// Subject of the new commit (the old one for `<`).
    pub subject: String,
    /// For `!`: the diff between the two patch texts, as unified-diff body lines.
    pub inter: String,
}

struct Patch {
    id: ObjectId,
    subject: String,
    /// Full text: metadata, message, per-file diffs.
    text: String,
    /// Hash of the diff part only (the exact-match key).
    diff_hash: u64,
    size: usize,
}

impl Repo {
    /// The non-merge commits of the range arguments, oldest first.
    /// `[A..B]`, `[A...B]` (as `B..A` / `A..B`, see [`Repo::range_diff_args`]) or any rev-list spec.
    fn range_commits(&self, zone: &str, spec: &RevSpec) -> Result<Vec<ObjectId>> {
        let mut ids = self.rev_list_ids(zone, spec, usize::MAX)?;
        ids.reverse();
        let mut out = Vec::new();
        for id in ids {
            if self.commit_obj(&id)?.parents.len() <= 1 {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Turn `range-diff` arguments into the old and the new commit list:
    /// `A..B C..D`, `A...B` (= `B..A A..B`) or `BASE OLD NEW` (= `BASE..OLD BASE..NEW`).
    pub fn range_diff_args(&self, zone: &str, args: &[String], creation_factor: u32) -> Result<Vec<RangePair>> {
        let zone = self.resolve_zone(zone)?;
        let spec = |s: &str| RevSpec::parse(&[s]);
        let (old, new) = match args {
            [one] if one.contains("...") => {
                let (a, b) = one.split_once("...").unwrap();
                let (a, b) = (if a.is_empty() { "HEAD" } else { a }, if b.is_empty() { "HEAD" } else { b });
                (format!("{b}..{a}"), format!("{a}..{b}"))
            }
            [a, b] if a.contains("..") && b.contains("..") && !a.contains("...") && !b.contains("...") => (a.clone(), b.clone()),
            [base, o, n] if ![base, o, n].iter().any(|x| x.contains("..")) => (format!("{base}..{o}"), format!("{base}..{n}")),
            _ => return msg("usage: range-diff <old-base>..<old-tip> <new-base>..<new-tip> | <base> <old-tip> <new-tip> | <A>...<B>"),
        };
        let (o, n) = (self.range_commits(&zone, &spec(&old)?)?, self.range_commits(&zone, &spec(&new)?)?);
        self.range_diff_ids(&zone, &o, &n, creation_factor)
    }

    fn patch_of(&self, zone: &str, id: &ObjectId) -> Result<Patch> {
        let c = self.commit_obj(id)?;
        let prefix = self.zones()?.path_of(zone).unwrap_or("").to_owned();
        let parent_root = match c.parents.first() {
            Some(p) => Some(self.commit_obj(p)?.root),
            None => None,
        };
        let d = treeops::diff_trees_files(&self.store, parent_root, Some(c.root), &prefix)?;
        let mut paths: BTreeSet<&String> = d.old.keys().collect();
        paths.extend(d.new.keys());
        let mut body = String::new();
        for p in paths {
            let (o, n) = (d.old.get(p), d.new.get(p));
            let ob = o.map(|(_, i)| self.store.get_content(i)).transpose()?;
            let nb = n.map(|(_, i)| self.store.get_content(i)).transpose()?;
            let u = diff::unified(p, ob.as_deref(), nb.as_deref());
            if u.is_empty() {
                continue; // mode-only changes are not part of the patch text
            }
            let tag = match (o.is_some(), n.is_some()) {
                (false, _) => " (new)",
                (_, false) => " (deleted)",
                _ => "",
            };
            body.push_str(&format!(" ## {p}{tag} ##\n"));
            for line in u.lines() {
                if line.starts_with("diff --vcs ")
                    || line.starts_with("--- ")
                    || line.starts_with("+++ ")
                    || line == "new file"
                    || line == "deleted file"
                {
                    continue;
                }
                if line.starts_with("@@ ") {
                    body.push_str("@@\n"); // line numbers differ between bases
                } else {
                    body.push_str(line);
                    body.push('\n');
                }
            }
        }
        let mut text = format!(
            " ## Metadata ##\nAuthor: {} <{}>\n\n ## Commit message ##\n",
            c.author.name, c.author.email
        );
        for l in c.message.trim_end().lines() {
            text.push_str(&format!("    {l}\n"));
        }
        text.push('\n');
        text.push_str(&body);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        body.hash(&mut h);
        Ok(Patch {
            id: *id,
            subject: c.message.lines().next().unwrap_or("").to_owned(),
            size: text.lines().count(),
            text,
            diff_hash: h.finish(),
        })
    }

    /// Pair the commits of two lists (oldest first) and describe each pair.
    pub fn range_diff_ids(&self, zone: &str, old: &[ObjectId], new: &[ObjectId], creation_factor: u32) -> Result<Vec<RangePair>> {
        let zone = self.resolve_zone(zone)?;
        let a: Vec<Patch> = old.iter().map(|i| self.patch_of(&zone, i)).collect::<Result<_>>()?;
        let b: Vec<Patch> = new.iter().map(|i| self.patch_of(&zone, i)).collect::<Result<_>>()?;
        let (an, bn) = (a.len(), b.len());
        let mut a_to_b: Vec<Option<usize>> = vec![None; an];
        let mut b_to_a: Vec<Option<usize>> = vec![None; bn];
        // 1. identical diffs (first unmatched wins)
        let mut by_hash: HashMap<u64, Vec<usize>> = HashMap::new();
        for (j, p) in b.iter().enumerate() {
            by_hash.entry(p.diff_hash).or_default().push(j);
        }
        for (i, p) in a.iter().enumerate() {
            if let Some(js) = by_hash.get(&p.diff_hash) {
                if let Some(&j) = js.iter().find(|j| b_to_a[**j].is_none()) {
                    a_to_b[i] = Some(j);
                    b_to_a[j] = Some(i);
                }
            }
        }
        // 2. the rest by minimum-cost assignment
        let ra: Vec<usize> = (0..an).filter(|i| a_to_b[*i].is_none()).collect();
        let rb: Vec<usize> = (0..bn).filter(|j| b_to_a[*j].is_none()).collect();
        if !ra.is_empty() && !rb.is_empty() {
            let n = ra.len() + rb.len();
            let mut cost = vec![vec![0i64; n]; n];
            for (x, &i) in ra.iter().enumerate() {
                for (y, &j) in rb.iter().enumerate() {
                    cost[x][y] = diff_size(&a[i].text, &b[j].text) as i64;
                }
                for c in &mut cost[x][rb.len()..] {
                    *c = (a[i].size as i64 * creation_factor as i64) / 100;
                }
            }
            for (y, &j) in rb.iter().enumerate() {
                for row in &mut cost[ra.len()..] {
                    row[y] = (b[j].size as i64 * creation_factor as i64) / 100;
                }
            }
            let assign = hungarian(&cost);
            for (x, &y) in assign.iter().enumerate() {
                if x < ra.len() && y < rb.len() {
                    a_to_b[ra[x]] = Some(rb[y]);
                    b_to_a[rb[y]] = Some(ra[x]);
                }
            }
        }
        // 3. output in git's order: old commits in order, new-only commits where they come up
        let mut out = Vec::new();
        let mut shown_b = vec![false; bn];
        let (mut i, mut j) = (0, 0);
        let pair = |i: usize, j: usize| -> RangePair {
            if a[i].text == b[j].text {
                RangePair {
                    kind: PairKind::Same,
                    old: Some((i + 1, a[i].id)),
                    new: Some((j + 1, b[j].id)),
                    subject: b[j].subject.clone(),
                    inter: String::new(),
                }
            } else {
                RangePair {
                    kind: PairKind::Changed,
                    old: Some((i + 1, a[i].id)),
                    new: Some((j + 1, b[j].id)),
                    subject: b[j].subject.clone(),
                    inter: inter_diff(&a[i].text, &b[j].text),
                }
            }
        };
        while i < an || j < bn {
            while j < bn && shown_b[j] {
                j += 1;
            }
            if i < an && a_to_b[i].is_none() {
                out.push(RangePair {
                    kind: PairKind::Dropped,
                    old: Some((i + 1, a[i].id)),
                    new: None,
                    subject: a[i].subject.clone(),
                    inter: String::new(),
                });
                i += 1;
                continue;
            }
            while j < bn && b_to_a[j].is_none() {
                out.push(RangePair {
                    kind: PairKind::Added,
                    old: None,
                    new: Some((j + 1, b[j].id)),
                    subject: b[j].subject.clone(),
                    inter: String::new(),
                });
                shown_b[j] = true;
                j += 1;
            }
            if i < an {
                let jj = a_to_b[i].unwrap();
                out.push(pair(i, jj));
                shown_b[jj] = true;
                i += 1;
            }
        }
        Ok(out)
    }
}

/// Lines that differ between two texts (additions plus deletions).
fn diff_size(a: &str, b: &str) -> usize {
    diff::unified("p", Some(a.as_bytes()), Some(b.as_bytes()))
        .lines()
        .filter(|l| (l.starts_with('+') && !l.starts_with("+++")) || (l.starts_with('-') && !l.starts_with("---")))
        .count()
}

/// Unified diff of two patch texts as body lines, hunk headers named by the section they are in
/// (`@@ Commit message`, `@@ src/a.rs`).
fn inter_diff(a: &str, b: &str) -> String {
    let u = diff::unified("p", Some(a.as_bytes()), Some(b.as_bytes()));
    let a_lines: Vec<&str> = a.lines().collect();
    let section_at = |upto: usize| {
        a_lines[..upto.min(a_lines.len())]
            .iter()
            .rev()
            .find_map(|l| l.strip_prefix(" ## ").and_then(|s| s.strip_suffix(" ##")))
    };
    let mut out = String::new();
    let mut hunk: Option<(usize, Vec<&str>)> = None;
    let flush = |out: &mut String, hunk: &mut Option<(usize, Vec<&str>)>| {
        if let Some((start, lines)) = hunk.take() {
            // name the hunk after the section its first change is in
            let lead = lines.iter().take_while(|l| l.starts_with(' ')).count();
            match section_at(start.saturating_sub(1) + lead) {
                Some(s) => out.push_str(&format!("@@ {s}\n")),
                None => out.push_str("@@\n"),
            }
            for l in lines {
                out.push_str(l);
                out.push('\n');
            }
        }
    };
    for line in u.lines() {
        if line.starts_with("diff --vcs ") || line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        if let Some(h) = line.strip_prefix("@@ -") {
            flush(&mut out, &mut hunk);
            let start: usize = h.split([',', ' ']).next().and_then(|s| s.parse().ok()).unwrap_or(1);
            hunk = Some((start, Vec::new()));
        } else if let Some((_, lines)) = hunk.as_mut() {
            lines.push(line);
        }
    }
    flush(&mut out, &mut hunk);
    out
}

/// Minimum-cost perfect matching of a square cost matrix; returns, per row, its column.
fn hungarian(c: &[Vec<i64>]) -> Vec<usize> {
    let n = c.len();
    let inf = i64::MAX / 4;
    let (mut u, mut v) = (vec![0i64; n + 1], vec![0i64; n + 1]);
    let (mut p, mut way) = (vec![0usize; n + 1], vec![0usize; n + 1]);
    for i in 1..=n {
        p[0] = i;
        let mut j0 = 0;
        let mut minv = vec![inf; n + 1];
        let mut used = vec![false; n + 1];
        loop {
            used[j0] = true;
            let (i0, mut delta, mut j1) = (p[j0], inf, 0);
            for j in 1..=n {
                if !used[j] {
                    let cur = c[i0 - 1][j - 1] - u[i0] - v[j];
                    if cur < minv[j] {
                        minv[j] = cur;
                        way[j] = j0;
                    }
                    if minv[j] < delta {
                        delta = minv[j];
                        j1 = j;
                    }
                }
            }
            for j in 0..=n {
                if used[j] {
                    u[p[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if p[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            p[j0] = p[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    let mut ans = vec![0usize; n];
    for j in 1..=n {
        ans[p[j] - 1] = j - 1;
    }
    ans
}

#[cfg(test)]
mod tests {
    use super::hungarian;

    #[test]
    fn hungarian_finds_the_cheapest_assignment() {
        let c = vec![vec![4, 1, 3], vec![2, 0, 5], vec![3, 2, 2]];
        let a = hungarian(&c);
        let total: i64 = a.iter().enumerate().map(|(i, j)| c[i][*j]).sum();
        assert_eq!(total, 5); // 1 + 2 + 2
    }
}
