//! `vcs diff` / `vcs show` rendering with options: stat, name-only, name-status, word diff,
//! colour (and moved-line colour), diff algorithm, context size, whitespace-insensitive compare,
//! binary summaries and rename / copy detection.
//!
//! Input is a list of [`FilePair`]s (old and new bytes per path, chunked files already
//! reassembled by `get_content`). Without any of the new options the original text from
//! `Repo::diff` / `Repo::show` is printed unchanged.
use clap::{Args, ValueEnum};
use std::collections::{BTreeSet, HashMap};
use std::io::IsTerminal;
use vcs_merge::diff::{matches, Algorithm};
use vcs_repo::diff::is_binary;
use vcs_repo::{Error, FilePair, Repo};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum, Debug)]
pub enum AlgArg {
    Myers,
    Patience,
    Histogram,
}

impl AlgArg {
    pub fn alg(self) -> Algorithm {
        match self {
            AlgArg::Myers => Algorithm::Myers,
            AlgArg::Patience => Algorithm::Patience,
            AlgArg::Histogram => Algorithm::Histogram,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum, Debug)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

/// Options shared by `diff` and `show`.
#[derive(Args, Clone, Debug, Default)]
pub struct DiffArgs {
    /// Per-file added/deleted line counts instead of the patch
    #[arg(long)]
    pub stat: bool,
    /// Only the names of changed files
    #[arg(long)]
    pub name_only: bool,
    /// Names with a status letter: A added, D deleted, M modified, R<score> renamed, C<score> copied
    #[arg(long)]
    pub name_status: bool,
    /// Show changed words inline as [-old-]{+new+} instead of changed lines
    #[arg(long)]
    pub word_diff: bool,
    /// Colour the output: auto (terminal and no NO_COLOR), always, never. Bare `--color` forces colour
    /// unless NO_COLOR is set; `--color=always` overrides NO_COLOR.
    #[arg(long, value_name = "WHEN", num_args = 0..=1, default_missing_value = "bare", value_parser = parse_color)]
    pub color: Option<String>,
    /// Colour lines that were moved (deleted in one place, added in another of the same diff) differently; only visible with colour
    #[arg(long)]
    pub color_moved: bool,
    /// Diff algorithm: myers (default), patience or histogram
    #[arg(long, value_enum, value_name = "ALGORITHM")]
    pub diff_algorithm: Option<AlgArg>,
    /// Lines of context around each change (default 3)
    #[arg(short = 'U', long = "unified", value_name = "N")]
    pub unified: Option<usize>,
    /// Ignore all whitespace when comparing lines
    #[arg(short = 'w', long)]
    pub ignore_all_space: bool,
    /// Detect renamed files (the short -C of git is taken by the global --chdir; use --find-copies)
    #[arg(short = 'M', long)]
    pub find_renames: bool,
    /// Detect copies of modified files as well as renames
    #[arg(long)]
    pub find_copies: bool,
    /// Minimum similarity in percent for rename and copy detection
    #[arg(long, value_name = "PERCENT", default_value_t = 50, value_parser = clap::value_parser!(u32).range(0..=100))]
    pub rename_threshold: u32,
}

fn parse_color(s: &str) -> Result<String, String> {
    match s {
        "auto" | "always" | "never" | "bare" => Ok(s.to_owned()),
        _ => Err("expected auto, always or never".into()),
    }
}

impl DiffArgs {
    /// Nothing but (maybe) `--stat`: the original renderers handle it.
    fn is_plain(&self) -> bool {
        !(self.name_only
            || self.name_status
            || self.word_diff
            || self.color.is_some()
            || self.color_moved
            || self.diff_algorithm.is_some()
            || self.unified.is_some()
            || self.ignore_all_space
            || self.find_renames
            || self.find_copies)
            && !colour_auto()
    }
    pub(crate) fn detect(&self) -> bool {
        self.find_renames || self.find_copies
    }
    /// Colour on or off for this invocation.
    pub fn use_color(&self) -> bool {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        match self.color.as_deref() {
            Some("always") => true,
            Some("never") => false,
            Some("bare") => !no_color,
            _ => colour_auto(),
        }
    }
}

/// Plain default (no `--color`): colour only on a terminal.
fn colour_auto() -> bool {
    std::io::stdout().is_terminal() && !std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Added,
    Deleted,
    Modified,
    Renamed(u32),
    Copied(u32),
}

/// One file of the result after rename detection.
pub struct Entry {
    pub status: Status,
    pub old_path: Option<String>,
    pub path: String,
    pub old: Option<Vec<u8>>,
    pub new: Option<Vec<u8>>,
    pub mode_changed: bool,
}

/// Apply rename / copy detection (when requested) and sort by path.
pub fn entries(pairs: Vec<FilePair>, a: &DiffArgs) -> Vec<Entry> {
    let th = a.rename_threshold * 10;
    let mut gone_ix = Vec::new();
    let mut added_ix = Vec::new();
    let mut mod_ix = Vec::new();
    for (i, p) in pairs.iter().enumerate() {
        match (&p.old, &p.new) {
            (Some(_), None) => gone_ix.push(i),
            (None, Some(_)) => added_ix.push(i),
            (Some(_), Some(_)) => mod_ix.push(i),
            _ => {}
        }
    }
    let mut renamed: HashMap<usize, (usize, u32, bool)> = HashMap::new(); // added idx -> (source idx, score, is_copy)
    let mut consumed: BTreeSet<usize> = BTreeSet::new(); // deleted sources absorbed by a rename
    if a.detect() && !added_ix.is_empty() {
        let g: Vec<(&str, &[u8])> = gone_ix
            .iter()
            .map(|&i| (pairs[i].path.as_str(), pairs[i].old.as_deref().unwrap_or(&[])))
            .collect();
        let ad: Vec<(&str, &[u8])> = added_ix
            .iter()
            .map(|&i| (pairs[i].path.as_str(), pairs[i].new.as_deref().unwrap_or(&[])))
            .collect();
        for (gi, aj, s) in vcs_merge::pair_by_content(&g, &ad, th) {
            renamed.insert(added_ix[aj], (gone_ix[gi], s, false));
            consumed.insert(gone_ix[gi]);
        }
        if a.find_copies {
            let rest: Vec<usize> = added_ix.iter().copied().filter(|i| !renamed.contains_key(i)).collect();
            let srcs: Vec<(&str, &[u8])> = mod_ix
                .iter()
                .map(|&i| (pairs[i].path.as_str(), pairs[i].old.as_deref().unwrap_or(&[])))
                .collect();
            let ad: Vec<(&str, &[u8])> = rest
                .iter()
                .map(|&i| (pairs[i].path.as_str(), pairs[i].new.as_deref().unwrap_or(&[])))
                .collect();
            for (si, aj, s) in vcs_merge::copies_by_content(&srcs, &ad, th) {
                renamed.insert(rest[aj], (mod_ix[si], s, true));
            }
        }
    }
    let mut out = Vec::new();
    for (i, p) in pairs.iter().enumerate() {
        if consumed.contains(&i) {
            continue;
        }
        if let Some(&(src, score, copy)) = renamed.get(&i) {
            let s = &pairs[src];
            out.push(Entry {
                status: if copy { Status::Copied(score) } else { Status::Renamed(score) },
                old_path: Some(s.path.clone()),
                path: p.path.clone(),
                old: s.old.clone(),
                new: p.new.clone(),
                mode_changed: false,
            });
            continue;
        }
        let status = match (&p.old, &p.new) {
            (None, Some(_)) => Status::Added,
            (Some(_), None) => Status::Deleted,
            _ => Status::Modified,
        };
        out.push(Entry {
            status,
            old_path: None,
            path: p.path.clone(),
            old: p.old.clone(),
            new: p.new.clone(),
            mode_changed: p.old == p.new,
        });
    }
    out.sort_by(|x, y| x.path.cmp(&y.path));
    out
}

fn letter(s: Status) -> String {
    match s {
        Status::Added => "A".into(),
        Status::Deleted => "D".into(),
        Status::Modified => "M".into(),
        Status::Renamed(n) => format!("R{:03}", n / 10),
        Status::Copied(n) => format!("C{:03}", n / 10),
    }
}

/// `name-status` lines (also used by `vcs log --name-status`).
pub fn name_status_lines(es: &[Entry]) -> Vec<String> {
    es.iter()
        .map(|e| match &e.old_path {
            Some(o) => format!("{}\t{o}\t{}", letter(e.status), e.path),
            None => format!("{}\t{}", letter(e.status), e.path),
        })
        .collect()
}

// ---------- line diff ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tag {
    Eq,
    Del,
    Ins,
}

type Op = (Tag, usize, usize);

fn split_lines(b: &[u8]) -> Vec<&[u8]> {
    b.split_inclusive(|&c| c == b'\n').collect()
}

/// Edit script of `a` -> `b`. Lines compare equal after dropping whitespace when `ignore_ws`.
fn script(alg: Algorithm, a: &[&[u8]], b: &[&[u8]], ignore_ws: bool) -> Vec<Op> {
    let mut ids: HashMap<Vec<u8>, u32> = HashMap::new();
    let mut intern = |l: &[u8]| -> u32 {
        let key: Vec<u8> = if ignore_ws {
            l.iter().copied().filter(|c| !c.is_ascii_whitespace()).collect()
        } else {
            l.to_vec()
        };
        let n = ids.len() as u32;
        *ids.entry(key).or_insert(n)
    };
    let ai: Vec<u32> = a.iter().map(|l| intern(l)).collect();
    let bi: Vec<u32> = b.iter().map(|l| intern(l)).collect();
    let m = matches(alg, &ai, &bi);
    let mut ops = Vec::with_capacity(a.len().max(b.len()));
    let (mut i, mut j) = (0, 0);
    for (mi, mj) in m.into_iter().chain(std::iter::once((a.len(), b.len()))) {
        while i < mi {
            ops.push((Tag::Del, i, 0));
            i += 1;
        }
        while j < mj {
            ops.push((Tag::Ins, 0, j));
            j += 1;
        }
        if mi < a.len() {
            ops.push((Tag::Eq, mi, mj));
            i += 1;
            j += 1;
        }
    }
    ops
}

/// Hunks as op ranges, with `ctx` lines of context.
fn hunks(ops: &[Op], ctx: usize) -> Vec<(usize, usize)> {
    let changed: Vec<usize> = ops.iter().enumerate().filter(|(_, o)| o.0 != Tag::Eq).map(|(i, _)| i).collect();
    let mut out = Vec::new();
    let mut k = 0;
    while k < changed.len() {
        let lo = changed[k].saturating_sub(ctx);
        let mut hi = changed[k];
        while k + 1 < changed.len() && changed[k + 1] - hi <= 2 * ctx + 1 {
            k += 1;
            hi = changed[k];
        }
        k += 1;
        out.push((lo, (hi + ctx + 1).min(ops.len())));
    }
    out
}

pub fn count_changes(old: &[u8], new: &[u8], alg: Algorithm, ignore_ws: bool) -> (u64, u64) {
    let (a, b) = (split_lines(old), split_lines(new));
    let ops = script(alg, &a, &b, ignore_ws);
    let ins = ops.iter().filter(|o| o.0 == Tag::Ins).count() as u64;
    let del = ops.iter().filter(|o| o.0 == Tag::Del).count() as u64;
    (ins, del)
}

// ---------- word diff ----------

fn tokens(s: &[u8]) -> Vec<&[u8]> {
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80;
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let st = i;
        if word(s[i]) {
            while i < s.len() && word(s[i]) {
                i += 1;
            }
        } else {
            i += 1;
        }
        out.push(&s[st..i]);
    }
    out
}

