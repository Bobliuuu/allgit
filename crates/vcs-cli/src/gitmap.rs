//! `vcs git <git-args...>`: run a git-style command line as its native vcs equivalent.
//!
//! vcs is deliberately not git-format-compatible; this front-end gives a git user the same
//! commands anyway. It parses the git arguments, rewrites them to native vcs invocations (or, for
//! a few commands, a short native composition) and runs them in this process. No git binary and
//! no libgit2 are involved. When a git command or flag has no vcs equivalent it fails with
//! `git <cmd>: no vcs equivalent (...)` and names the native way to get the same result.
//!
//! Where an equivalent behaves differently from git (for example `reset` acts on one zone,
//! `pull` only fast-forwards), the command prints a `vcs git: note:` line on stderr, silenced by
//! `VCS_GIT_QUIET=1`. The mapping table lives in [`table`] and is rendered into
//! docs/git-to-vcs.md; a test keeps the two in sync. Install guidance: docs/git-to-vcs.md.

use super::{open, zone_of_cwd};
use clap::{Args, Parser};
use vcs_repo::Error;

#[path = "gitmap_table.rs"]
pub mod table;

#[derive(Args, Clone)]
pub struct GitArgs {
    /// A git command line, e.g. `vcs git log --oneline -5`
    #[arg(allow_hyphen_values = true, trailing_var_arg = true, value_name = "GIT_ARGS")]
    pub args: Vec<String>,
}

/// When invoked through a symlink or copy named `git`, behave as `vcs git ...`.
pub fn argv_for_shim() -> Vec<std::ffi::OsString> {
    let mut a: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let is_git = a
        .first()
        .and_then(|p| std::path::Path::new(p).file_stem().map(|s| s == "git"))
        .unwrap_or(false);
    if is_git {
        a[0] = "vcs".into();
        a.insert(1, "git".into());
    }
    a
}

// ---------------------------------------------------------------- steps

/// One native action. A git command translates to one or a few of these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Native argv, without the leading `vcs`.
    Vcs(Vec<String>),
    /// Stage every modified or deleted TRACKED file (git's `add -u`, `commit -a`), under `paths` if given.
    AddTracked(Vec<String>),
    /// `commit --amend`: soft-reset the zone's branch to the parent, then commit with the old
    /// (or given) message; restores the old head when the commit fails.
    Amend { commit_argv: Vec<String>, message: Option<String> },
    /// `status --short`: `XY path` lines.
    StatusShort { branch: bool },
    /// Print the checked-out branch of the current zone.
    ShowCurrent,
    /// Diff between revisions/worktree/index limited to paths (`diff A B`, `diff HEAD`, `diff -- f`).
    Diff {
        staged: bool,
        specs: Vec<String>,
        flags: Vec<String>,
    },
    /// `log -p` / `log --stat`: native `log` selection, each commit shown with its diff.
    LogPatch { log_argv: Vec<String>, stat: bool },
    /// `checkout <rev> -- paths` / `restore --source`: file contents from a revision.
    FromRev { rev: String, paths: Vec<String>, stage: bool },
    /// `checkout X`: a branch (switch), or paths (restore) when X names a file and no branch.
    CheckoutAuto { target: String, paths: Vec<String> },
    /// Printed on stderr before running (a stated semantic difference).
    Note(String),
}

impl Step {
    #[cfg(test)]
    /// The canonical text of the step, as shown in docs/git-to-vcs.md.
    pub fn display(&self) -> String {
        match self {
            Step::Vcs(a) => format!("vcs {}", a.join(" ")),
            Step::AddTracked(p) if p.is_empty() => "vcs add <every modified or deleted tracked file>".into(),
            Step::AddTracked(p) => format!("vcs add <tracked changes under {}>", p.join(" ")),
            Step::Amend { message: Some(m), .. } => format!("vcs reset --soft HEAD^ && vcs commit -m {m}"),
            Step::Amend { message: None, .. } => "vcs reset --soft HEAD^ && vcs commit -m <old message>".into(),
            Step::StatusShort { branch } => format!("vcs status (rendered as XY lines{})", if *branch { " with ## branch" } else { "" }),
            Step::ShowCurrent => "vcs branch (the * line)".into(),
            Step::Diff { staged, specs, flags } => {
                let mut v = vec!["vcs git diff".to_string()];
                v.extend(flags.iter().cloned());
                if *staged {
                    v.push("--staged".into());
                }
                v.extend(specs.iter().cloned());
                v.join(" ")
            }
            Step::LogPatch { stat, .. } => format!("vcs log + vcs show {}<each>", if *stat { "--stat " } else { "" }),
            Step::FromRev { rev, paths, stage } => format!(
                "vcs cat-file -p {rev}:<path> into the file{} for {}",
                if *stage { " + vcs add" } else { "" },
                paths.join(" ")
            ),
            Step::CheckoutAuto { target, .. } => {
                format!("vcs switch {target}  (or vcs switch --detach for a tag or commit, vcs restore for a file)")
            }
            Step::Note(_) => String::new(),
        }
    }
}

#[cfg(test)]
/// `display()` of all steps, notes left out, joined by ` ; `.
pub fn render(steps: &[Step]) -> String {
    steps
        .iter()
        .map(Step::display)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ; ")
}

// ---------------------------------------------------------------- errors

fn missing(git: &str, why: &str, native: &str) -> Error {
    Error::Msg(format!("git {git}: no vcs equivalent ({why})\n  native: {native}"))
}

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------- argument parser

/// Parsed git arguments: `-x`/`--long[=val]` options (keys normalised), positionals, and what follows `--`.
struct P {
    opts: Vec<(String, Option<String>)>,
    pos: Vec<String>,
    dd: Vec<String>,
    had_dd: bool,
    cmd: String,
}

fn parse(cmd: &str, args: &[String], valued: &[&str]) -> P {
    let mut p = P {
        opts: vec![],
        pos: vec![],
        dd: vec![],
        had_dd: false,
        cmd: cmd.to_owned(),
    };
    let mut i = 0;
    while i < args.len() {
        let t = &args[i];
        i += 1;
        if p.had_dd {
            p.dd.push(t.clone());
        } else if t == "--" {
            p.had_dd = true;
        } else if let Some(long) = t.strip_prefix("--") {
            let (name, val) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_owned())),
                None => (long, None),
            };
            let key = format!("--{name}");
            let val = if val.is_none() && valued.contains(&key.as_str()) && i < args.len() {
                i += 1;
                Some(args[i - 1].clone())
            } else {
                val
            };
            p.opts.push((key, val));
        } else if t.len() > 1 && t.starts_with('-') && t[1..].bytes().all(|b| b.is_ascii_digit()) {
            p.opts.push(("-N".into(), Some(t[1..].to_owned())));
        } else if t.len() > 2 && (t.starts_with("-M") || t.starts_with("-C")) && t[2..].bytes().all(|b| b.is_ascii_digit() || b == b'%') {
            p.opts.push((t[..2].to_owned(), None));
        } else if t.len() > 1 && t.starts_with('-') {
            let chars: Vec<char> = t[1..].chars().collect();
            let mut k = 0;
            while k < chars.len() {
                let key = format!("-{}", chars[k]);
                if valued.contains(&key.as_str()) {
                    let rest: String = chars[k + 1..].iter().collect();
                    let val = if !rest.is_empty() {
                        rest
                    } else if i < args.len() {
                        i += 1;
                        args[i - 1].clone()
                    } else {
                        String::new()
                    };
                    p.opts.push((key, Some(val)));
                    break;
                }
                p.opts.push((key, None));
                k += 1;
            }
        } else {
            p.pos.push(t.clone());
        }
    }
    p
}

impl P {
    /// Remove and report presence of a boolean option (any of `names`).
    fn take(&mut self, names: &[&str]) -> bool {
        let n = self.opts.len();
        self.opts.retain(|(k, _)| !names.contains(&k.as_str()));
        self.opts.len() != n
    }
    /// Remove an option and return its (last) value.
    fn val(&mut self, names: &[&str]) -> Option<String> {
        let mut out = None;
        self.opts.retain(|(k, val)| {
            if names.contains(&k.as_str()) {
                out = val.clone().or(Some(String::new()));
                false
            } else {
                true
            }
        });
        out
    }
    fn vals(&mut self, names: &[&str]) -> Vec<String> {
        let mut out = vec![];
        self.opts.retain(|(k, val)| {
            if names.contains(&k.as_str()) {
                out.push(val.clone().unwrap_or_default());
                false
            } else {
                true
            }
        });
        out
    }
    /// Every option must have been taken; leftovers have no vcs equivalent.
    fn finish(&self, native: &str) -> Result<(), Error> {
        if let Some((k, _)) = self.opts.first() {
            return Err(missing(&self.cmd, &format!("the flag {k} is not supported"), native));
        }
        Ok(())
    }
    /// Positionals followed by the paths after `--`.
    fn all_pos(&self) -> Vec<String> {
        self.pos.iter().chain(&self.dd).cloned().collect()
    }
}

fn stash_index(s: &str) -> Result<String, Error> {
    let t = s.strip_prefix("stash@{").and_then(|r| r.strip_suffix('}')).unwrap_or(s);
    t.parse::<usize>()
        .map(|n| n.to_string())
        .map_err(|_| Error::Msg(format!("bad stash reference {s:?}; use stash@{{N}} or N")))
}

fn note(s: &str) -> Step {
    Step::Note(s.to_owned())
}

/// `https://host/a/b.git` -> `b`
fn dir_of(url: &str) -> String {
    let t = url.trim_end_matches('/');
    let last = t.rsplit(['/', ':']).next().unwrap_or(t);
    let last = last.strip_suffix(".git").unwrap_or(last);
    let last = last.strip_suffix(".bundle").unwrap_or(last);
    if last.is_empty() {
        "repo".into()
    } else {
        last.to_owned()
    }
}

// ---------------------------------------------------------------- translation

