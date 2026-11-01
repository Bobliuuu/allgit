//! Extra `vcs log` options: date range, message regex, merge filters, first-parent, pickaxe,
//! `--follow`, per-commit file lists with rename detection, and the `--format` template.
//!
//! `--format` placeholders (anything else is printed literally):
//!   %H full commit id      %h short id          %P parent ids      %p short parent ids
//!   %an author name        %ae author email     %at author time (Unix seconds)
//!   %ad author date, `YYYY-MM-DD HH:MM:SS` UTC  %s subject (first message line)
//!   %b body (message after the first line)      %B whole message   %C change id
//!   %z zone label          %n newline           %%  a percent sign
//! Dates for `--since` / `--until` are UTC: `YYYY-MM-DD` (`--until` takes the whole day),
//! `YYYY-MM-DD HH:MM[:SS]` (also with `T` and a trailing `Z`), Unix seconds, `now`, `yesterday`,
//! or `N second|minute|hour|day|week|month|year[s] ago`.
use crate::difftool::{self, DiffArgs};
use clap::Args;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;
use vcs_merge::{detect_renames, StoreBlobs};
use vcs_objects::{Commit, ObjectId};
use vcs_repo::treeops;
use vcs_repo::{Error, Repo};

#[derive(Args, Clone, Debug, Default)]
pub struct LogExtra {
    /// Only commits at or after this date (UTC; see `vcs log --help` placeholders in docs/guide)
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,
    /// Only commits at or before this date (a date without time includes that whole day)
    #[arg(long, value_name = "DATE")]
    pub until: Option<String>,
    /// Only commits whose message matches this regular expression
    #[arg(long, value_name = "REGEX")]
    pub grep: Option<String>,
    /// Only merge commits (two or more parents)
    #[arg(long, conflicts_with = "no_merges")]
    pub merges: bool,
    /// Skip merge commits
    #[arg(long)]
    pub no_merges: bool,
    /// Follow only the first parent of each commit
    #[arg(long)]
    pub first_parent: bool,
    /// Only commits that change the number of occurrences of this string in some file (pickaxe)
    #[arg(short = 'S', value_name = "STRING")]
    pub pickaxe: Option<String>,
    /// Template per commit. Placeholders: %H %h %P %p %an %ae %at %ad %s %b %B %C %z %n %%
    #[arg(long, value_name = "TEMPLATE")]
    pub format: Option<String>,
    /// With one path: the history of that file across renames (first-parent walk)
    #[arg(long)]
    pub follow: bool,
    /// List changed files with a status letter under each commit
    #[arg(long)]
    pub name_status: bool,
    /// List changed file names under each commit
    #[arg(long)]
    pub name_only: bool,
    /// Detect renames in --name-status / --name-only listings
    #[arg(short = 'M', long)]
    pub find_renames: bool,
    /// Detect copies of modified files too
    #[arg(long)]
    pub find_copies: bool,
    /// Minimum similarity in percent for rename and copy detection (also used by --follow)
    #[arg(long, value_name = "PERCENT", default_value_t = 50, value_parser = clap::value_parser!(u32).range(0..=100))]
    pub rename_threshold: u32,
}

impl LogExtra {
    /// Any option that needs the full history before the count is applied.
    pub fn filters(&self) -> bool {
        self.since.is_some()
            || self.until.is_some()
            || self.grep.is_some()
            || self.merges
            || self.no_merges
            || self.first_parent
            || self.pickaxe.is_some()
            || self.follow
    }
    pub fn lists_files(&self) -> bool {
        self.name_status || self.name_only
    }
    fn diff_args(&self) -> DiffArgs {
        DiffArgs {
            find_renames: self.find_renames,
            find_copies: self.find_copies,
            rename_threshold: self.rename_threshold,
            ..Default::default()
        }
    }