fn word_block(alg: Algorithm, del: &[u8], ins: &[u8], color: bool) -> String {
    let (a, b) = (tokens(del), tokens(ins));
    let ops = script(alg, &a, &b, false);
    let mut out = String::new();
    let mut cur: Option<Tag> = None;
    let close = |out: &mut String, t: Option<Tag>| match t {
        Some(Tag::Del) => out.push_str(if color { "-]\x1b[0m" } else { "-]" }),
        Some(Tag::Ins) => out.push_str(if color { "+}\x1b[0m" } else { "+}" }),
        _ => {}
    };
    for (t, ai, bi) in ops {
        if cur != Some(t) {
            close(&mut out, cur);
            match t {
                Tag::Del => out.push_str(if color { "\x1b[31m[-" } else { "[-" }),
                Tag::Ins => out.push_str(if color { "\x1b[32m{+" } else { "{+" }),
                Tag::Eq => {}
            }
            cur = Some(t);
        }
        let tok = if t == Tag::Ins { b[bi] } else { a[ai] };
        out.push_str(&String::from_utf8_lossy(tok));
    }
    close(&mut out, cur);
    out
}

// ---------- patch ----------

const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";
const MOVED_FROM: &str = "\x1b[1;35m";
const MOVED_TO: &str = "\x1b[1;36m";
const RESET: &str = "\x1b[0m";