/// Translate one git command line (without the leading `git`) into native steps.
pub fn translate(args: &[String]) -> Result<Vec<Step>, Error> {
    let mut a = args.to_vec();
    // git's own leading options
    let mut pre: Vec<String> = vec![];
    loop {
        match a.first().map(String::as_str) {
            Some("--no-pager" | "-P" | "--paginate" | "-p" | "--no-optional-locks" | "--no-replace-objects") => {
                a.remove(0);
            }
            Some("-c") => {
                return Err(missing(
                    "-c <name>=<value>",
                    "per-invocation config overrides",
                    "`vcs config set <key> <value>`, or the VCS_* environment variables (docs/guide/11-troubleshooting.md)",
                ))
            }
            Some("--git-dir" | "--work-tree") => {
                return Err(missing(
                    "--git-dir/--work-tree",
                    "vcs keeps its state in <worktree>/.vcs",
                    "`vcs -C <dir> ...` or `vcs worktree add`",
                ))
            }
            Some("-C") if a.len() > 1 => {
                pre.push(a[1].clone());
                a.drain(..2);
            }
            _ => break,
        }
    }
    let Some(cmd) = a.first().cloned() else {
        return Err(Error::Msg(
            "usage: vcs git <git command> [args]   (see docs/git-to-vcs.md; `vcs git --map` lists every mapping)".into(),
        ));
    };
    let rest = &a[1..];
    let mut steps = translate_cmd(&cmd, rest)?;
    if let Some(dir) = pre.first() {
        for s in &mut steps {
            if let Step::Vcs(argv) = s {
                argv.splice(0..0, ["-C".to_string(), dir.clone()]);
            }
        }
    }
    Ok(steps)
}

fn passthrough(name: &str, rest: &[String]) -> Vec<Step> {
    let mut a = vec![name.to_owned()];
    a.extend(rest.iter().cloned());
    vec![Step::Vcs(a)]
}