    /// Keep only the commits of `shown` that pass the filters. `all` is the whole listed history.
    pub fn filter(
        &self,
        repo: &Repo,
        prefix: &str,
        all: &[(ObjectId, Commit)],
        shown: &mut Vec<(ObjectId, Commit)>,
        paths: &[String],
        cwd: &std::path::Path,
    ) -> Result<(), Error> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let since = self
            .since
            .as_deref()
            .map(|s| parse_date(s, now, false))
            .transpose()
            .map_err(Error::Msg)?;
        let until = self
            .until
            .as_deref()
            .map(|s| parse_date(s, now, true))
            .transpose()
            .map_err(Error::Msg)?;
        let re = self
            .grep
            .as_deref()
            .map(Regex::new)
            .transpose()
            .map_err(|e| Error::Msg(format!("--grep: {e}")))?;
        shown.retain(|(_, c)| {
            since.is_none_or(|s| c.author.timestamp >= s)
                && until.is_none_or(|u| c.author.timestamp <= u)
                && re.as_ref().is_none_or(|r| r.is_match(&c.message))
                && (!self.merges || c.parents.len() > 1)
                && (!self.no_merges || c.parents.len() <= 1)
        });
        if self.follow {
            if paths.len() != 1 {
                return Err(Error::Msg("--follow needs exactly one path (after `--`)".into()));
            }
            let rel = repo.rel_path(cwd, &paths[0])?;
            let keep = follow_set(repo, prefix, all, &rel, self.rename_threshold * 10)?;
            shown.retain(|(id, _)| keep.contains(id));
        }
        if let Some(needle) = &self.pickaxe {
            let by_id: BTreeMap<ObjectId, &Commit> = all.iter().map(|(i, c)| (*i, c)).collect();
            // Commits are independent: test them on all cores, keep the order.
            let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(32);
            let per = shown.len().div_ceil(threads).max(1);
            let by_id = &by_id;
            // occurrences per blob: a blob is the new side of one commit and the old side of the next
            let counts: &Mutex<HashMap<ObjectId, usize>> = &Mutex::new(HashMap::new());
            let hits: Vec<Result<Vec<bool>, Error>> = std::thread::scope(|s| {
                let hs: Vec<_> = shown
                    .chunks(per)
                    .map(|chunk| {
                        s.spawn(move || -> Result<Vec<bool>, Error> {
                            chunk
                                .iter()
                                .map(|(_, c)| pickaxe_hit(repo, prefix, by_id, c, needle.as_bytes(), counts))
                                .collect()
                        })
                    })
                    .collect();
                hs.into_iter().map(|h| h.join().expect("pickaxe worker panicked")).collect()
            });
            let mut flags = Vec::with_capacity(shown.len());
            for h in hits {
                flags.extend(h?);
            }
            let mut it = flags.into_iter();
            shown.retain(|_| it.next().unwrap_or(false));
        }
        Ok(())
    }

    /// File list lines per commit for `--name-status` / `--name-only`.
    pub fn notes(&self, repo: &Repo, zone: &str, shown: &[(ObjectId, Commit)]) -> Result<BTreeMap<ObjectId, Vec<String>>, Error> {
        let mut out = BTreeMap::new();
        if !self.lists_files() {
            return Ok(out);
        }
        let a = self.diff_args();
        for (id, _) in shown {
            // contents matter only for rename / copy detection
            let pairs = if a.detect() { repo.commit_pairs(zone, id)? } else { repo.commit_pairs_names(zone, id)? };
            let es = difftool::entries(pairs, &a);
            let lines = if self.name_status {
                difftool::name_status_lines(&es)
            } else {
                es.iter().map(|e| e.path.clone()).collect()
            };
            out.insert(*id, lines);
        }
        Ok(out)
    }
}

/// Commits reachable from the tip through first parents only (input order kept).
pub fn first_parent_chain(all: &[(ObjectId, Commit)]) -> Vec<(ObjectId, Commit)> {
    let by_id: BTreeMap<ObjectId, &Commit> = all.iter().map(|(i, c)| (*i, c)).collect();
    let Some(tip) = tip_of(all) else { return Vec::new() };
    let mut keep = BTreeSet::new();
    let mut cur = Some(tip);
    while let Some(id) = cur {
        if !keep.insert(id) {
            break;
        }
        cur = by_id
            .get(&id)
            .and_then(|c| c.parents.first().copied())
            .filter(|p| by_id.contains_key(p));
    }
    all.iter().filter(|(i, _)| keep.contains(i)).cloned().collect()
}

/// The commit nothing in the list has as a parent (timestamp ties make list order unreliable).
fn tip_of(all: &[(ObjectId, Commit)]) -> Option<ObjectId> {
    let parents: BTreeSet<ObjectId> = all.iter().flat_map(|(_, c)| c.parents.iter().copied()).collect();
    all.iter().map(|(i, _)| *i).find(|i| !parents.contains(i))
}

/// `p` (repo-relative) as a path inside the zone at `prefix`, if it is inside it.
fn in_zone<'a>(prefix: &str, p: &'a str) -> Option<&'a str> {
    if prefix.is_empty() {
        Some(p)
    } else {
        p.strip_prefix(prefix).and_then(|r| r.strip_prefix('/'))
    }
}

/// The file (not directory) at repo-relative `p` in the tree `root` of the zone at `prefix`.
fn file_at(repo: &Repo, prefix: &str, root: ObjectId, p: &str) -> Result<Option<(vcs_objects::EntryMode, ObjectId)>, Error> {
    Ok(match in_zone(prefix, p) {
        Some(r) if !r.is_empty() => treeops::entry_at(repo.store(), root, r)?.filter(|e| e.0 != vcs_objects::EntryMode::Directory),
        _ => None,
    })
}