fn paint(on: bool, code: &str, s: &str) -> String {
    if on {
        format!("{code}{s}{RESET}")
    } else {
        s.to_owned()
    }
}

fn line_text(l: &[u8]) -> String {
    String::from_utf8_lossy(l).into_owned()
}

/// Render a patch for `es`.
pub fn patch(es: &[Entry], a: &DiffArgs, color: bool) -> String {
    let alg = a.diff_algorithm.map_or(Algorithm::Myers, AlgArg::alg);
    let ctx = a.unified.unwrap_or(3);
    // moved-line sets (whole diff): trimmed non-blank lines deleted somewhere / added somewhere
    let (mut dels, mut inss): (BTreeSet<Vec<u8>>, BTreeSet<Vec<u8>>) = (BTreeSet::new(), BTreeSet::new());
    if color && a.color_moved {
        for e in es {
            let (ob, nb) = (e.old.as_deref().unwrap_or(&[]), e.new.as_deref().unwrap_or(&[]));
            if is_binary(ob) || is_binary(nb) {
                continue;
            }
            let (x, y) = (split_lines(ob), split_lines(nb));
            for (t, i, j) in script(alg, &x, &y, a.ignore_all_space) {
                match t {
                    Tag::Del => {
                        dels.insert(x[i].trim_ascii().to_vec());
                    }
                    Tag::Ins => {
                        inss.insert(y[j].trim_ascii().to_vec());
                    }
                    Tag::Eq => {}
                }
            }
        }
    }
    let moved_del = |l: &[u8]| !l.trim_ascii().is_empty() && inss.contains(l.trim_ascii());
    let moved_ins = |l: &[u8]| !l.trim_ascii().is_empty() && dels.contains(l.trim_ascii());
    let mut out = String::new();
    for e in es {
        let (ob, nb) = (e.old.as_deref().unwrap_or(&[]), e.new.as_deref().unwrap_or(&[]));
        let (op, np) = (e.old_path.as_deref().unwrap_or(&e.path), e.path.as_str());
        let binary = is_binary(ob) || is_binary(nb);
        let (x, y) = if binary {
            (vec![], vec![])
        } else {
            (split_lines(ob), split_lines(nb))
        };
        let ops = if binary { vec![] } else { script(alg, &x, &y, a.ignore_all_space) };
        let has_change = ops.iter().any(|o| o.0 != Tag::Eq);
        let rename_like = matches!(e.status, Status::Renamed(_) | Status::Copied(_));
        if !rename_like && !binary && !has_change && !e.mode_changed {
            continue; // e.g. only whitespace differences under -w
        }
        let mut head = format!("diff --vcs a/{op} b/{np}\n");
        match e.status {
            Status::Added => head.push_str("new file\n"),
            Status::Deleted => head.push_str("deleted file\n"),
            Status::Renamed(s) | Status::Copied(s) => {
                let kind = if matches!(e.status, Status::Renamed(_)) { "rename" } else { "copy" };
                head.push_str(&format!("similarity index {}%\n{kind} from {op}\n{kind} to {np}\n", s / 10));
            }
            Status::Modified => {}
        }
        out.push_str(&paint(color, BOLD, head.trim_end()));
        out.push('\n');
        if binary {
            let l = if e.old.is_none() {
                "/dev/null".to_owned()
            } else {
                format!("a/{op}")
            };
            let r = if e.new.is_none() {
                "/dev/null".to_owned()
            } else {
                format!("b/{np}")
            };
            out.push_str(&format!(
                "Binary files {l} and {r} differ ({} -> {} bytes)\n",
                e.old.as_ref().map_or(0, Vec::len),
                e.new.as_ref().map_or(0, Vec::len)
            ));
            continue;
        }
        if !has_change {
            if e.mode_changed {
                out.push_str("mode changed\n");
            }
            continue;
        }
        out.push_str(&paint(
            color,
            BOLD,
            &format!("--- {}", if e.old.is_some() { format!("a/{op}") } else { "/dev/null".into() }),
        ));
        out.push('\n');
        out.push_str(&paint(
            color,
            BOLD,
            &format!("+++ {}", if e.new.is_some() { format!("b/{np}") } else { "/dev/null".into() }),
        ));
        out.push('\n');
        let mut apos = Vec::with_capacity(ops.len() + 1);
        let mut bpos = Vec::with_capacity(ops.len() + 1);
        let (mut ac, mut bc) = (0, 0);
        for (t, _, _) in &ops {
            apos.push(ac);
            bpos.push(bc);
            if *t != Tag::Ins {
                ac += 1;
            }
            if *t != Tag::Del {
                bc += 1;
            }
        }
        for (lo, hi) in hunks(&ops, ctx) {
            let (mut al, mut bl) = (0, 0);
            for (t, _, _) in &ops[lo..hi] {
                if *t != Tag::Ins {
                    al += 1;
                }
                if *t != Tag::Del {
                    bl += 1;
                }
            }
            let st = |pos: usize, len: usize| if len == 0 { pos } else { pos + 1 };
            let h = format!("@@ -{},{} +{},{} @@", st(apos[lo], al), al, st(bpos[lo], bl), bl);
            out.push_str(&paint(color, CYAN, &h));
            out.push('\n');
            if a.word_diff {
                let mut k = lo;
                while k < hi {
                    if ops[k].0 == Tag::Eq {
                        out.push_str(&line_text(y[ops[k].2]));
                        if !y[ops[k].2].ends_with(b"\n") {
                            out.push('\n');
                        }
                        k += 1;
                        continue;
                    }
                    let (mut d, mut n) = (Vec::new(), Vec::new());
                    while k < hi && ops[k].0 != Tag::Eq {
                        match ops[k].0 {
                            Tag::Del => d.extend_from_slice(x[ops[k].1]),
                            _ => n.extend_from_slice(y[ops[k].2]),
                        }
                        k += 1;
                    }
                    let s = word_block(alg, &d, &n, color);
                    out.push_str(&s);
                    if !s.ends_with('\n') {
                        out.push('\n');
                    }
                }
                continue;
            }
            for (t, ai, bi) in &ops[lo..hi] {
                let (prefix, line, code) = match t {
                    Tag::Eq => (' ', y[*bi], ""),
                    Tag::Del => ('-', x[*ai], if moved_del(x[*ai]) { MOVED_FROM } else { RED }),
                    Tag::Ins => ('+', y[*bi], if moved_ins(y[*bi]) { MOVED_TO } else { GREEN }),
                };
                let body = line_text(line);
                let body = body.strip_suffix('\n').unwrap_or(&body);
                out.push_str(&if code.is_empty() || !color {
                    format!("{prefix}{body}")
                } else {
                    format!("{code}{prefix}{body}{RESET}")
                });
                out.push('\n');
                if !line.ends_with(b"\n") {
                    out.push_str("\\ No newline at end of file\n");
                }
            }
        }
    }
    out
}