#[allow(clippy::too_many_lines)]
fn translate_cmd(cmd: &str, rest: &[String]) -> Result<Vec<Step>, Error> {
    Ok(match cmd {
        "--version" | "version" => vec![Step::Vcs(v(&["--version"]))],
        "help" | "--help" | "-h" => {
            if rest.is_empty() {
                vec![Step::Vcs(v(&["help"]))]
            } else {
                vec![Step::Vcs(v(&["help", &rest[0]]))]
            }
        }
        "init" => {
            let mut p = parse(cmd, rest, &["-b", "--initial-branch", "--template", "--separate-git-dir", "--object-format"]);
            p.take(&["-q", "--quiet"]);
            p.finish("`vcs init [path]` (the first branch is always main; no bare repositories)")?;
            let mut a = v(&["init"]);
            a.extend(p.pos.first().cloned());
            vec![Step::Vcs(a)]
        }
        "clone" => {
            let mut p = parse(cmd, rest, &["-b", "--branch", "--depth", "-o", "--origin", "--zone", "--template", "--reference", "--filter"]);
            p.take(&["-q", "--quiet", "--progress", "-v", "--verbose", "--no-checkout-ignored"]);
            let zones = p.vals(&["--zone"]);
            p.finish("`vcs clone [--zone Z]... <url> <dir>`; clones are never shallow or bare; `--zone`/`--for` fetch only parts")?;
            if p.pos.is_empty() || p.pos.len() > 2 {
                return Err(Error::Msg("usage: git clone <url> [<dir>]".into()));
            }
            let dir = p.pos.get(1).cloned().unwrap_or_else(|| dir_of(&p.pos[0]));
            let mut a = v(&["clone"]);
            for z in zones {
                a.extend(["--zone".to_owned(), z]);
            }
            a.extend([p.pos[0].clone(), dir]);
            vec![Step::Vcs(a)]
        }
        "add" => {
            let mut p = parse(cmd, rest, &[]);
            let all = p.take(&["-A", "--all"]);
            let upd = p.take(&["-u", "--update"]);
            p.take(&["-v", "--verbose"]);
            p.finish("`vcs add <paths>` stages new, modified and deleted files; no -p/-i/-n/-f/-N (use .vcsignore for ignored files)")?;
            let paths = p.all_pos();
            if upd {
                vec![Step::AddTracked(paths)]
            } else if paths.is_empty() && all {
                vec![Step::Vcs(v(&["add", "."]))]
            } else {
                let mut a = v(&["add"]);
                a.extend(paths);
                vec![Step::Vcs(a)]
            }
        }
        "rm" => {
            let mut p = parse(cmd, rest, &[]);
            p.take(&["-r", "-q", "--quiet"]);
            let f = p.take(&["-f", "--force"]);
            let cached = p.take(&["--cached"]);
            p.finish("`vcs rm [--cached] [--force] <paths>` (no -n, --ignore-unmatch)")?;
            let mut a = v(&["rm"]);
            if cached {
                a.push("--cached".into());
            }
            if f {
                a.push("--force".into());
            }
            a.extend(p.all_pos());
            vec![Step::Vcs(a)]
        }
        "mv" => {
            let mut p = parse(cmd, rest, &[]);
            let f = p.take(&["-f", "--force"]);
            p.take(&["-v", "--verbose"]);
            p.finish("`vcs mv [--force] <src> <dst>` (no -k, -n)")?;
            let pos = p.all_pos();
            if pos.len() < 2 {
                return Err(Error::Msg("usage: git mv <source>... <destination>".into()));
            }
            let (dst, srcs) = pos.split_last().unwrap();
            srcs.iter()
                .map(|s| {
                    let mut a = v(&["mv"]);
                    if f {
                        a.push("--force".into());
                    }
                    a.extend([s.clone(), dst.clone()]);
                    Step::Vcs(a)
                })
                .collect()
        }
        "status" => {
            let mut p = parse(cmd, rest, &["-u", "--untracked-files", "--porcelain"]);
            let short = p.take(&["-s", "--short"]);
            let porcelain = p.opts.iter().any(|(k, _)| k == "--porcelain");
            p.take(&["--porcelain"]);
            let branch = p.take(&["-b", "--branch"]);
            p.take(&["-v", "--verbose", "--no-renames", "--renames"]);
            p.finish("`vcs status` (no --ignored, -u<mode>, --column)")?;
            if short || porcelain {
                vec![Step::StatusShort { branch }]
            } else {
                vec![Step::Vcs(v(&["status"]))]
            }
        }
        "diff" | "difftool" => translate_diff(cmd, rest)?,
        "commit" => translate_commit(rest)?,
        "log" | "whatchanged" => translate_log(rest)?,
        "show" => {
            let mut p = parse(cmd, rest, &["-U", "--unified", "--diff-algorithm", "--format", "--pretty"]);
            let mut flags = diff_flags(&mut p)?;
            p.finish("`vcs show [rev] [--stat|--name-only|--name-status|...]` (no --format/-s/--oneline for show; use `vcs log --format`)")?;
            if p.pos.len() > 1 {
                return Err(missing(cmd, "several objects", "`vcs show <rev>` once per revision"));
            }
            if let Some(r) = p.pos.first() {
                if let Some((rev, path)) = r.split_once(':') {
                    let rev = if rev.is_empty() { "HEAD" } else { rev };
                    return Ok(vec![Step::Vcs(v(&["cat-file", "-p", &format!("{rev}:{path}")]))]);
                }
                flags.push(r.clone());
            }
            let mut a = vec!["show".to_owned()];
            a.extend(flags);
            vec![Step::Vcs(a)]
        }
        "branch" => translate_branch(rest)?,
        "switch" => {
            let mut p = parse(cmd, rest, &["-c", "-C", "--create", "--force-create", "--orphan"]);
            let create = p.val(&["-c", "--create"]);
            let detach = p.take(&["-d", "--detach"]);
            p.take(&["-q", "--quiet", "--guess", "--no-guess", "-m", "--merge", "-t", "--track"]);
            p.finish("`vcs switch [-c] [--detach] <branch|rev>`; no -C/--orphan/--discard-changes")?;
            if detach {
                match p.pos.as_slice() {
                    [] => vec![Step::Vcs(v(&["switch", "--detach", "HEAD"]))],
                    [r] => vec![Step::Vcs(v(&["switch", "--detach", r]))],
                    _ => return Err(Error::Msg("usage: git switch --detach [<commit>]".into())),
                }
            } else if let Some(n) = create {
                match p.pos.as_slice() {
                    [] => vec![Step::Vcs(v(&["switch", "-c", &n]))],
                    [s] if s == "HEAD" => vec![Step::Vcs(v(&["switch", "-c", &n]))],
                    [s] => vec![Step::Vcs(v(&["branch", &n, s])), Step::Vcs(v(&["switch", &n]))],
                    _ => return Err(Error::Msg("usage: git switch -c <new> [<start-point>]".into())),
                }
            } else {
                match p.pos.as_slice() {
                    [b] if b == "-" => return Err(missing(cmd, "`switch -` (previous branch)", "`vcs op log` shows previous checkouts; `vcs switch <branch>`")),
                    [b] => vec![Step::Vcs(v(&["switch", b]))],
                    _ => return Err(Error::Msg("usage: git switch [-c] <branch>".into())),
                }
            }
        }
        "checkout" => translate_checkout(rest)?,
        "restore" => translate_restore(rest)?,
        "reset" => translate_reset(rest)?,
        "revert" | "cherry-pick" => {
            let mut p = parse(cmd, rest, &["-m", "--mainline", "--strategy", "-X"]);
            let cont = p.take(&["--continue"]);
            let abort = p.take(&["--abort"]);
            let x = p.take(&["-x"]);
            let noedit = p.take(&["--no-edit", "-e", "--edit", "-s", "--signoff"]);
            let _ = noedit;
            p.finish(&format!("`vcs {cmd} <rev>`; no -n/--no-commit, -m, ranges, --skip, --quit"))?;
            if cont {
                return Ok(vec![note("a stopped pick/revert is finished with `vcs commit`"), Step::Vcs(v(&["commit"]))]);
            }
            if abort {
                if cmd == "revert" {
                    return Err(missing("revert --abort", "no separate abort for revert", "`vcs merge --abort`"));
                }
                return Ok(vec![Step::Vcs(v(&["merge", "--abort"]))]);
            }
            if p.pos.is_empty() {
                return Err(Error::Msg(format!("usage: git {cmd} <commit>...")));
            }
            let mut out = vec![];
            if x {
                out.push(note("-x ignored: vcs does not append '(cherry picked from commit ...)'; the change id is kept instead"));
            }
            for r in &p.pos {
                if r.contains("..") {
                    return Err(missing(cmd, "commit ranges", &format!("`vcs {cmd} <rev>` once per commit (see `vcs log`)")));
                }
                out.push(Step::Vcs(v(&[cmd, r])));
            }
            out
        }
        "merge" => {
            let mut p = parse(cmd, rest, &["-m", "--message", "-s", "--strategy", "-X"]);
            let abort = p.take(&["--abort"]);
            let cont = p.take(&["--continue"]);
            p.take(&["-q", "--quiet", "--no-edit", "--edit", "-e", "--verbose", "-v", "--stat", "--no-stat", "--ff", "--no-verify"]);
            p.finish("`vcs merge <branch>` (fast-forward when possible, else a merge commit); no --no-ff/--squash/--ff-only/-s/-m")?;
            if abort {
                vec![Step::Vcs(v(&["merge", "--abort"]))]
            } else if cont {
                vec![Step::Vcs(v(&["commit"]))]
            } else {
                match p.pos.as_slice() {
                    [b] => match b.strip_prefix("origin/") {
                        Some(r) => vec![Step::Vcs(v(&["merge", "--remote", r]))],
                        None => vec![Step::Vcs(v(&["merge", b]))],
                    },
                    _ => return Err(missing(cmd, "merging several branches", "`vcs merge <branch>` once per branch")),
                }
            }
        }
        "rebase" => {
            let mut p = parse(cmd, rest, &["--onto", "-s", "-X", "-x"]);
            let cont = p.take(&["--continue"]);
            let abort = p.take(&["--abort"]);
            p.take(&["-q", "--quiet", "--no-verify", "-v", "--verbose", "--no-stat", "--stat"]);
            p.finish("`vcs rebase <branch>` replays the checked-out branch onto a local branch; no -i/--onto/--skip/--autostash/--root")?;
            if cont {
                vec![Step::Vcs(v(&["rebase", "--continue"]))]
            } else if abort {
                vec![Step::Vcs(v(&["rebase", "--abort"]))]
            } else {
                match p.pos.as_slice() {
                    [b] if b.starts_with("origin/") => {
                        return Err(missing(cmd, "rebasing onto a remote-tracking branch", "`vcs sync` then `vcs rebase <local branch>`"))
                    }
                    [b] => vec![Step::Vcs(v(&["rebase", b]))],
                    _ => return Err(missing(cmd, "upstream/branch pairs", "`vcs switch <branch>` then `vcs rebase <onto>`")),
                }
            }
        }
        "stash" => translate_stash(rest)?,
        "tag" => translate_tag(rest)?,
        "remote" => {
            let mut p = parse(cmd, rest, &[]);
            let verbose = p.take(&["-v", "--verbose"]);
            p.finish("`vcs remote set <url>` / `vcs remote show`")?;
            let (sub, args) = match p.pos.split_first() {
                None => ("show".to_owned(), vec![]),
                Some((s, r)) => (s.clone(), r.to_vec()),
            };
            let _ = verbose;
            match sub.as_str() {
                "show" | "get-url" => vec![Step::Vcs(v(&["remote", "show"]))],
                "add" | "set-url" => {
                    if args.len() < 2 {
                        return Err(Error::Msg(format!("usage: git remote {sub} <name> <url>")));
                    }
                    let mut out = vec![];
                    if args[0] != "origin" {
                        out.push(note("vcs has ONE remote; the name is ignored (it is always 'origin')"));
                    }
                    out.push(Step::Vcs(v(&["remote", "set", &args[1]])));
                    out
                }
                _ => {
                    return Err(missing(
                        &format!("remote {sub}"),
                        "vcs has a single remote",
                        "`vcs remote set <url>` replaces it; there is nothing to remove, rename or prune",
                    ))
                }
            }
        }
        "fetch" => {
            let mut p = parse(cmd, rest, &[]);
            p.take(&["-q", "--quiet", "--all", "-v", "--verbose", "-p", "--prune", "--tags", "-t", "--no-tags", "-f", "--force"]);
            p.finish("`vcs fetch` (no --depth/--unshallow/refspecs)")?;
            match p.pos.as_slice() {
                [] => vec![Step::Vcs(v(&["fetch"]))],
                [r] if r == "origin" => vec![Step::Vcs(v(&["fetch"]))],
                [f] if std::path::Path::new(f).is_file() => vec![Step::Vcs(v(&["fetch", f]))],
                _ => return Err(missing(cmd, "named remotes and refspecs", "`vcs fetch` (the one remote), or `vcs fetch <bundle-file>`")),
            }
        }
        "pull" => {
            let mut p = parse(cmd, rest, &[]);
            let rebase = p.take(&["--rebase", "-r"]);
            p.take(&["-q", "--quiet", "--ff-only", "--no-rebase", "--ff", "-v", "--verbose", "--no-edit", "--autostash", "--all", "--tags"]);
            p.finish("`vcs sync` (fetch + fast-forward); `vcs merge --remote <branch>` for a real merge")?;
            if rebase {
                return Err(missing(
                    "pull --rebase",
                    "rebase onto a remote-tracking branch",
                    "`vcs fetch`, `vcs merge --remote <branch>` (or `vcs sync`, then `vcs rebase <branch>`)",
                ));
            }
            if p.pos.len() > 1 || p.pos.first().is_some_and(|r| r != "origin") {
                return Err(missing(cmd, "pulling a named branch", "`vcs sync` updates every branch it can fast-forward"));
            }
            vec![
                note("pull = fetch + fast-forward only; diverged branches are left alone (nothing is merged). Use `vcs merge --remote <branch>`."),
                Step::Vcs(v(&["sync"])),
            ]
        }
        "push" => {
            let mut p = parse(cmd, rest, &["--zone", "--receive-pack", "--repo"]);
            p.take(&["-u", "--set-upstream", "-q", "--quiet", "-v", "--verbose", "--all", "--follow-tags", "--no-verify"]);
            let zones = p.vals(&["--zone"]);
            if p.take(&["-f", "--force", "--force-with-lease", "--delete", "-d", "--tags", "--mirror", "--prune"]) {
                return Err(missing(
                    cmd,
                    "force/delete/tag pushes",
                    "`vcs publish` only fast-forwards; delete a remote branch with `vcs branch -d` then publish; tags travel with `vcs publish`",
                ));
            }
            p.finish("`vcs publish [--zone Z]`")?;
            let mut a = v(&["publish"]);
            for z in zones {
                a.extend(["--zone".to_owned(), z]);
            }
            let mut out = vec![];
            if p.pos.len() > 1 || p.pos.first().is_some_and(|r| r != "origin") {
                out.push(note("push publishes ALL checked-out branches atomically; the branch/refspec argument is ignored"));
            }
            out.push(Step::Vcs(a));
            out
        }
        "blame" | "annotate" => {
            let mut p = parse(cmd, rest, &["-L", "--rev"]);
            let m = p.take(&["-M"]);
            p.take(&["-C", "--show-name", "-f", "-n", "--show-number"]);
            p.finish("`vcs blame [--rev R] [-M] <file>` (no -L line ranges, -w, -e)")?;
            let pos = p.all_pos();
            let (rev, file) = match pos.as_slice() {
                [f] => (None, f.clone()),
                [r, f] => (Some(r.clone()), f.clone()),
                _ => return Err(Error::Msg("usage: git blame [<rev>] [--] <file>".into())),
            };
            let mut a = v(&["blame"]);
            if m {
                a.push("-M".into());
            }
            if let Some(r) = rev {
                a.extend(["--rev".to_owned(), r]);
            }
            a.push(file);
            vec![Step::Vcs(a)]
        }
        "bisect" => {
            let sub = rest.first().map(String::as_str).unwrap_or("");
            match sub {
                "start" | "good" | "bad" | "skip" | "reset" | "run" => passthrough("bisect", rest),
                "new" => passthrough("bisect", &[&["bad".to_owned()][..], &rest[1..]].concat()),
                "old" => passthrough("bisect", &[&["good".to_owned()][..], &rest[1..]].concat()),
                _ => return Err(missing(&format!("bisect {sub}"), "only start|good|bad|skip|reset|run exist", "`vcs bisect --help`")),
            }
        }
        "reflog" => {
            let mut p = parse(cmd, rest, &["-n", "--max-count"]);
            let n = p.val(&["-n", "--max-count"]).or_else(|| p.val(&["-N"]));
            p.finish("`vcs op log`")?;
            match p.pos.first().map(String::as_str) {
                None | Some("show") => {}
                Some(s) => return Err(missing(&format!("reflog {s}"), "the operation log is not expired by hand", "`vcs gc` removes unreachable objects")),
            }
            let mut a = v(&["op", "log"]);
            if let Some(n) = n {
                a.extend(["-n".to_owned(), n]);
            }
            vec![note("reflog = the repository-wide operation log (every ref/workspace move), not one log per ref"), Step::Vcs(a)]
        }
        "clean" => {
            let mut p = parse(cmd, rest, &["-e", "--exclude"]);
            let f = p.take(&["-f", "--force"]);
            let n = p.take(&["-n", "--dry-run"]);
            let d = p.take(&["-d"]);
            p.take(&["-q", "--quiet"]);
            p.finish("`vcs clean [-n|-f] [-d] [paths]` (no -x/-X: ignored files are never removed)")?;
            let mut a = v(&["clean"]);
            if n {
                a.push("-n".into());
            }
            if f {
                a.push("-f".into());
            }
            if d {
                a.push("-d".into());
            }
            a.extend(p.all_pos());
            vec![Step::Vcs(a)]
        }
        "grep" => {
            let mut p = parse(cmd, rest, &["-e", "--regexp"]);
            let mut a = v(&["grep"]);
            for (long, flags) in [
                ("-n", &["-n", "--line-number"][..]),
                ("-i", &["-i", "--ignore-case"][..]),
                ("-w", &["-w", "--word-regexp"][..]),
                ("-l", &["-l", "--files-with-matches", "--name-only"][..]),
                ("-c", &["-c", "--count"][..]),
                ("-F", &["-F", "--fixed-strings"][..]),
            ] {
                if p.take(flags) {
                    a.push(long.into());
                }
            }
            p.take(&["-I", "-H", "-r", "--recursive", "-E", "--extended-regexp", "--no-color", "--color"]);
            let pats = p.vals(&["-e", "--regexp"]);
            p.finish("`vcs grep [-n -i -w -l -c -F] <regex> [rev] [-- paths]` (Rust regex syntax; no --cached/--untracked/-v/-A/-B/-C)")?;
            let mut pos = p.pos.clone();
            let pat = if let Some(first) = pats.first() {
                first.clone()
            } else if pos.is_empty() {
                return Err(Error::Msg("usage: git grep <pattern> [<rev>] [-- <paths>]".into()));
            } else {
                pos.remove(0)
            };
            a.push(pat);
            a.extend(pos);
            if !p.dd.is_empty() {
                a.push("--".into());
                a.extend(p.dd.clone());
            }
            vec![Step::Vcs(a)]
        }
        "describe" => {
            let mut p = parse(cmd, rest, &["--abbrev"]);
            let tags = p.take(&["--tags"]);
            let always = p.take(&["--always"]);
            p.val(&["--abbrev"]);
            p.finish("`vcs describe [--tags] [--always] [rev]` (no --long/--dirty/--match/--contains)")?;
            let mut a = v(&["describe"]);
            if tags {
                a.push("--tags".into());
            }
            if always {
                a.push("--always".into());
            }
            a.extend(p.pos.first().cloned());
            vec![Step::Vcs(a)]
        }
        "archive" => {
            let mut p = parse(cmd, rest, &["-o", "--output", "--format", "--prefix", "--remote"]);
            let o = p.val(&["-o", "--output"]);
            let f = p.val(&["--format"]);
            let pre = p.val(&["--prefix"]);
            p.finish("`vcs archive [-o file] [--format tar|zip] [--prefix dir/] [rev]`")?;
            if p.pos.len() > 1 {
                return Err(missing(cmd, "a path list after the tree-ish", "`vcs archive` archives the whole revision"));
            }
            let mut a = v(&["archive"]);
            for (k, val) in [("-o", o), ("--format", f), ("--prefix", pre)] {
                if let Some(val) = val {
                    a.extend([k.to_owned(), val]);
                }
            }
            a.extend(p.pos.first().cloned());
            vec![Step::Vcs(a)]
        }
        "worktree" => {
            let sub = rest.first().map(String::as_str).unwrap_or("");
            let mut p = parse("worktree", rest.get(1..).unwrap_or(&[]), &["-b", "-B"]);
            match sub {
                "add" => {
                    let b = p.val(&["-b"]);
                    p.take(&["-q", "--quiet"]);
                    p.finish("`vcs worktree add <path> [branch]`")?;
                    let mut a = v(&["worktree", "add"]);
                    a.push(p.pos.first().cloned().ok_or_else(|| Error::Msg("usage: git worktree add [-b <branch>] <path> [<branch>]".into()))?);
                    a.extend(b.or_else(|| p.pos.get(1).cloned()));
                    vec![Step::Vcs(a)]
                }
                "list" => {
                    p.finish("`vcs worktree list`")?;
                    vec![Step::Vcs(v(&["worktree", "list"]))]
                }
                "remove" => {
                    let f = p.take(&["-f", "--force"]);
                    p.finish("`vcs worktree remove [--force] <path>`")?;
                    let mut a = v(&["worktree", "remove"]);
                    if f {
                        a.push("--force".into());
                    }
                    a.extend(p.pos.first().cloned());
                    vec![Step::Vcs(a)]
                }
                _ => return Err(missing(&format!("worktree {sub}"), "only add|list|remove exist", "`vcs worktree --help`")),
            }
        }
        "submodule" => {
            return Err(missing(
                "submodule",
                "vcs has no nested repositories; a zone is a path-prefix owned by one history in the SAME repository",
                "`vcs zone create <name> --path <dir>` (split an existing dir), `vcs zone add/remove <name>` (subscribe), `vcs clone --zone <name>` (partial clone); see docs/zones.md",
            ))
        }
        "sparse-checkout" => {
            return Err(missing(
                "sparse-checkout",
                "sparse sets are per zone",
                "`vcs sparse set <zone> <paths>`, `vcs sparse add|remove|list|disable`",
            ))
        }
        "config" => translate_config(rest)?,
        "ls-files" => {
            let mut p = parse(cmd, rest, &[]);
            let s = p.take(&["-s", "--stage"]);
            p.take(&["-c", "--cached"]);
            p.finish("`vcs ls-files [-s] [--rev R] [paths]` (tracked files only; no -o/-m/-d/-i/-z)")?;
            let mut a = v(&["ls-files"]);
            if s {
                a.push("-s".into());
            }
            a.extend(p.all_pos());
            vec![Step::Vcs(a)]
        }
        "ls-tree" => {
            let mut p = parse(cmd, rest, &[]);
            p.take(&["-r", "-t"]);
            let names = p.take(&["--name-only", "--name-status"]);
            p.finish("`vcs ls-files --rev <rev> [-s] [paths]`")?;
            let pos = p.all_pos();
            let Some((rev, paths)) = pos.split_first() else {
                return Err(Error::Msg("usage: git ls-tree [-r] [--name-only] <tree-ish> [<path>...]".into()));
            };
            let mut a = v(&["ls-files", "--rev", rev]);
            if !names {
                a.insert(1, "-s".into());
            }
            a.extend(paths.iter().cloned());
            vec![Step::Vcs(a)]
        }
        "cat-file" => {
            let mut p = parse(cmd, rest, &[]);
            let mut a = v(&["cat-file"]);
            for f in ["-t", "-s", "-p"] {
                if p.take(&[f]) {
                    a.push(f.into());
                }
            }
            p.finish("`vcs cat-file (-t|-s|-p) <object>` (no -e, --batch)")?;
            if p.pos.len() != 1 {
                return Err(missing(cmd, "the `<type> <object>` form", "`vcs cat-file -p <object>`"));
            }
            a.push(p.pos[0].clone());
            vec![Step::Vcs(a)]
        }
        "rev-parse" => {
            let mut p = parse(cmd, rest, &[]);
            if p.opts.iter().any(|(k, _)| k == "--abbrev-ref") || p.pos.iter().any(|x| x == "--abbrev-ref") {
                p.take(&["--abbrev-ref"]);
                p.finish("`vcs branch`")?;
                return Ok(vec![Step::ShowCurrent]);
            }
            let short = p.val(&["--short"]);
            let top = p.take(&["--show-toplevel"]);
            p.take(&["--verify", "-q", "--quiet"]);
            p.finish("`vcs rev-parse [--short] [--show-toplevel] <rev>...` (no --git-dir, --is-inside-work-tree)")?;
            let mut a = v(&["rev-parse"]);
            if short.is_some() {
                a.push("--short".into());
            }
            if top {
                a.push("--show-toplevel".into());
            }
            a.extend(p.pos.clone());
            vec![Step::Vcs(a)]
        }
        "symbolic-ref" => {
            let mut p = parse(cmd, rest, &[]);
            p.take(&["--short", "-q", "--quiet"]);
            p.finish("`vcs branch`")?;
            if p.pos.as_slice() != ["HEAD"] {
                return Err(missing(cmd, "only `symbolic-ref HEAD` can be answered", "`vcs branch`"));
            }
            vec![Step::ShowCurrent]
        }
        "rev-list" => {
            let rest = &fold_not(rest);
            let mut p = parse(cmd, rest, &["-n", "--max-count"]);
            let mut a = v(&["rev-list"]);
            if p.take(&["--count"]) {
                a.push("--count".into());
            }
            if p.take(&["--all"]) {
                a.push("--all".into());
            }
            if p.take(&["--reverse"]) {
                a.push("--reverse".into());
            }
            if let Some(n) = p.val(&["-n", "--max-count"]).or_else(|| p.val(&["-N"])) {
                a.extend(["-n".to_owned(), n]);
            }
            p.finish("`vcs rev-list [--count] [--all] [--reverse] [-n N] [revs]` (no --parents, --objects, --since, ...)")?;
            a.extend(p.all_pos().into_iter().filter(|t| t != "--"));
            vec![Step::Vcs(a)]
        }
        "shortlog" => {
            let rest = &fold_not(rest);
            let mut p = parse(cmd, rest, &[]);
            let mut a = v(&["shortlog"]);
            for (long, fl) in [("-s", &["-s", "--summary"][..]), ("-n", &["-n", "--numbered"][..]), ("-e", &["-e", "--email"][..])] {
                if p.take(fl) {
                    a.push(long.into());
                }
            }
            p.finish("`vcs shortlog [-s] [-n] [-e] [branch]`")?;
            a.extend(p.pos.first().cloned());
            vec![Step::Vcs(a)]
        }
        "merge-base" => {
            let mut p = parse(cmd, rest, &[]);
            let all = p.take(&["--all", "-a"]);
            p.finish("`vcs merge-base [--all] <a> <b>` (no --is-ancestor/--octopus)")?;
            if p.pos.len() != 2 {
                return Err(Error::Msg("usage: git merge-base <commit> <commit>".into()));
            }
            let mut a = v(&["merge-base"]);
            if all {
                a.push("--all".into());
            }
            a.extend(p.pos.clone());
            vec![Step::Vcs(a)]
        }
        "diff-tree" => {
            let mut p = parse(cmd, rest, &[]);
            p.take(&["-r", "--no-commit-id", "--name-status", "--root"]);
            p.finish("`vcs diff-tree <a> <b>` (prints A/D/M and path)")?;
            if p.pos.len() != 2 {
                return Err(Error::Msg("usage: git diff-tree [-r] <tree-ish> <tree-ish>".into()));
            }
            let mut a = v(&["diff-tree"]);
            a.extend(p.pos.clone());
            vec![Step::Vcs(a)]
        }
        "fast-export" => {
            let mut p = parse(cmd, rest, &["--output"]);
            let o = p.val(&["--output"]);
            p.finish("`vcs export-git [branch] [--output file]`")?;
            let mut a = v(&["export-git"]);
            a.extend(p.pos.first().cloned());
            if let Some(o) = o {
                a.extend(["--output".to_owned(), o]);
            }
            vec![note("the stream carries vcs history in git's fast-import syntax; commit ids on the git side differ"), Step::Vcs(a)]
        }
        "fast-import" => {
            let mut p = parse(cmd, rest, &[]);
            p.take(&["--quiet"]);
            p.finish("`vcs import-git <file|->`")?;
            vec![Step::Vcs(v(&["import-git", "-"]))]
        }
        "bundle" => match rest.first().map(String::as_str) {
            Some("create") => {
                let mut p = parse("bundle", &rest[1..], &[]);
                p.take(&["-q", "--quiet"]);
                p.finish("`vcs bundle create <file> [branches/zones]`")?;
                let mut a = v(&["bundle", "create"]);
                a.extend(p.pos.clone());
                vec![Step::Vcs(a)]
            }
            Some("verify") => passthrough("bundle", rest),
            Some("unbundle") if rest.len() == 2 => vec![Step::Vcs(v(&["fetch", &rest[1]]))],
            _ => return Err(missing("bundle", "only create|verify|unbundle exist", "`vcs bundle --help`")),
        },
        "gc" | "maintenance" => {
            let mut p = parse(cmd, rest, &["--prune"]);
            let auto = p.take(&["--auto"]);
            p.take(&["--aggressive", "-q", "--quiet", "--no-prune", "run"]);
            p.val(&["--prune"]);
            p.finish("`vcs gc [--dry-run]`")?;
            if auto {
                vec![Step::Vcs(v(&["repack", "--auto"]))]
            } else {
                vec![note("vcs gc deletes unreachable objects older than a 14-day grace period; add --dry-run to preview"), Step::Vcs(v(&["gc"]))]
            }
        }
        "repack" => {
            let mut p = parse(cmd, rest, &["--window", "--depth"]);
            p.take(&["-a", "-d", "-q", "-f", "-F", "-l"]);
            p.val(&["--window", "--depth"]);
            p.finish("`vcs repack`")?;
            vec![Step::Vcs(v(&["repack"]))]
        }
        "fsck" => {
            let mut p = parse(cmd, rest, &[]);
            p.take(&["--full", "--strict", "--no-progress"]);
            p.finish("`vcs fsck`")?;
            vec![Step::Vcs(v(&["fsck"]))]
        }
        "verify-commit" | "verify-tag" => passthrough(cmd, rest),
        "mergetool" => return Err(missing(cmd, "no external merge tool launcher", "`vcs conflict show <path>`, edit, then `vcs resolve --mark <path>` (or --ours/--theirs/--base/--file)")),
        "credential" => passthrough("credential", rest),
        "format-patch" | "am" | "apply" | "send-email" | "request-pull" => {
            return Err(missing(
                cmd,
                "vcs has no patch-file format",
                "exchange commits with `vcs bundle create/verify` + `vcs fetch <file>`, or a shared remote with `vcs publish`/`vcs fetch`; plain diffs with `vcs diff` (cannot be applied back)",
            ))
        }
        "lfs" => return Err(missing(cmd, "large files are native", "files are chunked automatically; see docs/guide/06-large-files.md")),
        "filter-branch" | "filter-repo" | "replace" | "notes" | "rerere" | "range-diff" | "show-branch" | "stripspace" | "cherry" => {
            return Err(missing(cmd, "history rewriting and annotation tools are not implemented", "`vcs rebase`, `vcs op undo`, `vcs zone apply` (splits history by path)"))
        }
        "ls-remote" | "for-each-ref" | "show-ref" | "name-rev" | "update-ref" | "update-index" | "hash-object" | "write-tree" | "read-tree" | "commit-tree" | "count-objects" | "prune" | "pack-objects" | "index-pack" | "unpack-objects" | "check-ignore" | "check-attr" | "var" | "mktag" => {
            return Err(missing(
                cmd,
                "git plumbing with no vcs counterpart",
                "`vcs branch`, `vcs tag`, `vcs rev-parse`, `vcs cat-file`, `vcs ls-files`, `vcs remote show`, `vcs fsck`, `vcs gc --dry-run`",
            ))
        }
        other => {
            return Err(Error::Msg(format!(
                "git {other}: unknown to `vcs git`; run `vcs git --map` for the supported commands or `vcs --help` for native ones"
            )))
        }
    })
}