/// Commits (first-parent walk from the tip) that touched `path`, following renames backwards.
fn follow_set(repo: &Repo, prefix: &str, all: &[(ObjectId, Commit)], path: &str, th: u32) -> Result<BTreeSet<ObjectId>, Error> {
    let by_id: BTreeMap<ObjectId, &Commit> = all.iter().map(|(i, c)| (*i, c)).collect();
    let mut keep = BTreeSet::new();
    let mut cur = path.to_owned();
    let mut at = tip_of(all);
    // entry of `cur` in the commit being looked at; carried to the next step as the "before" side
    let mut here: Option<Option<(vcs_objects::EntryMode, ObjectId)>> = None;
    let zone = all.first().map(|(_, c)| c.zone.clone()).unwrap_or_default();
    let mut skip = at.map(|t| vcs_repo::FirstParentSkip::new(repo, &zone, t));
    while let Some(mut id) = at {
        // commits that leave the file as it is in their first parent are neither kept nor change the name
        if let (Some(sk), Some(rel)) = (&skip, in_zone(prefix, &cur)) {
            let reached = sk.skip(id, rel);
            if reached != id && by_id.contains_key(&reached) {
                id = reached;
            }
        }
        let c = by_id[&id];
        if let Some(sk) = skip.as_mut() {
            sk.note(&id, c);
        }
        let h = match here.take() {
            Some(h) => h,
            None => file_at(repo, prefix, c.root, &cur)?,
        };
        let parent = c.parents.first().and_then(|p| by_id.get(p).map(|pc| (*p, *pc)));
        let Some((pid, pc)) = parent else {
            if h.is_some() {
                keep.insert(id);
            }
            break;
        };
        let before = file_at(repo, prefix, pc.root, &cur)?;
        if h != before {
            keep.insert(id);
            if h.is_some() && before.is_none() {
                // only the changed paths matter for renames: a path in both trees is neither gone nor new
                let d = treeops::diff_trees_files(repo.store(), Some(pc.root), Some(c.root), prefix)?;
                let rs = detect_renames(&StoreBlobs(repo.store()), &d.old, &d.new, th).map_err(|e| Error::Msg(e.to_string()))?;
                if let Some(r) = rs.iter().find(|r| r.to == cur) {
                    cur = r.from.clone();
                    here = None; // new name: look it up in the parent
                    at = Some(pid);
                    continue;
                }
            }
        }
        here = Some(before);
        at = Some(pid);
    }
    Ok(keep)
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || hay.len() < needle.len() {
        return 0;
    }
    let (mut n, mut i) = (0, 0);
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            n += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    n
}

fn pickaxe_hit(
    repo: &Repo,
    prefix: &str,
    by_id: &BTreeMap<ObjectId, &Commit>,
    c: &Commit,
    needle: &[u8],
    counts: &Mutex<HashMap<ObjectId, usize>>,
) -> Result<bool, Error> {
    let old_root = match c.parents.first().and_then(|p| by_id.get(p)) {
        Some(pc) => Some(pc.root),
        None if c.parents.is_empty() => None,
        None => return Ok(false), // parent outside the listed history
    };
    // only the paths that differ (equal subtrees are not read)
    let d = treeops::diff_trees_files(repo.store(), old_root, Some(c.root), prefix)?;
    let paths: BTreeSet<&String> = d.new.keys().chain(d.old.keys()).collect();
    let occurrences = |e: Option<&(vcs_objects::EntryMode, ObjectId)>| -> Result<usize, Error> {
        let Some((_, id)) = e else { return Ok(0) };
        if let Some(n) = counts.lock().expect("pickaxe cache").get(id) {
            return Ok(*n);
        }
        let n = count(&repo.store().get_content(id)?, needle);
        counts.lock().expect("pickaxe cache").insert(*id, n);
        Ok(n)
    };
    for p in paths {
        let (o, n) = (d.old.get(p), d.new.get(p));
        if o == n {
            continue;
        }
        if occurrences(o)? != occurrences(n)? {
            return Ok(true);
        }
    }
    Ok(false)
}