/// `--stat` text for `es`.
pub fn stat(es: &[Entry], a: &DiffArgs) -> String {
    let alg = a.diff_algorithm.map_or(Algorithm::Myers, AlgArg::alg);
    let mut out = String::new();
    let (mut files, mut ins_t, mut del_t) = (0, 0u64, 0u64);
    for e in es {
        let (ob, nb) = (e.old.as_deref().unwrap_or(&[]), e.new.as_deref().unwrap_or(&[]));
        let name = match &e.old_path {
            Some(o) => format!("{o} => {}", e.path),
            None => e.path.clone(),
        };
        if is_binary(ob) || is_binary(nb) {
            out.push_str(&format!(" {name} | Bin {} -> {} bytes\n", ob.len(), nb.len()));
            files += 1;
            continue;
        }
        let (i, d) = count_changes(ob, nb, alg, a.ignore_all_space);
        if i + d == 0 && !matches!(e.status, Status::Renamed(_) | Status::Copied(_)) && !e.mode_changed {
            continue;
        }
        files += 1;
        ins_t += i;
        del_t += d;
        out.push_str(&format!(" {name} | +{i} -{d}\n"));
    }
    out.push_str(&format!(" {files} file(s) changed, {ins_t} insertion(s), {del_t} deletion(s)\n"));
    out
}