// ---------------------------------------------------------------- diff

const DIFF_VALUED: &[&str] = &[
    "-U",
    "--unified",
    "--diff-algorithm",
    "--color",
    "--find-renames",
    "--find-copies",
    "--src-prefix",
    "--dst-prefix",
];

/// Shared diff/show flags in vcs spelling.
fn diff_flags(p: &mut P) -> Result<Vec<String>, Error> {
    let mut f: Vec<String> = vec![];
    for (name, fl) in [
        ("--stat", &["--stat"][..]),
        ("--name-only", &["--name-only"][..]),
        ("--name-status", &["--name-status"][..]),
        ("--word-diff", &["--word-diff"][..]),
        ("--color-moved", &["--color-moved"][..]),
        ("-w", &["-w", "--ignore-all-space"][..]),
        ("--find-copies", &["--find-copies"][..]),
    ] {
        if p.take(fl) {
            f.push(name.into());
        }
    }
    if p.take(&["-M"]) {
        f.push("-M".into());
    }
    if let Some(t) = p.val(&["--find-renames"]) {
        f.push("-M".into());
        if !t.is_empty() {
            f.extend(["--rename-threshold".to_owned(), t.trim_end_matches('%').to_owned()]);
        }
    }
    if let Some(u) = p.val(&["-U", "--unified"]) {
        f.extend(["-U".to_owned(), u]);
    }
    if let Some(alg) = p.val(&["--diff-algorithm"]) {
        f.push(format!("--diff-algorithm={alg}"));
    }
    for alg in ["patience", "histogram", "minimal"] {
        if p.take(&[&format!("--{alg}")]) {
            f.push(format!("--diff-algorithm={}", if alg == "minimal" { "myers" } else { alg }));
        }
    }
    if let Some(c) = p.val(&["--color"]) {
        f.push(if c.is_empty() { "--color".into() } else { format!("--color={c}") });
    }
    if p.take(&["--no-color"]) {
        f.push("--color=never".into());
    }
    p.take(&[
        "-p",
        "-u",
        "--patch",
        "--no-ext-diff",
        "--no-textconv",
        "--no-prefix",
        "--minimal",
        "--full-index",
        "--binary",
        "--ext-diff",
    ]);
    Ok(f)
}