// ---------- dates ----------

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `YYYY-MM-DD HH:MM:SS` (UTC).
pub fn iso(ts: u64) -> String {
    let (days, rem) = ((ts / 86400) as i64, ts % 86400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Parse a `--since` / `--until` value. `end_of_day` makes a date without time mean its last second.
pub fn parse_date(s: &str, now: u64, end_of_day: bool) -> Result<u64, String> {
    let t = s.trim();
    let bad = || format!("cannot parse date {s:?} (use YYYY-MM-DD[ HH:MM[:SS]], Unix seconds, 'N days ago', now, yesterday)");
    match t {
        "now" => return Ok(now),
        "yesterday" => return Ok(now.saturating_sub(86400)),
        _ => {}
    }
    if t.len() >= 9 && t.bytes().all(|b| b.is_ascii_digit()) {
        return t.parse().map_err(|_| bad());
    }
    if let Some(rest) = t.strip_suffix(" ago") {
        let mut it = rest.split_whitespace();
        let n: u64 = it.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?;
        let unit = it.next().ok_or_else(bad)?.trim_end_matches('s');
        let secs = match unit {
            "second" => 1,
            "minute" => 60,
            "hour" => 3600,
            "day" => 86400,
            "week" => 7 * 86400,
            "month" => 30 * 86400,
            "year" => 365 * 86400,
            _ => return Err(bad()),
        };
        return Ok(now.saturating_sub(n * secs));
    }
    let t = t.trim_end_matches('Z');
    let (date, time) = match t.split_once(['T', ' ']) {
        Some((d, tm)) => (d, Some(tm)),
        None => (t, None),
    };
    let mut dp = date.split('-');
    let (y, m, d): (i64, i64, i64) = (
        dp.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?,
        dp.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?,
        dp.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?,
    );
    if dp.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) || y < 1970 {
        return Err(bad());
    }
    let secs = match time {
        Some(tm) => {
            let mut p = tm.split(':');
            let h: i64 = p.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?;
            let mi: i64 = p.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?;
            let se: i64 = p.next().map_or(Ok(0), |x| x.parse().map_err(|_| bad()))?;
            if h > 23 || mi > 59 || se > 60 {
                return Err(bad());
            }
            h * 3600 + mi * 60 + se
        }
        None if end_of_day => 86399,
        None => 0,
    };
    Ok((days_from_civil(y, m, d) * 86400 + secs) as u64)
}

// ---------- format ----------

/// Expand a `--format` template for one commit.
pub fn expand(fmt: &str, id: &ObjectId, c: &Commit, zone_label: &str) -> String {
    let mut out = String::new();
    let mut it = fmt.chars().peekable();
    let (subject, body) = match c.message.split_once('\n') {
        Some((s, b)) => (s, b.trim_start_matches('\n')),
        None => (c.message.as_str(), ""),
    };
    while let Some(ch) = it.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match it.next() {
            Some('H') => out.push_str(&id.to_string()),
            Some('h') => out.push_str(&id.short()),
            Some('P') => out.push_str(&c.parents.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ")),
            Some('p') => out.push_str(&c.parents.iter().map(ObjectId::short).collect::<Vec<_>>().join(" ")),
            Some('s') => out.push_str(subject),
            Some('b') => out.push_str(body.trim_end()),
            Some('B') => out.push_str(c.message.trim_end()),
            Some('C') => out.push_str(&c.change_id.to_string()),
            Some('z') => out.push_str(zone_label),
            Some('n') => out.push('\n'),
            Some('%') => out.push('%'),
            Some('a') => match it.peek().copied() {
                Some('n') => {
                    it.next();
                    out.push_str(&c.author.name);
                }
                Some('e') => {
                    it.next();
                    out.push_str(&c.author.email);
                }
                Some('t') => {
                    it.next();
                    out.push_str(&c.author.timestamp.to_string());
                }
                Some('d') => {
                    it.next();
                    out.push_str(&iso(c.author.timestamp));
                }
                _ => out.push_str("%a"),
            },
            Some(o) => {
                out.push('%');
                out.push(o);
            }
            None => out.push('%'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(parse_date("1970-01-02", 0, false), Ok(86400));
        assert_eq!(parse_date("1970-01-02", 0, true), Ok(86400 + 86399));
        assert_eq!(parse_date("2024-02-29 12:30:15", 0, false), Ok(1709209815));
        assert_eq!(parse_date("2024-02-29T12:30:15Z", 0, false), Ok(1709209815));
        assert_eq!(parse_date("1709209815", 0, false), Ok(1709209815));
        assert_eq!(parse_date("2 days ago", 1_000_000, false), Ok(1_000_000 - 172800));
        assert!(parse_date("garbage", 0, false).is_err());
        assert!(parse_date("2024-13-01", 0, false).is_err());
        assert_eq!(iso(1709209815), "2024-02-29 12:30:15");
    }

    #[test]
    fn counting() {
        assert_eq!(count(b"aaaa", b"aa"), 2);
        assert_eq!(count(b"abc", b""), 0);
    }
}