/// Render `pairs` per the options.
pub fn render_pairs(pairs: Vec<FilePair>, a: &DiffArgs) -> String {
    let es = entries(pairs, a);
    if a.name_only {
        return es.iter().map(|e| format!("{}\n", e.path)).collect();
    }
    if a.name_status {
        return name_status_lines(&es).into_iter().map(|l| l + "\n").collect();
    }
    if a.stat {
        return stat(&es, a);
    }
    patch(&es, a, a.use_color())
}

/// `vcs diff [--staged]`.
pub fn run_diff(repo: &Repo, staged: bool, a: &DiffArgs) -> Result<(), Error> {
    if a.is_plain() {
        let d = repo.diff(staged)?;
        if a.stat {
            let stats = crate::json::diff_stat(&d);
            for s in &stats {
                if s.binary {
                    println!(" {} | binary", s.path);
                } else {
                    println!(" {} | +{} -{}", s.path, s.added, s.deleted);
                }
            }
            println!(
                " {} file(s) changed, {} insertion(s), {} deletion(s)",
                stats.len(),
                stats.iter().map(|s| s.added).sum::<u64>(),
                stats.iter().map(|s| s.deleted).sum::<u64>()
            );
        } else {
            print!("{d}");
        }
        return Ok(());
    }
    print!("{}", render_pairs(repo.diff_pairs(staged)?, a));
    Ok(())
}