fn translate_diff(cmd: &str, rest: &[String]) -> Result<Vec<Step>, Error> {
    let mut p = parse(cmd, rest, DIFF_VALUED);
    let staged = p.take(&["--staged", "--cached"]);
    let flags = diff_flags(&mut p)?;
    p.finish("`vcs diff [--staged] [--stat|--name-only|--name-status|-U<n>|-w|-M|--word-diff|...]`; `vcs git diff A B` also works (no --check/--exit-code/--numstat/--shortstat/-R)")?;
    let mut specs: Vec<String> = vec![];
    for s in &p.pos {
        if s.contains("...") {
            specs.push(s.clone()); // resolved at run time: merge-base(A, B) against B
            continue;
        }
        if let Some((a, b)) = s.split_once("..") {
            specs.push(if a.is_empty() { "HEAD".into() } else { a.into() });
            specs.push(if b.is_empty() { "HEAD".into() } else { b.into() });
        } else {
            specs.push(s.clone());
        }
    }
    let paths_after = !p.dd.is_empty();
    if specs.is_empty() && !paths_after {
        let mut a = vec!["diff".to_owned()];
        if staged {
            a.push("--staged".into());
        }
        a.extend(flags);
        return Ok(vec![Step::Vcs(a)]);
    }
    // marker between revisions/paths for the runtime resolver
    let mut all = specs;
    if paths_after {
        all.push("--".into());
        all.extend(p.dd.clone());
    }
    Ok(vec![Step::Diff { staged, specs: all, flags }])
}

// ---------------------------------------------------------------- commit

fn translate_commit(rest: &[String]) -> Result<Vec<Step>, Error> {
    let mut p = parse(
        "commit",
        rest,
        &[
            "-m",
            "--message",
            "-F",
            "--file",
            "--author",
            "--date",
            "-C",
            "-c",
            "--fixup",
            "--squash",
            "--cleanup",
            "-t",
            "--template",
        ],
    );
    let msgs = p.vals(&["-m", "--message"]);
    let file = p.val(&["-F", "--file"]);
    let all = p.take(&["-a", "--all"]);
    let amend = p.take(&["--amend"]);
    p.take(&["--no-edit", "-q", "--quiet", "-v", "--verbose", "-e", "--edit"]);
    let no_verify = p.take(&["-n", "--no-verify"]);
    let nosign = p.take(&["--no-gpg-sign"]);
    // `-S` takes an optional key; as a bare flag git's `-S` consumed nothing, but `parse` may have eaten the next token
    let sign = if p.take(&["-S"]) {
        Some(String::new())
    } else {
        p.val(&["--gpg-sign"])
    };
    p.finish("`vcs commit [-m msg] [-S]`; `vcs git commit -a/--amend` also work (no --allow-empty/--fixup/--author/--date/-s/-C)")?;
    let mut message = if msgs.is_empty() { None } else { Some(msgs.join("\n\n")) };
    if let Some(f) = file {
        message = Some(std::fs::read_to_string(&f).map_err(|source| Error::Io { path: f.into(), source })?);
    }
    let mut commit = vec!["commit".to_owned()];
    if no_verify {
        commit.insert(0, "--no-hooks".into());
    }
    if let Some(k) = sign {
        // a bare -S followed by a positional would have consumed it as the key; restore it as positional
        if k.is_empty() {
            commit.push("-S".into());
        } else if std::path::Path::new(&k).is_file() {
            commit.extend(["-S".to_owned(), k]);
        } else {
            return Err(Error::Msg(format!(
                "git commit -S{k}: vcs signs with an Ed25519 OpenSSH key file; pass the key path (`-S <file>`) or set `user.signingkey`"
            )));
        }
    }
    if nosign {
        commit.push("--no-sign".into());
    }
    let mut steps = vec![];
    if all {
        steps.push(Step::AddTracked(vec![]));
    }
    if amend {
        steps.push(note(
            "amend writes a NEW commit in place of the old tip (same parents); the old tip stays reachable only through `vcs op log`",
        ));
        steps.push(Step::Amend {
            commit_argv: commit,
            message,
        });
    } else {
        if let Some(m) = message {
            commit.extend(["-m".to_owned(), m]);
        }
        steps.push(Step::Vcs(commit));
    }
    Ok(steps)
}

// ---------------------------------------------------------------- log

/// `--not A B` flips every revision after it (up to `--`) into `^A ^B`, as git does.
fn fold_not(rest: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(rest.len());
    let (mut not, mut dd) = (false, false);
    for t in rest {
        if dd {
            out.push(t.clone());
        } else if t == "--" {
            dd = true;
            out.push(t.clone());
        } else if t == "--not" {
            not = !not;
        } else if not && !t.starts_with('-') {
            out.push(match t.strip_prefix('^') {
                Some(r) => r.to_owned(),
                None => format!("^{t}"),
            });
        } else {
            out.push(t.clone());
        }
    }
    out
}

fn translate_log(rest: &[String]) -> Result<Vec<Step>, Error> {
    let rest = &fold_not(rest);
    let mut p = parse(
        "log",
        rest,
        &[
            "-n",
            "--max-count",
            "--author",
            "--since",
            "--after",
            "--until",
            "--before",
            "--grep",
            "-S",
            "--format",
            "--pretty",
            "-G",
            "--skip",
            "--date",
            "-M",
            "--diff-filter",
            "--find-renames",
            "--color",
        ],
    );
    let mut a = v(&["log"]);
    let mut n = p.val(&["-n", "--max-count"]).or_else(|| p.val(&["-N"]));
    if n.is_none() {
        n = Some("1000000000".into()); // git has no default limit
    }
    if let Some(n) = n {
        a.extend(["-n".to_owned(), n]);
    }
    let patch = p.take(&["-p", "-u", "--patch"]);
    let stat = p.take(&["--stat"]);
    if p.take(&["--oneline"]) {
        a.push("--oneline".into());
    }
    for (long, fl) in [
        ("--graph", &["--graph"][..]),
        ("--merges", &["--merges"][..]),
        ("--no-merges", &["--no-merges"][..]),
        ("--first-parent", &["--first-parent"][..]),
        ("--reverse", &["--reverse"][..]),
        ("--all", &["--all"][..]),
        ("--follow", &["--follow"][..]),
        ("--name-status", &["--name-status"][..]),
        ("--name-only", &["--name-only"][..]),
        ("--show-signature", &["--show-signature"][..]),
        ("--find-copies", &["--find-copies", "-C"][..]),
    ] {
        if p.take(fl) {
            a.push(long.into());
        }
    }
    p.take(&[
        "--abbrev-commit",
        "--decorate",
        "--no-decorate",
        "--no-color",
        "--no-patch",
        "-s",
        "--source",
        "--no-abbrev-commit",
    ]);
    if p.take(&["-M"]) || p.val(&["--find-renames"]).is_some() {
        a.push("-M".into());
    }
    for (long, fl) in [("--author", &["--author"][..]), ("--grep", &["--grep"][..]), ("-S", &["-S"][..])] {
        if let Some(x) = p.val(fl) {
            a.extend([long.to_owned(), x]);
        }
    }
    if let Some(x) = p.val(&["--since", "--after"]) {
        a.extend(["--since".to_owned(), x]);
    }
    if let Some(x) = p.val(&["--until", "--before"]) {
        a.extend(["--until".to_owned(), x]);
    }
    if let Some(c) = p.val(&["--color"]) {
        a.push(format!("--color={}", if c.is_empty() { "always" } else { &c }));
    }
    if let Some(f) = p.val(&["--format", "--pretty"]) {
        match f.as_str() {
            "oneline" => a.push("--oneline".into()),
            "medium" | "" => {}
            _ => {
                let t = f.strip_prefix("format:").or_else(|| f.strip_prefix("tformat:")).unwrap_or(&f);
                if matches!(t, "short" | "full" | "fuller" | "reference" | "email" | "raw") {
                    return Err(missing(
                        "log --pretty",
                        &format!("the {t} layout"),
                        "`vcs log --format '<template>'` (placeholders %H %h %an %ae %at %ad %s %b)",
                    ));
                }
                a.push(format!("--format={t}"));
            }
        }
    }
    if p.opts.iter().any(|(k, _)| k == "-G") {
        return Err(missing(
            "log -G",
            "regex pickaxe",
            "`vcs log -S <string>` (count-change pickaxe) or `vcs grep <regex> <rev>`",
        ));
    }
    p.finish(
        "`vcs log [branch] [-n N] [--oneline|--graph|--author|--since|--until|--grep|-S|--format|--follow|--name-status] [-- paths]`",
    )?;
    // positionals: revisions (a branch, `A..B`, `A...B`, `^A`), or (heuristically) existing paths without `--`
    let mut paths = p.dd.clone();
    let mut revs: Vec<String> = vec![];
    for t in &p.pos {
        let range = vcs_repo::RevSpec::is_range_token(t);
        if !p.had_dd && !range && std::path::Path::new(t).exists() {
            paths.push(t.clone());
        } else {
            revs.push(t.clone());
        }
    }
    if patch || stat {
        let mut la = a.clone();
        la.extend(revs);
        if !paths.is_empty() {
            la.push("--".into());
            la.extend(paths);
        }
        return Ok(vec![Step::LogPatch {
            log_argv: la,
            stat: stat && !patch || stat,
        }]);
    }
    a.extend(revs);
    if !paths.is_empty() {
        a.push("--".into());
        a.extend(paths);
    }
    Ok(vec![Step::Vcs(a)])
}

// ---------------------------------------------------------------- branch / checkout / restore / reset

fn translate_branch(rest: &[String]) -> Result<Vec<Step>, Error> {
    let mut p = parse(
        "branch",
        rest,
        &[
            "--contains",
            "--merged",
            "--no-merged",
            "-u",
            "--set-upstream-to",
            "--points-at",
            "--sort",
            "--format",
        ],
    );
    p.take(&["-v", "-vv", "--verbose", "-a", "--all", "--list", "-l", "--no-color", "--color"]);
    if p.take(&["--show-current"]) {
        p.finish("`vcs branch`")?;
        return Ok(vec![Step::ShowCurrent]);
    }
    let del = p.take(&["-d", "--delete"]);
    let del_force = p.take(&["-D"]);
    let rename = p.take(&["-m", "--move"]);
    let force_rename = p.take(&["-M"]);
    if p.take(&["-c", "-C", "--copy"]) {
        return Err(missing(
            "branch -c",
            "copying a branch",
            "`vcs branch <new> <old>` creates a branch at another branch's head",
        ));
    }
    if rename || force_rename {
        p.finish("`vcs branch -m [<old>] <new>` / `vcs branch -M <old> <new>`")?;
        if p.pos.is_empty() || p.pos.len() > 2 {
            return Err(Error::Msg("usage: git branch -m [<old>] <new>".into()));
        }
        let mut a = v(&["branch", if force_rename { "-M" } else { "-m" }]);
        a.extend(p.pos.iter().cloned());
        return Ok(vec![Step::Vcs(a)]);
    }
    if p.take(&["-r", "--remotes"]) {
        return Err(missing(
            "branch -r",
            "listing remote-tracking branches",
            "`vcs remote show` lists them",
        ));
    }
    let force = p.take(&["-f", "--force"]);
    p.finish("`vcs branch [name]` / `vcs branch -d [--force] <name>` (no --merged/--contains/-u)")?;
    if del || del_force {
        if p.pos.is_empty() {
            return Err(Error::Msg("usage: git branch -d <branch>...".into()));
        }
        return Ok(p
            .pos
            .iter()
            .map(|b| {
                let mut a = v(&["branch", "-d"]);
                if del_force {
                    a.push("--force".into());
                }
                a.push(b.clone());
                Step::Vcs(a)
            })
            .collect());
    }
    let _ = force;
    match p.pos.as_slice() {
        [] => Ok(vec![Step::Vcs(v(&["branch"]))]),
        [n] => Ok(vec![Step::Vcs(v(&["branch", n]))]),
        [n, s] if s == "HEAD" => Ok(vec![Step::Vcs(v(&["branch", n]))]),
        [n, s] => Ok(vec![Step::Vcs(v(&["branch", n, s]))]),
        _ => Err(Error::Msg("usage: git branch <name> [<start-point>]".into())),
    }
}

fn translate_checkout(rest: &[String]) -> Result<Vec<Step>, Error> {
    let mut p = parse("checkout", rest, &["-b", "-B", "--orphan"]);
    p.take(&["-q", "--quiet", "--progress", "--guess", "--no-guess", "-t", "--track"]);
    if p.take(&["--ours"]) {
        let mut a = v(&["resolve", "--ours"]);
        a.extend(p.all_pos());
        return Ok(vec![Step::Vcs(a)]);
    }
    if p.take(&["--theirs"]) {
        let mut a = v(&["resolve", "--theirs"]);
        a.extend(p.all_pos());
        return Ok(vec![Step::Vcs(a)]);
    }
    if let Some(b) = p.val(&["-b"]) {
        p.finish("`vcs switch -c <branch>`")?;
        return match p.pos.as_slice() {
            [] => Ok(vec![Step::Vcs(v(&["switch", "-c", &b]))]),
            [s] if s == "HEAD" => Ok(vec![Step::Vcs(v(&["switch", "-c", &b]))]),
            [s] => Ok(vec![Step::Vcs(v(&["branch", &b, s])), Step::Vcs(v(&["switch", &b]))]),
            _ => Err(Error::Msg("usage: git checkout -b <new> [<start-point>]".into())),
        };
    }
    if p.take(&["--detach", "-d"]) {
        p.finish("`vcs switch --detach <rev>`")?;
        return match p.pos.as_slice() {
            [] => Ok(vec![Step::Vcs(v(&["switch", "--detach", "HEAD"]))]),
            [r] => Ok(vec![Step::Vcs(v(&["switch", "--detach", r]))]),
            _ => Err(Error::Msg("usage: git checkout --detach [<commit>]".into())),
        };
    }
    if p.take(&["-f", "--force", "-B", "--orphan", "-m", "--merge", "-p", "--patch"]) {
        return Err(missing(
            "checkout",
            "forced, merging, orphan or interactive checkout",
            "`vcs switch <branch>`; discard changes with `vcs restore .`; `vcs worktree add <dir> <branch>` for a second checkout",
        ));
    }
    p.finish("`vcs switch <branch>` / `vcs restore <paths>`")?;
    if p.had_dd {
        // checkout [<rev>] -- paths
        return Ok(match p.pos.first() {
            Some(r) if r != "HEAD" => vec![Step::FromRev {
                rev: r.clone(),
                paths: p.dd.clone(),
                stage: true,
            }],
            Some(_) | None => {
                let mut a = v(&["restore"]);
                a.extend(p.dd.clone());
                vec![Step::Vcs(a)]
            }
        });
    }
    match p.pos.as_slice() {
        [] => Err(Error::Msg("usage: git checkout <branch> | git checkout [<rev>] -- <paths>".into())),
        [t] => Ok(vec![Step::CheckoutAuto {
            target: t.clone(),
            paths: vec![],
        }]),
        [r, rest @ ..] => Ok(vec![Step::FromRev {
            rev: r.clone(),
            paths: rest.to_vec(),
            stage: true,
        }]),
    }
}

fn translate_restore(rest: &[String]) -> Result<Vec<Step>, Error> {
    let mut p = parse("restore", rest, &["-s", "--source"]);
    let staged = p.take(&["-S", "--staged"]);
    let worktree = p.take(&["-W", "--worktree"]);
    let src = p.val(&["-s", "--source"]);
    p.take(&["-q", "--quiet", "--progress"]);
    if p.take(&["--ours"]) {
        let mut a = v(&["resolve", "--ours"]);
        a.extend(p.all_pos());
        return Ok(vec![Step::Vcs(a)]);
    }
    if p.take(&["--theirs"]) {
        let mut a = v(&["resolve", "--theirs"]);
        a.extend(p.all_pos());
        return Ok(vec![Step::Vcs(a)]);
    }
    p.finish("`vcs restore [--staged] <paths>` (no -p, --ignore-unmerged)")?;
    let paths = p.all_pos();
    if paths.is_empty() {
        return Err(Error::Msg("you must specify path(s) to restore".into()));
    }
    if let Some(r) = src.filter(|r| r != "HEAD" || !staged) {
        if staged {
            return Err(missing(
                "restore --source=<rev> --staged",
                "index entries from another revision",
                "`vcs git restore --source=<rev> <paths>` then `vcs add <paths>`",
            ));
        }
        return Ok(vec![Step::FromRev {
            rev: r,
            paths,
            stage: false,
        }]);
    }
    let mut out = vec![];
    if staged {
        let mut a = v(&["restore", "--staged"]);
        a.extend(paths.clone());
        out.push(Step::Vcs(a));
    }
    if !staged || worktree {
        let mut a = v(&["restore"]);
        a.extend(paths);
        out.push(Step::Vcs(a));
    }
    Ok(out)
}