/// The diff part of `vcs show`.
pub fn show_diff(repo: &Repo, rep: &vcs_repo::ShowReport, a: &DiffArgs) -> Result<String, Error> {
    if a.is_plain() && !a.stat {
        return Ok(rep.diff.clone());
    }
    Ok(render_pairs(repo.commit_pairs(&rep.commit.zone, &rep.id)?, a))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apply the hunks of a unified `patch` for one file to `old`.
    fn apply(old: &str, patch: &str) -> String {
        let a: Vec<&str> = old.split_inclusive('\n').collect();
        let mut out = String::new();
        let mut pos = 0;
        let lines = patch.lines().skip_while(|l| !l.starts_with("@@"));
        for h in lines {
            if h.starts_with("@@") {
                let s: usize = h.split(' ').nth(1).unwrap()[1..].split(',').next().unwrap().parse().unwrap();
                let len: usize = h.split(' ').nth(1).unwrap().split(',').nth(1).unwrap().parse().unwrap();
                let start = if len == 0 { s } else { s - 1 };
                while pos < start {
                    out.push_str(a[pos]);
                    pos += 1;
                }
            } else if let Some(r) = h.strip_prefix('+') {
                out.push_str(r);
                out.push('\n');
            } else if h.starts_with('-') {
                pos += 1;
            } else if let Some(r) = h.strip_prefix(' ') {
                out.push_str(r);
                out.push('\n');
                pos += 1;
            }
        }
        while pos < a.len() {
            out.push_str(a[pos]);
            pos += 1;
        }
        out
    }

    fn pair(old: &str, new: &str) -> Vec<FilePair> {
        vec![FilePair {
            path: "f.txt".into(),
            old: Some(old.as_bytes().to_vec()),
            new: Some(new.as_bytes().to_vec()),
            old_mode: Some(vcs_objects::EntryMode::File),
            new_mode: Some(vcs_objects::EntryMode::File),
        }]
    }

    #[test]
    fn every_algorithm_and_context_round_trips() {
        let mut s = 0x1234_5678_9abc_def1u64;
        let mut rnd = |m: u64| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s % m
        };
        for _ in 0..300 {
            let alpha = 2 + rnd(6);
            let old: String = (0..rnd(30)).map(|_| format!("l{}\n", rnd(alpha))).collect();
            let new: String = (0..rnd(30)).map(|_| format!("l{}\n", rnd(alpha))).collect();
            for alg in [AlgArg::Myers, AlgArg::Patience, AlgArg::Histogram] {
                for ctx in [0usize, 1, 3] {
                    let a = DiffArgs {
                        diff_algorithm: Some(alg),
                        unified: Some(ctx),
                        ..Default::default()
                    };
                    let p = patch(&entries(pair(&old, &new), &a), &a, false);
                    assert_eq!(apply(&old, &p), new, "{alg:?} U{ctx}\nold={old:?}\nnew={new:?}\n{p}");
                }
            }
        }
    }

    #[test]
    fn whitespace_and_word_diff() {
        let a = DiffArgs {
            ignore_all_space: true,
            ..Default::default()
        };
        assert_eq!(patch(&entries(pair("a b\nc\n", "a  b\nc\n"), &a), &a, false), "");
        let w = DiffArgs {
            word_diff: true,
            ..Default::default()
        };
        let p = patch(&entries(pair("one two three\n", "one 2 three\n"), &w), &w, false);
        assert!(p.contains("one [-two-]{+2+} three"), "{p}");
    }

    #[test]
    fn binary_summary_and_stat() {
        let p = vec![FilePair {
            path: "b.bin".into(),
            old: Some(vec![0, 1]),
            new: Some(vec![0, 1, 2]),
            old_mode: None,
            new_mode: None,
        }];
        let a = DiffArgs::default();
        assert!(patch(&entries(p.clone(), &a), &a, false).contains("Binary files a/b.bin and b/b.bin differ (2 -> 3 bytes)"));
        let s = DiffArgs {
            stat: true,
            ..Default::default()
        };
        assert!(stat(&entries(p, &s), &s).contains("b.bin | Bin 2 -> 3 bytes"));
    }
}