fn translate_reset(rest: &[String]) -> Result<Vec<Step>, Error> {
    let mut p = parse("reset", rest, &[]);
    let mut a = v(&["reset"]);
    for m in ["soft", "mixed", "hard"] {
        if p.take(&[&format!("--{m}")]) {
            a.push(format!("--{m}"));
        }
    }
    p.take(&["-q", "--quiet", "-N"]);
    if p.take(&["--keep", "--merge", "-p", "--patch"]) {
        return Err(missing(
            "reset",
            "--keep/--merge/--patch",
            "`vcs reset --soft|--mixed|--hard <rev>`",
        ));
    }
    p.finish("`vcs reset [--soft|--mixed|--hard] [rev] [-- paths]`")?;
    let (rev, paths): (Option<String>, Vec<String>) = if p.had_dd {
        (p.pos.first().cloned(), p.dd.clone())
    } else {
        match p.pos.as_slice() {
            [] => (None, vec![]),
            [r] => (Some(r.clone()), vec![]),
            [r, rest @ ..] => (Some(r.clone()), rest.to_vec()),
        }
    };
    if let Some(r) = &rev {
        if r.contains("@{") || r == "ORIG_HEAD" || r == "FETCH_HEAD" {
            return Err(missing(
                "reset",
                "reflog revisions (HEAD@{n}, ORIG_HEAD)",
                "`vcs op log`, then `vcs op undo` or `vcs op restore <id>`",
            ));
        }
        a.push(r.clone());
    }
    if !paths.is_empty() {
        a.push("--".into());
        a.extend(paths);
    }
    let mut out = vec![];
    if a.iter().any(|x| x == "--hard") {
        out.push(note("reset acts on ONE zone (the one containing the current directory; use --zone Z for another) and --hard discards its uncommitted work"));
    } else if !a.iter().any(|x| x == "--") {
        out.push(note(
            "reset acts on ONE zone (the one containing the current directory; use --zone Z for another)",
        ));
    }
    out.push(Step::Vcs(a));
    Ok(out)
}

fn translate_stash(rest: &[String]) -> Result<Vec<Step>, Error> {
    let sub = rest.first().map(String::as_str).filter(|s| !s.starts_with('-'));
    let args = if sub.is_some() { &rest[1..] } else { rest };
    let mut p = parse("stash", args, &["-m", "--message"]);
    let m = p.val(&["-m", "--message"]);
    p.take(&["-q", "--quiet", "--index"]);
    if p.take(&["-u", "--include-untracked", "-a", "--all", "-k", "--keep-index", "-p", "--patch"]) {
        return Err(missing(
            "stash -u/-a/-k/-p",
            "stashing untracked files, or partially",
            "`vcs stash push` (tracked changes only; untracked files stay in place)",
        ));
    }
    p.finish("`vcs stash push [-m msg]` / `pop` / `apply` / `list` / `drop`")?;
    Ok(match sub {
        None | Some("push" | "save") => {
            let mut a = v(&["stash", "push"]);
            let msg = m.or_else(|| p.pos.first().cloned());
            if let Some(msg) = msg {
                a.extend(["-m".to_owned(), msg]);
            }
            vec![Step::Vcs(a)]
        }
        Some(s @ ("pop" | "apply" | "drop")) => {
            let mut a = v(&["stash", s]);
            if let Some(i) = p.pos.first() {
                a.push(stash_index(i)?);
            }
            vec![Step::Vcs(a)]
        }
        Some("list") => vec![Step::Vcs(v(&["stash", "list"]))],
        Some(other) => {
            return Err(missing(
                &format!("stash {other}"),
                "only push|pop|apply|list|drop exist",
                "`vcs stash list`, then `vcs stash pop N`; to inspect a stash entry apply it with `vcs stash apply N`",
            ))
        }
    })
}

fn translate_tag(rest: &[String]) -> Result<Vec<Step>, Error> {
    let mut p = parse(
        "tag",
        rest,
        &["-m", "--message", "-F", "-u", "--sort", "--contains", "--points-at", "-n"],
    );
    let m = p.val(&["-m", "--message"]);
    let annotate = p.take(&["-a", "--annotate"]);
    let sign = p.take(&["-s", "--sign"]);
    let del = p.take(&["-d", "--delete"]);
    let verify = p.take(&["-v", "--verify"]);
    let list = p.take(&["-l", "--list"]);
    p.take(&["--no-column", "-i", "--ignore-case"]);
    if p.take(&["-f", "--force"]) {
        return Err(missing(
            "tag -f",
            "moving an existing tag",
            "`vcs tag -d <name>` then `vcs tag <name> [rev]`",
        ));
    }
    p.finish("`vcs tag [name [rev]] [-m msg] [-s] [-d name]` (no --contains/--points-at/--sort)")?;
    if verify {
        let mut a = v(&["verify-tag"]);
        a.extend(p.pos.clone());
        return Ok(vec![Step::Vcs(a)]);
    }
    if del {
        return Ok(p.pos.iter().map(|t| Step::Vcs(v(&["tag", "-d", t]))).collect());
    }
    if list || p.pos.is_empty() {
        if !p.pos.is_empty() {
            return Err(missing("tag -l <pattern>", "pattern filtering", "`vcs tag` lists all tags"));
        }
        return Ok(vec![Step::Vcs(v(&["tag"]))]);
    }
    let mut a = v(&["tag"]);
    let annotated = m.is_some() || sign;
    if annotate && m.is_none() && !sign {
        return Err(missing(
            "tag -a without -m",
            "the editor prompt for a tag message",
            "`vcs tag <name> -m <message>`",
        ));
    }
    a.extend(p.pos.iter().take(2).cloned());
    if let Some(m) = m {
        a.extend(["-m".to_owned(), m]);
    }
    if sign {
        a.push("-s".into());
    }
    let _ = annotated;
    Ok(vec![Step::Vcs(a)])
}

fn translate_config(rest: &[String]) -> Result<Vec<Step>, Error> {
    let mut p = parse("config", rest, &["--get", "--unset", "--file", "-f", "--type"]);
    let user = p.take(&["--global", "--user"]);
    p.take(&["--local", "--show-origin", "--includes", "--no-includes"]);
    if p.take(&[
        "--system",
        "--worktree",
        "-e",
        "--edit",
        "--add",
        "--get-all",
        "--replace-all",
        "--unset-all",
        "--rename-section",
        "--remove-section",
    ]) {
        return Err(missing(
            "config",
            "system scope, editing and multi-value keys",
            "`vcs config get|set|unset|list [--user]`",
        ));
    }
    let get = p.val(&["--get"]);
    let unset = p.val(&["--unset"]);
    let list = p.take(&["-l", "--list"]);
    p.take(&["--bool", "--int", "--path", "-z"]);
    p.val(&["--type"]);
    p.finish("`vcs config get|set|unset|list [--user]`")?;
    let u = |mut a: Vec<String>| {
        if user {
            a.push("--user".into());
        }
        a
    };
    Ok(vec![Step::Vcs(if list {
        v(&["config", "list"])
    } else if let Some(k) = get {
        v(&["config", "get", &k])
    } else if let Some(k) = unset {
        u(v(&["config", "unset", &k]))
    } else {
        match p.pos.as_slice() {
            [k] => v(&["config", "get", k]),
            [k, val] => u(v(&["config", "set", k, val])),
            _ => {
                return Err(Error::Msg(
                    "usage: git config [--global] <key> [<value>] | --get <key> | --unset <key> | --list".into(),
                ))
            }
        }
    })])
}

// ---------------------------------------------------------------- running

fn quiet() -> bool {
    std::env::var("VCS_GIT_QUIET").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Run one native argv in this process (same path as a real `vcs` command line).
pub fn native(argv: &[String], json: bool) -> Result<(), Error> {
    let mut full = vec!["vcs".to_owned()];
    if json {
        full.push("--json".into());
    }
    full.extend(argv.iter().cloned());
    let cli = crate::Cli::try_parse_from(full).map_err(|e| Error::Msg(e.to_string().trim_end().to_owned()))?;
    let prev_hooks = cli.no_hooks;
    if prev_hooks {
        crate::hooks::set_disabled(true);
    }
    let refresh = crate::lockable_cmd::refresh_target(&cli.cmd);
    let r = crate::run_lazy(cli);
    if let Some(d) = refresh {
        crate::lockable_cmd::refresh(&d);
    }
    if prev_hooks {
        crate::hooks::set_disabled(false);
    }
    r
}

pub fn run(args: &[String], json: bool) -> Result<(), Error> {
    if args.first().map(String::as_str) == Some("--map") {
        print!("{}", table::markdown());
        return Ok(());
    }
    let steps = translate(args)?;
    for s in &steps {
        exec(s, json)?;
    }
    Ok(())
}

fn exec(s: &Step, json: bool) -> Result<(), Error> {
    match s {
        Step::Note(n) => {
            if !quiet() {
                eprintln!("vcs git: note: {n}");
            }
            Ok(())
        }
        Step::Vcs(a) => native(a, json),
        Step::AddTracked(paths) => add_tracked(paths),
        Step::Amend { commit_argv, message } => amend(commit_argv, message.clone()),
        Step::StatusShort { branch } => status_short(*branch),
        Step::ShowCurrent => {
            let (repo, cwd) = open()?;
            let zone = zone_of_cwd(&repo, &cwd, None)?;
            match repo.current_branch(&zone)? {
                Some(b) => println!("{b}"),
                None if repo.detached_head(&zone)?.is_some() => println!("HEAD"),
                None => return Err(Error::Msg("no branch is checked out in this zone".into())),
            }
            Ok(())
        }
        Step::Diff { staged, specs, flags } => diff_specs(*staged, specs, flags),
        Step::LogPatch { log_argv, stat } => log_patch(log_argv, *stat),
        Step::FromRev { rev, paths, stage } => from_rev(rev, paths, *stage),
        Step::CheckoutAuto { target, paths } => checkout_auto(target, paths),
    }
}

fn add_tracked(paths: &[String]) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let st = repo.status_auto(Default::default())?;
    let filters: Vec<String> = paths.iter().map(|p| repo.rel_path(&cwd, p)).collect::<Result<_, _>>()?;
    let mut rel: Vec<String> = vec![];
    for z in &st.zones {
        for c in &z.unstaged {
            if filters.is_empty()
                || filters
                    .iter()
                    .any(|f| f.is_empty() || c.path == *f || c.path.starts_with(&format!("{f}/")))
            {
                rel.push(c.path.clone());
            }
        }
    }
    if rel.is_empty() {
        return Ok(());
    }
    crate::lockable_cmd::guard(&repo, crate::lockable_cmd::Stage::Add, &rel, false)?;
    let rep = repo.add(&rel)?;
    println!("staged {} file(s), {} removal(s)", rep.staged, rep.removed);
    Ok(())
}

fn amend(commit_argv: &[String], message: Option<String>) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = zone_of_cwd(&repo, &cwd, None)?;
    let tip = repo
        .head_commit(&zone)?
        .ok_or_else(|| Error::Msg(format!("zone {zone:?} has no commit to amend")))?;
    let c = repo.commit_of(&tip)?;
    let Some(&parent) = c.parents.first() else {
        return Err(Error::Msg(
            "cannot amend the first commit of a zone; `vcs reset --soft` has no parent to move to".into(),
        ));
    };
    if c.parents.len() > 1 {
        return Err(Error::Msg("cannot amend a merge commit".into()));
    }
    let msg = message.unwrap_or_else(|| vcs_objects::signing::strip_commit_signature(&c.message).to_string());
    repo.reset(&zone, &parent, vcs_repo::ResetMode::Soft)?;
    let mut argv = commit_argv.to_vec();
    argv.extend(["-m".to_owned(), msg]);
    match native(&argv, false) {
        Ok(()) => Ok(()),
        Err(e) => {
            // put the old tip back so a failed amend changes nothing
            let _ = repo.reset(&zone, &tip, vcs_repo::ResetMode::Soft);
            Err(e)
        }
    }
}

fn status_short(branch: bool) -> Result<(), Error> {
    use vcs_repo::ChangeKind::{Added, Deleted, Modified};
    let (repo, _) = open()?;
    let st = repo.status_auto(Default::default())?;
    let letter = |k| match k {
        Added => 'A',
        Modified => 'M',
        Deleted => 'D',
    };
    if branch {
        if let Some(z) = st.zones.first() {
            println!("## {}", z.branch);
        }
    }
    let mut rows: std::collections::BTreeMap<String, (char, char)> = Default::default();
    for z in &st.zones {
        for c in &z.staged {
            rows.entry(c.path.clone()).or_insert((' ', ' ')).0 = letter(c.kind);
        }
        for c in &z.unstaged {
            rows.entry(c.path.clone()).or_insert((' ', ' ')).1 = letter(c.kind);
        }
    }
    if let Some(m) = &st.merging {
        for c in m.conflicts.iter().filter(|c| !c.resolved) {
            rows.insert(c.path.clone(), ('U', 'U'));
        }
    }
    for (p, (x, y)) in rows {
        println!("{x}{y} {p}");
    }
    for u in &st.untracked {
        println!("?? {u}");
    }
    Ok(())
}

fn flat_pairs(
    repo: &vcs_repo::Repo,
    old: &vcs_repo::treeops::Flat,
    new: &vcs_repo::treeops::Flat,
) -> Result<Vec<vcs_repo::FilePair>, Error> {
    let mut keys: std::collections::BTreeSet<&String> = old.keys().collect();
    keys.extend(new.keys());
    let mut out = vec![];
    for k in keys {
        let (o, n) = (old.get(k), new.get(k));
        if o == n {
            continue;
        }
        out.push(vcs_repo::FilePair {
            path: k.clone(),
            old: o.map(|(_, id)| repo.content(id)).transpose()?,
            new: n.map(|(_, id)| repo.content(id)).transpose()?,
            old_mode: o.map(|(m, _)| *m),
            new_mode: n.map(|(m, _)| *m),
        });
    }
    Ok(out)
}

fn diff_specs(staged: bool, specs: &[String], flags: &[String]) -> Result<(), Error> {
    use vcs_repo::treeops::Flat;
    let (repo, cwd) = open()?;
    let mut full = vec!["vcs".to_owned(), "diff".to_owned()];
    full.extend(flags.iter().cloned());
    let opts = match crate::Cli::try_parse_from(full).map_err(|e| Error::Msg(e.to_string()))?.cmd {
        crate::Cmd::Diff { opts, .. } => opts,
        _ => unreachable!(),
    };
    // `A...B` is the diff from the merge base of A and B to B
    let mut expanded: Vec<String> = Vec::with_capacity(specs.len());
    let mut before_dd = true;
    for sp in specs {
        if sp == "--" {
            before_dd = false;
        }
        match sp.split_once("...") {
            Some((a, b)) if before_dd => {
                let zone = repo.resolve_zone(&zone_of_cwd(&repo, &cwd, None)?)?;
                let (a, b) = (if a.is_empty() { "HEAD" } else { a }, if b.is_empty() { "HEAD" } else { b });
                let (ia, ib) = (repo.resolve_rev(&zone, a)?, repo.resolve_rev(&zone, b)?);
                let base = repo
                    .merge_bases(&ia, &ib)?
                    .first()
                    .copied()
                    .ok_or_else(|| Error::Msg(format!("{a}...{b}: no merge base")))?;
                expanded.push(base.to_hex());
                expanded.push(ib.to_hex());
            }
            _ => expanded.push(sp.clone()),
        }
    }
    let specs: &[String] = &expanded;
    let (revs_part, path_part) = match specs.iter().position(|s| s == "--") {
        Some(i) => (&specs[..i], &specs[i + 1..]),
        None => (specs, &specs[0..0]),
    };
    // leading specs that resolve as revisions are revisions (at most 2); the rest are paths
    let mut revs: Vec<String> = vec![];
    let mut paths: Vec<String> = path_part.to_vec();
    for (i, s) in revs_part.iter().enumerate() {
        if revs.len() < 2
            && i == revs.len()
            && repo.rev_files(s).is_ok()
            && !(cwd.join(s).exists() && path_part.is_empty() && revs_part.len() == 1 && repo.rev_files(s).is_err())
        {
            revs.push(s.clone());
        } else {
            paths.push(s.clone());
        }
    }
    let rels: Vec<String> = paths.iter().map(|p| repo.rel_path(&cwd, p)).collect::<Result<_, _>>()?;
    let keep = |p: &str| rels.is_empty() || rels.iter().any(|f| f.is_empty() || p == f || p.starts_with(&format!("{f}/")));
    let mut pairs = match (revs.len(), staged) {
        (0, _) => repo.diff_pairs(staged)?,
        (1, false) => {
            // revision against the working tree, for tracked files
            let old = repo.rev_files(&revs[0])?;
            let mut keys: std::collections::BTreeSet<String> = old.keys().cloned().collect();
            keys.extend(repo.tracked_files()?.into_iter().map(|(p, _, _)| p));
            let mut out = vec![];
            for k in keys {
                let new = std::fs::read(repo.worktree().join(&k)).ok();
                let o = old.get(&k);
                let same = match (&o, &new) {
                    (Some((_, id)), Some(n)) => &repo.content(id)? == n,
                    (None, None) => true,
                    _ => false,
                };
                if same {
                    continue;
                }
                out.push(vcs_repo::FilePair {
                    path: k.clone(),
                    old: o.map(|(_, id)| repo.content(id)).transpose()?,
                    old_mode: o.map(|(m, _)| *m),
                    new_mode: new.as_ref().map(|_| o.map_or(vcs_objects::EntryMode::File, |(m, _)| *m)),
                    new: new,
                });
            }
            out
        }
        (1, true) => {
            let old = repo.rev_files(&revs[0])?;
            let new: Flat = repo.tracked_files()?.into_iter().map(|(p, m, id)| (p, (m, id))).collect();
            flat_pairs(&repo, &old, &new)?
        }
        (_, _) => {
            if staged {
                return Err(missing("diff --staged A B", "--staged with two revisions", "`vcs git diff A B`"));
            }
            flat_pairs(&repo, &repo.rev_files(&revs[0])?, &repo.rev_files(&revs[1])?)?
        }
    };
    pairs.retain(|p| keep(&p.path));
    print!("{}", crate::difftool::render_pairs(pairs, &opts));
    Ok(())
}

fn log_patch(log_argv: &[String], stat: bool) -> Result<(), Error> {
    let mut full = vec!["vcs".to_owned()];
    full.extend(log_argv.iter().cloned());
    let crate::Cmd::Log {
        zone,
        limit,
        author,
        x,
        branch,
        revs,
        paths,
        ..
    } = crate::Cli::try_parse_from(full).map_err(|e| Error::Msg(e.to_string()))?.cmd
    else {
        unreachable!()
    };
    let mut x = x;
    x.revs = revs;
    let (repo, cwd) = open()?;
    let zone = zone_of_cwd(&repo, &cwd, zone)?;
    let opts = crate::logfmt::Opts {
        oneline: false,
        graph: false,
        author,
        color: false,
        paths,
        x,
        trust: None,
    };
    let (shown, _) = crate::logfmt::select(&repo, &cwd, &zone, branch.as_deref(), limit, &opts)?;
    for (id, _) in shown {
        let mut a = vec!["show".to_owned(), id.to_string()];
        if stat {
            a.push("--stat".into());
        }
        native(&a, false)?;
        println!();
    }
    Ok(())
}

fn from_rev(rev: &str, paths: &[String], stage: bool) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let files = repo.rev_files(rev)?;
    let rels: Vec<String> = paths.iter().map(|p| repo.rel_path(&cwd, p)).collect::<Result<_, _>>()?;
    let mut written = vec![];
    for r in &rels {
        let hits: Vec<(&String, &(vcs_objects::EntryMode, vcs_objects::ObjectId))> = files
            .iter()
            .filter(|(p, _)| r.is_empty() || *p == r || p.starts_with(&format!("{r}/")))
            .collect();
        if hits.is_empty() {
            return Err(Error::Msg(format!("pathspec {r:?} did not match any file in {rev}")));
        }
        for (p, (_, id)) in hits {
            let data = repo.content(id)?;
            let abs = repo.worktree().join(p);
            if let Some(d) = abs.parent() {
                std::fs::create_dir_all(d).map_err(|source| Error::Io {
                    path: d.to_owned(),
                    source,
                })?;
            }
            std::fs::write(&abs, data).map_err(|source| Error::Io { path: abs.clone(), source })?;
            written.push(p.clone());
        }
    }
    println!("updated {} file(s) from {rev}", written.len());
    if stage && !written.is_empty() {
        repo.add(&written)?;
    }
    Ok(())
}

fn checkout_auto(target: &str, _paths: &[String]) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = zone_of_cwd(&repo, &cwd, None)?;
    let is_branch = repo.branches(&zone)?.iter().any(|(b, _, _)| b == target);
    if !is_branch && target == "." || (!is_branch && cwd.join(target).exists()) {
        return native(&v(&["restore", target]), false);
    }
    if !is_branch && repo.resolve_rev(&zone, target).is_ok() {
        return native(&v(&["switch", "--detach", target]), false);
    }
    native(&v(&["switch", target]), false)
}
