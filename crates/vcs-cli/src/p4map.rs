//! `vcs p4 <p4 args...>`: every everyday Perforce command mapped one to one onto a vcs command,
//! in process, with no `p4` binary and no server. The map is documented in docs/p4-to-vcs.md and
//! printed by `vcs p4 --list`.
//!
//! The front-end only translates arguments into [`Step`]s (vcs argv, or a few small computed
//! steps that need the repository), prints the vcs command it runs (stderr, unless `-q`), and
//! runs it through the normal CLI. `vcs p4 --explain <p4 args>` prints the translation and runs
//! nothing. Where p4 and vcs differ in meaning, a `note:` line says so; where there is no
//! equivalent the command fails and names the native vcs command.

use crate::{need_remote, open};
use clap::Args;
use std::path::Path;
use vcs_repo::{ChangeKind, Error, Repo, ROOT_ZONE};

#[derive(Args, Clone)]
pub struct P4Args {
    /// A Perforce command line: `vcs p4 [--explain] [-q] [-d DIR] <command> [flags] [files]`;
    /// `vcs p4 --list` prints the whole map
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, value_name = "P4_ARGS")]
    pub args: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// same effect, arguments translate mechanically
    Exact,
    /// same effect under another command name or flag
    Renamed,
    /// translated, but the semantics differ (a note says how)
    Different,
    /// no equivalent: the command fails and names the nearest native command
    Missing,
}

impl Status {
    fn name(self) -> &'static str {
        match self {
            Status::Exact => "exact",
            Status::Renamed => "renamed",
            Status::Different => "different-semantics",
            Status::Missing => "missing",
        }
    }
}

pub struct Row {
    pub p4: &'static str,
    pub vcs: &'static str,
    pub status: Status,
    /// case name in crates/vcs-cli/tests/p4map.rs
    pub test: &'static str,
}

const fn r(p4: &'static str, vcs: &'static str, status: Status, test: &'static str) -> Row {
    Row { p4, vcs, status, test }
}

use Status::{Different as D, Exact as E, Missing as M, Renamed as R};

/// The map. `docs/p4-to-vcs.md` carries the same rows (a test checks that every row is there).
pub const TABLE: &[Row] = &[
    r("client -o / workspace -o", "zone list; sparse list", D, "client_view"),
    r("clients / workspaces", "worktree list", D, "clients"),
    r("depots", "zone list", R, "depots"),
    r("sync", "sync", D, "sync"),
    r("sync -f", "sync; restore .", D, "sync_f"),
    r("sync -n", "fetch", D, "sync_n"),
    r("sync path@rev / sync @label", "(none)", M, "sync_rev_missing"),
    r("edit", "lock (lockable files); nothing otherwise", D, "edit_plain"),
    r("add", "add", E, "add"),
    r("delete", "rm", R, "delete"),
    r("move / rename", "mv", R, "move"),
    r("revert", "restore --staged; restore", R, "revert"),
    r("revert -a", "(nothing to do)", D, "revert_a"),
    r("diff", "diff; diff --staged (filtered to the files)", D, "diff"),
    r("diff2", "(computed) unified diff of two revisions", R, "diff2"),
    r("opened", "status (without untracked)", R, "opened"),
    r("opened -a", "locks", D, "opened_a"),
    r("reconcile [-a -e -d -n]", "add (the selected files)", R, "reconcile"),
    r("status", "reconcile -n", R, "p4_status"),
    r("submit [-d msg]", "add (edits); commit -m; publish", D, "submit"),
    r("changes [-m -s -u -l]", "log [-n --author --oneline]", R, "changes"),
    r("describe [-s]", "show [--name-status]", R, "describe"),
    r("filelog", "log --follow --name-status -- file", R, "filelog"),
    r("annotate", "blame", R, "annotate"),
    r("files", "ls-files --rev", R, "files"),
    r("fstat", "log -n 1 --format ... -- file", D, "fstat"),
    r("print", "cat-file -p rev:path", R, "print"),
    r("have", "ls-files", R, "have"),
    r("integrate / merge", "merge", D, "integrate"),
    r("copy", "(none)", M, "copy_missing"),
    r(
        "resolve [-at -ay -af -n]",
        "resolve --theirs/--ours/--mark; conflict list",
        R,
        "resolve",
    ),
    r("shelve [-c name] [-d]", "shelve [name] [--delete]", D, "shelve"),
    r("unshelve -s name", "unshelve name", D, "shelve"),
    r("label / tag / labelsync", "tag [-d]", D, "label"),
    r("labels", "tag", R, "labels"),
    r("branch / stream", "branch [-d]", D, "branch"),
    r("branches / streams", "branch", R, "branches"),
    r("switch", "switch", E, "switch"),
    r("lock", "lock --lease-secs", D, "lock"),
    r("unlock [-f]", "unlock [--break]", E, "lock"),
    r("locks", "locks", E, "lock"),
    r("where", "(computed) zone path, worktree path", R, "where"),
    r("info", "(computed) identity, root, remote", D, "info"),
    r("users", "shortlog -s -e", D, "users"),
    r("login / logout", "login / logout", E, "login"),
    r("init / fetch / push / clone", "init / fetch / publish / clone", R, "push"),
    r("grep", "grep", E, "grep"),
    r("clean", "restore --staged .; restore .", D, "clean"),
    r("change / changelist", "status", D, "change"),
    r("verify", "fsck", R, "verify"),
    r("reopen / obliterate / jobs / fix", "(none)", M, "missing_cmds"),
    r(
        "protect / groups / group / user / passwd",
        "(none: vcs-server tokens)",
        M,
        "protect_missing",
    ),
];

enum Step {
    Vcs(Vec<String>),
    Say(String),
    /// stage the p4 "opened" set: tracked edits, deletions and/or untracked files (add)
    Stage {
        edits: bool,
        deletes: bool,
        adds: bool,
        only: Vec<String>,
        preview: bool,
    },
    Revert(Vec<String>),
    Diff {
        only: Vec<String>,
        names: bool,
        staged: bool,
        unstaged: bool,
    },
    Diff2(String, String),
    Opened(Vec<String>),
    Where(Vec<String>),
    Info,
    ResolveAll(&'static str),
    PublishIfRemote,
}

#[derive(Default)]
struct Plan {
    steps: Vec<Step>,
    notes: Vec<String>,
}

impl Plan {
    fn vcs(mut self, a: &[&str]) -> Self {
        self.steps.push(Step::Vcs(a.iter().map(|s| s.to_string()).collect()));
        self
    }
    fn argv(mut self, a: Vec<String>) -> Self {
        self.steps.push(Step::Vcs(a));
        self
    }
    fn step(mut self, s: Step) -> Self {
        self.steps.push(s);
        self
    }
    fn note(mut self, n: impl Into<String>) -> Self {
        self.notes.push(n.into());
        self
    }
}

fn msg<T>(s: impl Into<String>) -> Result<T, Error> {
    Err(Error::Msg(s.into()))
}

/// "no equivalent" error: why, and the nearest native command.
fn nomap<T>(p4: &str, why: &str, native: &str) -> Result<T, Error> {
    msg(format!("p4 {p4}: no vcs equivalent: {why}\n\nNative command:\n  {native}"))
}

// ---------- getopt-style flags ----------

struct Opts {
    flags: Vec<(char, Option<String>)>,
    rest: Vec<String>,
    cmd: String,
}

impl Opts {
    /// `with_arg`: flags that take a value; `bools`: flags that do not. Any other flag is refused.
    fn parse(cmd: &str, args: &[String], with_arg: &str, bools: &str, native: &str) -> Result<Opts, Error> {
        let mut o = Opts {
            flags: vec![],
            rest: vec![],
            cmd: cmd.to_owned(),
        };
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            i += 1;
            if a == "--" {
                o.rest.extend(args[i..].iter().cloned());
                break;
            }
            if !a.starts_with('-') || a.len() == 1 {
                o.rest.push(a.clone());
                continue;
            }
            if a.starts_with("--") {
                return nomap(cmd, &format!("long flag {a} is not mapped"), native);
            }
            let chars: Vec<char> = a[1..].chars().collect();
            let mut k = 0;
            while k < chars.len() {
                let c = chars[k];
                k += 1;
                if with_arg.contains(c) {
                    let attached: String = chars[k..].iter().collect();
                    let v = if !attached.is_empty() {
                        attached
                    } else if i < args.len() {
                        i += 1;
                        args[i - 1].clone()
                    } else {
                        return msg(format!("p4 {cmd}: flag -{c} needs a value"));
                    };
                    o.flags.push((c, Some(v)));
                    break;
                } else if bools.contains(c) {
                    o.flags.push((c, None));
                } else {
                    return nomap(cmd, &format!("flag -{c} is not mapped"), native);
                }
            }
        }
        Ok(o)
    }
    fn has(&self, c: char) -> bool {
        self.flags.iter().any(|(f, _)| *f == c)
    }
    fn val(&self, c: char) -> Option<&str> {
        self.flags.iter().find(|(f, _)| *f == c).and_then(|(_, v)| v.as_deref())
    }
    /// `-c <change>`: only the default changelist exists (it is the index).
    fn default_change_only(&self) -> Result<(), Error> {
        match self.val('c') {
            None | Some("default") => Ok(()),
            Some(n) => nomap(
                &self.cmd,
                &format!("pending changelist {n}: vcs has one pending set (the index), no numbered pending changelists"),
                "vcs add <files> to stage a subset, `vcs shelve <name>` to park work, one commit per zone",
            ),
        }
    }
    fn need_files(&self) -> Result<(), Error> {
        if self.rest.is_empty() {
            return msg(format!(
                "p4 {}: no file given (usage: vcs p4 {} [flags] file...)",
                self.cmd, self.cmd
            ));
        }
        Ok(())
    }
}

// ---------- depot paths and revisions ----------

fn unescape(s: &str) -> String {
    s.replace("%40", "@")
        .replace("%23", "#")
        .replace("%2A", "*")
        .replace("%2a", "*")
        .replace("%25", "%")
}

/// Split `file@rev` / `file#rev` (before unescaping). `#head`/`#have` mean the checked-out head.
fn split_rev(arg: &str) -> Result<(String, Option<String>), Error> {
    if let Some((p, r)) = arg.rsplit_once('@') {
        if r.is_empty() {
            return msg(format!("empty revision in {arg:?}"));
        }
        return Ok((p.to_owned(), Some(rev_name(r)?)));
    }
    if let Some((p, r)) = arg.rsplit_once('#') {
        if r == "head" || r == "have" {
            return Ok((p.to_owned(), None));
        }
        return nomap(
            "revision",
            &format!("file revision numbers (#{r}) do not exist; vcs versions whole commits, not files"),
            "vcs log --follow -- <file> to find the commit, then `vcs cat-file -p <commit>:<file>`",
        );
    }
    Ok((arg.to_owned(), None))
}

/// A revision for vcs: tag, branch, commit id prefix. Short all-digit names are changelist numbers.
fn rev_name(r: &str) -> Result<String, Error> {
    let r = r.trim_start_matches('=');
    if !r.is_empty() && r.len() <= 6 && r.bytes().all(|b| b.is_ascii_digit()) {
        return nomap(
            "revision",
            &format!("changelist number {r}: vcs commits have content-hash ids, there is no global counter"),
            "vcs log --oneline (ids), then use an id prefix, a tag or a branch name",
        );
    }
    Ok(r.to_owned())
}

/// Depot-style name of a repo-relative path: `//depot/<path>` in the root zone, `//<zone>/<rest>` in another.
fn depot_path(repo: &Repo, rel: &str) -> String {
    if let Ok(z) = repo.zones() {
        let owner = z.owner(rel).to_owned();
        if owner != ROOT_ZONE {
            let pre = z.path_of(&owner).unwrap_or("");
            let rest = rel.strip_prefix(pre).unwrap_or(rel).trim_start_matches('/');
            return format!("//{}/{}", z.label(&owner), rest);
        }
    }
    format!("//depot/{rel}")
}

fn strip_wild(s: &str) -> Result<String, Error> {
    if s == "..." || s == "//..." {
        return Ok(".".into());
    }
    if let Some(x) = s.strip_suffix("/...") {
        return Ok(if x.is_empty() { ".".into() } else { x.to_owned() });
    }
    if s.contains("...") {
        return nomap(
            "path",
            &format!("wildcard `...` is only supported at the end of a path ({s})"),
            "vcs ls-files | grep <pattern>, or vcs grep",
        );
    }
    Ok(s.to_owned())
}

/// One file argument: depot syntax (`//depot/...`, `//<zone>/dir/...`) or local, trailing `...` -> directory.
/// Returns a path vcs accepts (absolute for depot syntax) and the revision suffix.
fn path_arg(arg: &str) -> Result<(String, Option<String>), Error> {
    let (p, rev) = split_rev(arg)?;
    let p = unescape(&p);
    if let Some(rest) = p.strip_prefix("//") {
        let (repo, _) = open()?;
        let (head, tail) = rest.split_once('/').unwrap_or((rest, ""));
        let zones = repo.zones()?;
        let rel = match zones.resolve(head) {
            Some(id) if id != ROOT_ZONE => {
                let pre = zones.path_of(&id).unwrap_or("");
                let t = tail.trim_start_matches('/');
                if t.is_empty() || t == "..." {
                    format!("{pre}/...")
                } else {
                    format!("{pre}/{t}")
                }
            }
            _ => tail.to_owned(),
        };
        let rel = strip_wild(&rel)?;
        let abs = if rel == "." {
            repo.worktree().to_path_buf()
        } else {
            repo.worktree().join(rel)
        };
        return Ok((abs.to_string_lossy().into_owned(), rev));
    }
    Ok((strip_wild(&p)?, rev))
}

/// File arguments without revision suffixes.
fn plain_paths(args: &[String]) -> Result<Vec<String>, Error> {
    args.iter()
        .map(|a| {
            let (p, r) = path_arg(a)?;
            match r {
                None => Ok(p),
                Some(r) => msg(format!("revision @{r} on {a:?} is not supported by this command")),
            }
        })
        .collect()
}

/// `//depot/dev/...`, `//art/rel/1.0`, `dev` -> (zone, branch).
fn branch_of(arg: &str) -> Result<(Option<String>, String), Error> {
    let (a, _) = split_rev(arg)?;
    let a = unescape(&a);
    let a = a.strip_suffix("/...").unwrap_or(&a).trim_end_matches('/').to_owned();
    if let Some(rest) = a.strip_prefix("//") {
        let (head, tail) = rest.split_once('/').unwrap_or((rest, ""));
        if tail.is_empty() {
            return Ok((None, head.to_owned()));
        }
        if let Ok((repo, _)) = open() {
            if let Some(id) = repo.zones()?.resolve(head) {
                if id != ROOT_ZONE {
                    return Ok((Some(head.to_owned()), tail.to_owned()));
                }
            }
        }
        return Ok((None, tail.to_owned()));
    }
    Ok((None, a))
}

fn s(x: &str) -> String {
    x.to_owned()
}

fn lease_secs() -> u64 {
    std::env::var("VCS_P4_LOCK_LEASE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(8 * 3600)
}

const ALL: &str = "1000000000";

// ---------- translation ----------

fn translate(cmd: &str, a: &[String]) -> Result<Plan, Error> {
    match cmd {
        "add" => {
            let o = Opts::parse("add", a, "c", "nf", "vcs add --help")?;
            o.default_change_only()?;
            o.need_files()?;
            let paths = plain_paths(&o.rest)?;
            if o.has('n') {
                return Ok(Plan::default().step(Step::Stage {
                    edits: false,
                    deletes: false,
                    adds: true,
                    only: paths,
                    preview: true,
                }));
            }
            let mut v = vec![s("add")];
            v.extend(paths);
            Ok(Plan::default().argv(v))
        }
        "edit" => {
            let o = Opts::parse("edit", a, "c", "nk", "vcs lock --help")?;
            o.default_change_only()?;
            o.need_files()?;
            let (repo, cwd) = open()?;
            let attrs = repo.attrs()?;
            let mut plan = Plan::default();
            let mut locks = vec![];
            for f in plain_paths(&o.rest)? {
                let rel = repo.rel_path(&cwd, &f)?;
                if attrs.lookup(&rel).lockable {
                    locks.push(f);
                } else {
                    if !repo.worktree().join(&rel).exists() {
                        return msg(format!("p4 edit: {f}: file not found in the workspace"));
                    }
                    plan = plan.step(Step::Say(format!(
                        "{f}: open for edit (nothing to do: vcs files are writable and an edit is detected by content; `vcs add`/`p4 submit` stage it)"
                    )));
                }
            }
            if !locks.is_empty() && !o.has('n') {
                let mut v = vec![s("lock")];
                v.extend(locks);
                v.extend([s("--lease-secs"), lease_secs().to_string(), s("--reason"), s("p4 edit")]);
                plan = plan.argv(v).note(format!(
                    "lockable file: p4 edit took an exclusive lock on the remote for {} s (override with VCS_P4_LOCK_LEASE_SECS); repeat the command to renew",
                    lease_secs()
                ));
            }
            Ok(plan)
        }
        "delete" => {
            let o = Opts::parse("delete", a, "c", "kv", "vcs rm --help")?;
            o.default_change_only()?;
            o.need_files()?;
            let mut v = vec![s("rm")];
            if o.has('k') {
                v.push(s("--cached"));
            }
            v.extend(plain_paths(&o.rest)?);
            Ok(Plan::default().argv(v))
        }
        "move" | "rename" => {
            let o = Opts::parse(cmd, a, "c", "f", "vcs mv --help")?;
            o.default_change_only()?;
            if o.rest.len() != 2 {
                return msg(format!("p4 {cmd}: expected exactly two arguments: source and target"));
            }
            let p = plain_paths(&o.rest)?;
            let mut v = vec![s("mv")];
            if o.has('f') {
                v.push(s("--force"));
            }
            v.extend(p);
            Ok(Plan::default().argv(v))
        }
        "revert" => {
            let o = Opts::parse("revert", a, "c", "ank", "vcs restore --help")?;
            o.default_change_only()?;
            if o.has('k') {
                return nomap("revert -k", "there is no have list to keep", "vcs restore --staged <files> (unstage only)");
            }
            if o.has('a') {
                return Ok(Plan::default().step(Step::Say(
                    "revert -a: nothing to do (vcs never stages a file whose content equals HEAD, so there are no opened-but-unchanged files)".into(),
                )));
            }
            o.need_files()?;
            if o.has('n') {
                return Ok(Plan::default().vcs(&["status"]).note("-n: showing `vcs status` (what would be reverted); nothing changed"));
            }
            Ok(Plan::default().step(Step::Revert(plain_paths(&o.rest)?)).note(
                "a new file that was added stays on disk after revert (same as p4); changes to tracked files are discarded",
            ))
        }
        "diff" => {
            let o = Opts::parse("diff", a, "s", "fu", "vcs diff --help")
                .or_else(|_| Opts::parse("diff", &fix_diff_flags(a), "s", "fu", "vcs diff --help"))?;
            let only = plain_paths(&o.rest)?;
            let (names, staged, unstaged) = match o.val('s') {
                None => (false, true, true),
                Some("e") => (true, false, true),
                Some("a") => (true, true, false),
                Some(x) => {
                    return nomap(&format!("diff -s{x}"), "only -se (unstaged edits) and -sa (staged) map", "vcs status")
                }
            };
            Ok(Plan::default()
                .step(Step::Diff { only, names, staged, unstaged })
                .note("shows the staged diff (HEAD -> index) then the unstaged diff (index -> disk); a file with both appears twice"))
        }
        "diff2" => {
            let o = Opts::parse("diff2", a, "", "u", "vcs show <rev>; vcs diff-tree A B")?;
            if o.rest.len() != 2 {
                return msg("p4 diff2: expected two file arguments (file1[@rev] file2[@rev])");
            }
            Ok(Plan::default().step(Step::Diff2(o.rest[0].clone(), o.rest[1].clone())))
        }
        "opened" => {
            let o = Opts::parse("opened", a, "cCum", "al", "vcs status")?;
            if o.has('a') {
                return Ok(Plan::default()
                    .vcs(&["locks"])
                    .note("opened -a: vcs has no per-user list of opened files; showing the exclusive locks held by everyone"));
            }
            if o.val('c').is_some() || o.val('C').is_some() || o.val('u').is_some() {
                return nomap("opened -c/-C/-u", "no numbered changelists, clients or per-user open lists", "vcs status");
            }
            Ok(Plan::default().step(Step::Opened(plain_paths(&o.rest)?)))
        }
        "reconcile" | "status" => {
            let o = Opts::parse(cmd, a, "", "aednl", "vcs add / vcs status")?;
            let any = o.has('a') || o.has('e') || o.has('d');
            let preview = o.has('n') || cmd == "status";
            let mut only = plain_paths(&o.rest)?;
            if only.is_empty() {
                only.push(s("."));
            }
            Ok(Plan::default().step(Step::Stage {
                adds: o.has('a') || !any,
                edits: o.has('e') || !any,
                deletes: o.has('d') || !any,
                only,
                preview,
            }))
        }
        "submit" => {
            let o = Opts::parse("submit", a, "dc", "", "vcs commit --help; vcs publish")
                .map_err(|e| match e {
                    Error::Msg(m) if m.contains("flag -") => Error::Msg(format!(
                        "{m}\n\nNote: -f/-i/-e/-s/-r have no vcs equivalent (no forms, no submit of shelves; unshelve then commit)"
                    )),
                    e => e,
                })?;
            o.default_change_only()?;
            let only = plain_paths(&o.rest)?;
            let mut commit = vec![s("commit")];
            if let Some(m) = o.val('d') {
                commit.extend([s("-m"), m.to_owned()]);
            }
            let mut plan = Plan::default()
                .step(Step::Stage {
                    adds: false,
                    edits: true,
                    deletes: true,
                    only: if only.is_empty() { vec![s(".")] } else { only.clone() },
                    preview: false,
                })
                .argv(commit)
                .step(Step::PublishIfRemote)
                .note("p4 submit is one server step; vcs commits locally, then publishes when a remote is configured. New files must be `p4 add`ed first (as in p4)");
            if !only.is_empty() {
                plan = plan.note("files given: only those edits are staged, but anything already staged is committed too");
            }
            Ok(plan)
        }
        "changes" => {
            let o = Opts::parse("changes", a, "mscuiL", "ltrLe", "vcs log --help")?;
            if o.val('c').is_some() {
                return nomap("changes -c", "no clients", "vcs log");
            }
            if o.has('r') {
                return nomap("changes -r", "oldest-first order is not available", "vcs log -n N, read bottom-up");
            }
            match o.val('s') {
                None | Some("submitted") => {}
                Some("pending") => {
                    return Ok(Plan::default().vcs(&["status"]).note("changes -s pending: your pending set is the index; showing `vcs status`"))
                }
                Some("shelved") => return Ok(Plan::default().vcs(&["shelves"])),
                Some(x) => return nomap(&format!("changes -s {x}"), "status not mapped", "vcs log; vcs shelves; vcs status"),
            }
            let n = o.val('m').unwrap_or(ALL).to_owned();
            let mut v = vec![s("log"), s("-n"), n];
            if let Some(u) = o.val('u') {
                v.extend([s("--author"), u.to_owned()]);
            }
            if o.has('l') {
                // full messages are the default log layout
            } else if o.has('t') {
                v.extend([s("--format"), s("%h %ad %an: %s")]);
            } else {
                v.push(s("--oneline"));
            }
            let paths = plain_paths(&o.rest)?;
            if !paths.is_empty() {
                v.push(s("--"));
                v.extend(paths);
            }
            Ok(Plan::default().argv(v).note("changes are listed as commit ids, not changelist numbers"))
        }
        "describe" => {
            let o = Opts::parse("describe", a, "", "sduSa", "vcs show <rev>")?;
            if o.has('S') {
                return nomap("describe -S", "shelved diffs cannot be shown without applying them", "vcs unshelve <shelf>; vcs diff --staged");
            }
            if o.rest.is_empty() {
                return msg("p4 describe: no change given (a commit id prefix, tag or branch)");
            }
            let mut plan = Plan::default();
            for r in &o.rest {
                let mut v = vec![s("show"), rev_name(r)?];
                if o.has('s') {
                    v.push(s("--name-status"));
                }
                plan = plan.argv(v);
            }
            Ok(plan)
        }
        "filelog" => {
            let o = Opts::parse("filelog", a, "m", "ilLtsh", "vcs log --follow -- <file>")?;
            o.need_files()?;
            let mut plan = Plan::default();
            for f in plain_paths(&o.rest)? {
                plan = plan.step(Step::Say(f.clone())).argv(vec![
                    s("log"),
                    s("--follow"),
                    s("--name-status"),
                    s("-n"),
                    o.val('m').unwrap_or(ALL).to_owned(),
                    s("--"),
                    f,
                ]);
            }
            Ok(plan)
        }
        "annotate" => {
            let o = Opts::parse("annotate", a, "", "cqitu", "vcs blame --help")?;
            if o.rest.len() != 1 {
                return msg("p4 annotate: expected one file (file[@rev])");
            }
            let (p, rev) = path_arg(&o.rest[0])?;
            let mut v = vec![s("blame")];
            if o.has('i') {
                v.push(s("-M"));
            }
            v.push(p);
            if let Some(r) = rev {
                v.extend([s("--rev"), r]);
            }
            Ok(Plan::default().argv(v))
        }
        "files" => {
            let o = Opts::parse("files", a, "m", "ael", "vcs ls-files --help")?;
            if o.has('a') {
                return nomap("files -a", "per-file revision lists do not exist", "vcs log --follow --name-status -- <file>");
            }
            let mut rev = None;
            let mut paths = vec![];
            for f in &o.rest {
                let (p, r) = path_arg(f)?;
                rev = rev.or(r);
                paths.push(p);
            }
            let mut v = vec![s("ls-files"), s("--rev"), rev.unwrap_or_else(|| s("HEAD"))];
            v.extend(paths);
            Ok(Plan::default().argv(v))
        }
        "fstat" => {
            let o = Opts::parse("fstat", a, "", "", "vcs ls-files -s; vcs log -n 1 -- <file>")?;
            o.need_files()?;
            let mut plan = Plan::default().note("fstat: only depotFile, headChange (commit), headTime and headAuthor are available; haveRev, headRev, headAction, headType, otherOpen are not");
            for f in plain_paths(&o.rest)? {
                plan = plan.step(Step::Say(format!("... depotFile {f}"))).argv(vec![
                    s("log"),
                    s("-n"),
                    s("1"),
                    s("--format"),
                    s("... headChange %h%n... headTime %at%n... headAuthor %an"),
                    s("--"),
                    f,
                ]);
            }
            Ok(plan)
        }
        "print" => {
            let o = Opts::parse("print", a, "", "qk", "vcs cat-file -p <rev>:<path>")?;
            o.need_files()?;
            let mut plan = Plan::default();
            if !o.has('q') {
                plan = plan.note("print: no `//depot/file#rev - action` header line is written (always like -q)");
            }
            for f in &o.rest {
                let (p, rev) = path_arg(f)?;
                plan = plan.argv(vec![s("cat-file"), s("-p"), format!("{}:{p}", rev.unwrap_or_else(|| s("HEAD")))]);
            }
            Ok(plan)
        }
        "have" => {
            let o = Opts::parse("have", a, "", "", "vcs ls-files")?;
            let mut v = vec![s("ls-files")];
            v.extend(plain_paths(&o.rest)?);
            Ok(Plan::default().argv(v).note("have: paths only, there are no per-file revision numbers"))
        }
        "integrate" | "merge" => {
            let o = Opts::parse(cmd, a, "bSsPDo", "tvfdhirRFcn", "vcs merge --help")?;
            if o.has('n') || o.has('r') || o.has('R') || o.has('F') || o.has('c') {
                return nomap(&format!("{cmd} -n/-r/-R/-F/-c"), "no preview, reverse or file-level integrate", "vcs merge <branch> (then vcs merge --abort to undo)");
            }
            let src = o
                .val('S')
                .or(o.val('b'))
                .map(str::to_owned)
                .or_else(|| o.rest.first().cloned());
            let Some(src) = src else {
                return msg(format!("p4 {cmd}: name the source: -S //stream, -b branch, or a branch path"));
            };
            if o.rest.len() > 1 || (o.rest.len() == 1 && (o.val('S').is_some() || o.val('b').is_some())) {
                return nomap(
                    &format!("{cmd} <source> <target>"),
                    "the target is always the checked-out branch of the zone",
                    "vcs switch <target-branch>; vcs merge <source-branch>",
                );
            }
            let (zone, branch) = branch_of(&src)?;
            let mut v = vec![s("merge"), branch];
            if let Some(z) = zone {
                v.extend([s("--zone"), z]);
            }
            Ok(Plan::default().argv(v).note(
                "p4 integrate opens files for a later resolve + submit; vcs merge merges whole branches, commits when clean, and stops with conflicts for `p4 resolve`",
            ))
        }
        "copy" => nomap(
            "copy",
            "there is no 'make target identical to source' operation",
            "vcs merge <branch> (fast-forwards when possible), or vcs switch",
        ),
        "resolve" => {
            let o = Opts::parse("resolve", a, "", "amstyfnd", "vcs resolve --help")?;
            let mode = ["t", "y", "f"].iter().find(|m| o.has(m.chars().next().unwrap()) && o.has('a'));
            if o.has('n') {
                return Ok(Plan::default().vcs(&["conflict", "list"]));
            }
            if o.has('a') && (o.has('m') || o.has('s')) && mode.is_none() {
                return Ok(Plan::default().vcs(&["conflict", "list"]).note(
                    "resolve -am/-as: vcs already merged what it could when you ran `p4 merge`; the files listed need -at (take theirs), -ay (keep yours) or an edit then `p4 resolve -af`",
                ));
            }
            let flag = match mode.copied() {
                Some("t") => "--theirs",
                Some("y") => "--ours",
                Some("f") => "--mark",
                _ => {
                    return nomap(
                        "resolve (interactive)",
                        "there is no interactive resolve",
                        "edit the file, then `vcs resolve <file> --mark`; or `vcs resolve <file> --ours|--theirs`",
                    )
                }
            };
            let paths = plain_paths(&o.rest)?;
            let note = "resolve: p4 'theirs' is the merged-in branch (--theirs), p4 'yours' is the checked-out branch (--ours); -af marks the file as it is on disk";
            if paths.is_empty() {
                return Ok(Plan::default().step(Step::ResolveAll(flag)).note(note));
            }
            let mut plan = Plan::default().note(note);
            for p in paths {
                plan = plan.argv(vec![s("resolve"), p, s(flag)]);
            }
            Ok(plan)
        }
        "shelve" => {
            let o = Opts::parse("shelve", a, "c", "dfrpi", "vcs shelve --help")?;
            if o.has('i') || o.has('p') {
                return nomap("shelve -i/-p", "no change forms, no promotion", "vcs shelve <name> -m <message>");
            }
            if !o.rest.is_empty() {
                return nomap("shelve <files>", "a shelf holds all your changes in the zone", "vcs shelve <name> [--zone Z]");
            }
            let mut v = vec![s("shelve")];
            if o.has('d') {
                v.push(s("--delete"));
            }
            if let Some(n) = o.val('c') {
                v.push(n.to_owned());
            } else if o.has('d') {
                return msg("p4 shelve -d: name the shelf with -c <name>");
            }
            Ok(Plan::default().argv(v).note(
                "shelves are named (-c takes a name, not a change number), live on the remote, and are private to you; shelving to an existing name fails (delete it first, there is no -r)",
            ))
        }
        "unshelve" => {
            let o = Opts::parse("unshelve", a, "sbc", "fnr", "vcs unshelve --help")?;
            let Some(n) = o.val('s') else {
                return msg("p4 unshelve: name the shelf with -s <name> (or principal/name)");
            };
            if o.val('b').is_some() || o.val('c').is_some_and(|c| c != "default") {
                return nomap("unshelve -b/-c", "no branch mappings or numbered changelists", "vcs unshelve <shelf>");
            }
            Ok(Plan::default().vcs(&["unshelve", n]))
        }
        "label" | "tag" | "labelsync" => {
            let o = Opts::parse(cmd, a, "lt", "dnaoiq", "vcs tag --help")?;
            if o.has('o') || o.has('i') || o.has('n') || o.val('t').is_some() {
                return nomap(&format!("{cmd} -o/-i/-n/-t"), "labels have no spec form", "vcs tag <name> [rev]; vcs tag");
            }
            let name = if cmd == "label" {
                o.rest.first().cloned()
            } else {
                o.val('l').map(str::to_owned)
            };
            let Some(name) = name else {
                return msg(format!("p4 {cmd}: no label name ({})", if cmd == "label" { "p4 label <name>" } else { "-l <label>" }));
            };
            let files = if cmd == "label" { &o.rest[1..] } else { &o.rest[..] };
            for f in files {
                let p = plain_paths(std::slice::from_ref(f))?.remove(0);
                if p != "." && !is_repo_root(&p) {
                    return nomap(
                        &format!("{cmd} <files>"),
                        "a vcs tag names a whole commit of a zone, not a subset of files",
                        "vcs tag <name> [rev]",
                    );
                }
            }
            let mut v = vec![s("tag")];
            if o.has('d') {
                v.push(s("-d"));
            }
            v.push(name);
            Ok(Plan::default().argv(v).note("a label is a tag: it names the commit checked out in the current zone"))
        }
        "labels" => {
            Opts::parse("labels", a, "", "", "vcs tag")?;
            Ok(Plan::default().vcs(&["tag"]))
        }
        "branch" | "stream" => {
            let o = Opts::parse(cmd, a, "", "dfoiv", "vcs branch --help")?;
            if o.has('o') || o.has('i') {
                return nomap(&format!("{cmd} -o/-i"), "branches have no spec form", "vcs branch <name>; vcs branch");
            }
            let Some(name) = o.rest.first() else {
                return msg(format!("p4 {cmd}: no name given"));
            };
            let (zone, b) = branch_of(name)?;
            let mut v = vec![s("branch")];
            if o.has('d') {
                v.push(s("-d"));
                if o.has('f') {
                    v.push(s("--force"));
                }
            }
            v.push(b);
            if let Some(z) = zone {
                v.extend([s("--zone"), z]);
            }
            Ok(Plan::default().argv(v).note("creates a branch at the checked-out head of the zone (a p4 branch spec/stream is a mapping, not a line of history)"))
        }
        "branches" | "streams" => {
            Opts::parse(cmd, a, "", "", "vcs branch")?;
            Ok(Plan::default().vcs(&["branch"]))
        }
        "switch" => {
            let o = Opts::parse("switch", a, "", "", "vcs switch <branch>")?;
            let Some(n) = o.rest.first() else {
                return msg("p4 switch: no stream/branch given");
            };
            let (zone, b) = branch_of(n)?;
            let mut v = vec![s("switch"), b];
            if let Some(z) = zone {
                v.extend([s("--zone"), z]);
            }
            Ok(Plan::default().argv(v))
        }
        "lock" => {
            let o = Opts::parse("lock", a, "c", "", "vcs lock --lease-secs N <files>")?;
            o.default_change_only()?;
            o.need_files()?;
            let mut v = vec![s("lock")];
            v.extend(plain_paths(&o.rest)?);
            v.extend([s("--lease-secs"), lease_secs().to_string(), s("--reason"), s("p4 lock")]);
            Ok(Plan::default().argv(v).note(format!(
                "p4 locks do not expire; vcs locks are leases: {} s here (VCS_P4_LOCK_LEASE_SECS), repeat to renew",
                lease_secs()
            )))
        }
        "unlock" => {
            let o = Opts::parse("unlock", a, "cx", "f", "vcs unlock --help")?;
            o.default_change_only()?;
            o.need_files()?;
            let mut v = vec![s("unlock")];
            if o.has('f') {
                v.push(s("--break"));
            }
            v.extend(plain_paths(&o.rest)?);
            Ok(Plan::default().argv(v))
        }
        "locks" => {
            Opts::parse("locks", a, "", "", "vcs locks")?;
            Ok(Plan::default().vcs(&["locks"]))
        }
        "where" => {
            let o = Opts::parse("where", a, "", "", "vcs ls-files")?;
            let mut p = plain_paths(&o.rest)?;
            if p.is_empty() {
                p.push(s("."));
            }
            Ok(Plan::default().step(Step::Where(p)))
        }
        "info" => Ok(Plan::default().step(Step::Info)),
        "users" => {
            Opts::parse("users", a, "", "m", "vcs shortlog -s -e")?;
            Ok(Plan::default()
                .vcs(&["shortlog", "-s", "-e"])
                .note("users: lists commit authors of the current branch, not accounts (accounts are server tokens, see docs/server.md)"))
        }
        "login" => {
            let o = Opts::parse("login", a, "", "s", "vcs login <url>")?;
            if o.has('s') {
                return Ok(Plan::default().vcs(&["auth", "status"]));
            }
            let (repo, _) = open()?;
            let url = need_remote(&repo)?;
            Ok(Plan::default().vcs(&["login", &url]))
        }
        "logout" => Ok(Plan::default().vcs(&["logout"])),
        "init" => Ok(Plan::default().vcs(&["init"])),
        "fetch" => {
            Opts::parse("fetch", a, "", "", "vcs fetch")?;
            Ok(Plan::default().vcs(&["fetch"]))
        }
        "push" => {
            Opts::parse("push", a, "", "", "vcs publish")?;
            Ok(Plan::default().vcs(&["publish"]))
        }
        "clone" => {
            let o = Opts::parse("clone", a, "pd", "", "vcs clone <url> <dir>")?;
            let Some(url) = o.val('p') else {
                return msg("p4 clone: -p <remote url> is required (a vcs remote: directory, file://, http(s)://, s3://)");
            };
            let dir = o
                .val('d')
                .map(str::to_owned)
                .unwrap_or_else(|| url.trim_end_matches('/').rsplit('/').next().unwrap_or("repo").to_owned());
            Ok(Plan::default().vcs(&["clone", url, &dir]))
        }
        "grep" => {
            let o = Opts::parse("grep", a, "e", "invlsa", "vcs grep --help")?;
            if o.has('v') || o.has('a') {
                return nomap("grep -v/-a", "inverted matches and all revisions are not available", "vcs grep <pattern>");
            }
            let Some(pat) = o.val('e') else {
                return msg("p4 grep: -e <pattern> is required");
            };
            let mut v = vec![s("grep")];
            if o.has('i') {
                v.push(s("-i"));
            }
            if o.has('n') {
                v.push(s("-n"));
            }
            if o.has('l') {
                v.push(s("-l"));
            }
            v.push(pat.to_owned());
            let paths = plain_paths(&o.rest)?;
            if !paths.is_empty() {
                v.push(s("--"));
                v.extend(paths);
            }
            Ok(Plan::default().argv(v))
        }
        "clean" => {
            let o = Opts::parse("clean", a, "", "n", "vcs restore .")?;
            if o.has('n') {
                return Ok(Plan::default().vcs(&["status"]).note("clean -n: showing `vcs status`; nothing changed"));
            }
            Ok(Plan::default()
                .vcs(&["restore", "--staged", "."])
                .vcs(&["restore", "."])
                .note("clean: tracked files are restored; untracked files are NOT deleted (vcs has no delete-untracked command; `vcs status` lists them)"))
        }
        "change" | "changelist" => Ok(Plan::default().vcs(&["status"]).note(
            "change: vcs has no numbered pending changelists; the pending set is the index (`p4 add`/`p4 reconcile` fill it) and the description is given at `p4 submit -d`",
        )),
        "verify" => Ok(Plan::default().vcs(&["fsck"])),
        "depots" => Ok(Plan::default().vcs(&["zone", "list"])),
        "client" | "workspace" => {
            let o = Opts::parse(cmd, a, "", "oSdfi", "vcs zone list; vcs sparse list")?;
            if !o.has('o') {
                return nomap(
                    &format!("{cmd} (edit the view)"),
                    "the client view is the zone subscription plus the sparse set; there is no spec form",
                    "vcs zone add|remove <zone>; vcs sparse set <zone> <paths...>",
                );
            }
            Ok(Plan::default().vcs(&["zone", "list"]).vcs(&["sparse", "list"]).note(
                "client -o: the view is the zone list (subscribed or not) and the sparse sets; edit it with `vcs zone add|remove` and `vcs sparse set|add|remove`",
            ))
        }
        "clients" | "workspaces" => Ok(Plan::default().vcs(&["worktree", "list"])),
        "sync" => {
            let o = Opts::parse("sync", a, "", "fnq", "vcs sync; vcs fetch")?;
            for f in &o.rest {
                let (_, rev) = split_rev(f)?;
                if let Some(r) = rev {
                    return nomap(
                        &format!("sync {f}"),
                        &format!("a workspace cannot be synced to @{r}; vcs follows branches"),
                        "vcs switch <branch>; vcs snapshot checkout <name>; vcs cat-file -p <rev>:<file>",
                    );
                }
            }
            let mut plan = Plan::default();
            if o.has('n') {
                plan = plan
                    .vcs(&["fetch"])
                    .note("sync -n: vcs has no dry run for sync; `vcs fetch` only downloads and updates remote-tracking refs, local branches and files are untouched");
            } else {
                plan = plan.vcs(&["sync"]);
                if o.has('f') {
                    plan = plan.vcs(&["restore", "."]).note("sync -f: files are re-written from the index, local edits are discarded");
                }
            }
            if !o.rest.is_empty() {
                plan = plan.note("sync <paths>: paths are ignored; vcs sync updates every subscribed zone (use `vcs sparse` to limit what is materialized)");
            }
            Ok(plan)
        }
        "reopen" => nomap("reopen", "files are not split into numbered pending changelists", "vcs add <files> / vcs restore --staged <files>; vcs shelve <name>"),
        "obliterate" => nomap("obliterate", "history is content-addressed and immutable; unreachable data is removed by gc", "vcs gc --dry-run"),
        "jobs" | "job" | "fix" | "fixes" => nomap(cmd, "vcs has no issue tracker", "put issue ids in the commit message and use `vcs log --grep <id>`"),
        "protect" | "groups" | "group" | "user" | "passwd" | "counters" | "triggers" | "monitor" | "admin" => nomap(
            cmd,
            "permissions and accounts are configured on vcs-server (tokens, roles), not by a client command",
            "see docs/server.md; vcs hook list for client-side triggers",
        ),
        other => msg(format!(
            "p4 {other}: not an everyday command in the map; nothing was run\n\nSee `vcs p4 --list` for every mapped command, or run the native command directly (`vcs --help`)"
        )),
    }
}

/// `-du`, `-dw` style diff flags are written `-du`: the getopt parser sees d as unknown, so accept `-du`/`-dn`.
fn fix_diff_flags(a: &[String]) -> Vec<String> {
    a.iter()
        .filter(|x| !(x.starts_with("-d") && x.len() <= 3 && x != &"-d"))
        .cloned()
        .collect()
}

fn is_repo_root(p: &str) -> bool {
    open().map(|(r, _)| r.worktree().to_string_lossy() == p).unwrap_or(false)
}

// ---------- execution ----------

fn run_vcs(argv: &[String]) -> Result<(), Error> {
    use clap::Parser;
    let mut full = vec![s("vcs")];
    full.extend(argv.iter().cloned());
    let cli = crate::Cli::try_parse_from(&full)
        .map_err(|e| Error::Msg(format!("internal: translated command `{}` was rejected: {e}", argv.join(" "))))?;
    crate::run_lazy(cli)
}

fn only_prefixes(repo: &Repo, cwd: &Path, only: &[String]) -> Result<Vec<String>, Error> {
    only.iter().map(|p| repo.rel_path(cwd, p)).collect()
}

fn covered(only: &[String], p: &str) -> bool {
    only.is_empty() || only.iter().any(|u| u.is_empty() || p == u || p.starts_with(&format!("{u}/")))
}

fn explain(st: &Step) -> String {
    match st {
        Step::Vcs(v) => format!("vcs {}", v.join(" ")),
        Step::Say(m) => format!("(print) {m}"),
        Step::Stage {
            edits,
            deletes,
            adds,
            preview,
            ..
        } => {
            let mut w = vec![];
            if *adds {
                w.push("new files");
            }
            if *edits {
                w.push("edited files");
            }
            if *deletes {
                w.push("deleted files");
            }
            if *preview {
                format!("(list, change nothing) {}", w.join(", "))
            } else {
                format!("vcs add <{}>", w.join(", "))
            }
        }
        Step::Revert(p) => format!("vcs restore --staged {0}; vcs restore {0}", p.join(" ")),
        Step::Diff {
            staged, unstaged, names, ..
        } => {
            let mut v = vec![];
            if *staged {
                v.push(if *names {
                    "vcs diff --staged --name-only"
                } else {
                    "vcs diff --staged"
                });
            }
            if *unstaged {
                v.push(if *names { "vcs diff --name-only" } else { "vcs diff" });
            }
            format!("{} (limited to the files)", v.join("; "))
        }
        Step::Diff2(a, b) => format!("(unified diff of {a} and {b})"),
        Step::Opened(_) => "vcs status (staged and unstaged changes of tracked files, no untracked)".into(),
        Step::Where(_) => "(print zone path, worktree path and absolute path)".into(),
        Step::Info => "(print identity, root and remote)".into(),
        Step::ResolveAll(f) => format!("vcs resolve <each unresolved file> {f}"),
        Step::PublishIfRemote => "vcs publish (only if a remote is configured)".into(),
    }
}

fn exec(st: &Step, quiet: bool) -> Result<(), Error> {
    match st {
        Step::Vcs(v) => {
            if !quiet {
                eprintln!("+ vcs {}", v.join(" "));
            }
            run_vcs(v)
        }
        Step::Say(m) => {
            println!("{m}");
            Ok(())
        }
        Step::Stage {
            edits,
            deletes,
            adds,
            only,
            preview,
        } => {
            let (repo, cwd) = open()?;
            let only = only_prefixes(&repo, &cwd, only)?;
            let stat = repo.status_auto(Default::default())?;
            let mut found: Vec<(&str, String)> = vec![];
            for z in &stat.zones {
                for c in &z.unstaged {
                    if !covered(&only, &c.path) {
                        continue;
                    }
                    match c.kind {
                        ChangeKind::Modified if *edits => found.push(("edit", c.path.clone())),
                        ChangeKind::Deleted if *deletes => found.push(("delete", c.path.clone())),
                        ChangeKind::Added if *adds => found.push(("add", c.path.clone())),
                        _ => {}
                    }
                }
            }
            if *adds {
                for u in &stat.untracked {
                    if covered(&only, u.trim_end_matches('/')) {
                        found.push(("add", u.clone()));
                    }
                }
            }
            if found.is_empty() {
                println!("nothing to {}", if *preview { "reconcile" } else { "stage" });
                return Ok(());
            }
            if *preview {
                for (k, p) in &found {
                    println!("{} - would {k}", depot_path(&repo, p));
                }
                return Ok(());
            }
            let mut v = vec![s("add")];
            v.extend(found.iter().map(|(_, p)| repo.worktree().join(p).to_string_lossy().into_owned()));
            exec(&Step::Vcs(v), quiet)
        }
        Step::Revert(paths) => {
            for p in paths {
                exec(&Step::Vcs(vec![s("restore"), s("--staged"), p.clone()]), quiet)?;
                match exec(&Step::Vcs(vec![s("restore"), p.clone()]), quiet) {
                    Err(Error::Msg(m)) if m.contains("did not match any tracked files") => {}
                    r => r?,
                }
            }
            Ok(())
        }
        Step::Diff {
            only,
            names,
            staged,
            unstaged,
        } => {
            let (repo, cwd) = open()?;
            let only = only_prefixes(&repo, &cwd, only)?;
            let mut seen = std::collections::BTreeSet::new();
            for (want, is_staged) in [(*staged, true), (*unstaged, false)] {
                if !want {
                    continue;
                }
                for (path, block) in diff_blocks(&repo.diff(is_staged)?) {
                    if !covered(&only, &path) {
                        continue;
                    }
                    if *names {
                        if seen.insert(path.clone()) {
                            println!("{}", depot_path(&repo, &path));
                        }
                    } else {
                        print!("{block}");
                    }
                }
            }
            Ok(())
        }
        Step::Diff2(a, b) => {
            let (repo, cwd) = open()?;
            let side = |arg: &String| -> Result<(String, Option<Vec<u8>>), Error> {
                let (p, rev) = path_arg(arg)?;
                let rel = repo.rel_path(&cwd, &p)?;
                let files = repo.rev_files(rev.as_deref().unwrap_or("HEAD"))?;
                let data = match files.get(&rel) {
                    Some((_, id)) => Some(repo.content(id)?),
                    None => None,
                };
                Ok((rel, data))
            };
            let ((pa, da), (pb, db)) = (side(a)?, side(b)?);
            if da.is_none() && db.is_none() {
                return msg("p4 diff2: neither file exists in the given revisions");
            }
            if da == db {
                println!("==== {pa} - {pb} ==== identical");
            } else {
                print!(
                    "{}",
                    vcs_repo::diff::unified(if db.is_some() { &pb } else { &pa }, da.as_deref(), db.as_deref())
                );
            }
            Ok(())
        }
        Step::Opened(only) => {
            let (repo, cwd) = open()?;
            let only = only_prefixes(&repo, &cwd, only)?;
            let stat = repo.status_auto(Default::default())?;
            let mut n = 0;
            for z in &stat.zones {
                for (list, staged) in [(&z.staged, true), (&z.unstaged, false)] {
                    for c in list {
                        if !covered(&only, &c.path) {
                            continue;
                        }
                        let act = match c.kind {
                            ChangeKind::Added => "add",
                            ChangeKind::Modified => "edit",
                            ChangeKind::Deleted => "delete",
                        };
                        println!(
                            "{} - {act} default change{}",
                            depot_path(&repo, &c.path),
                            if staged {
                                ""
                            } else {
                                " (not staged: `p4 reconcile` or `p4 add` it)"
                            }
                        );
                        n += 1;
                    }
                }
            }
            if n == 0 {
                println!("File(s) not opened on this client.");
            }
            Ok(())
        }
        Step::Where(paths) => {
            let (repo, cwd) = open()?;
            let client = repo
                .worktree()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            for p in paths {
                let rel = repo.rel_path(&cwd, p)?;
                let abs = repo.worktree().join(&rel);
                println!("{} //{client}/{rel} {}", depot_path(&repo, &rel), abs.display());
            }
            Ok(())
        }
        Step::Info => {
            let (repo, cwd) = open()?;
            let cfg = crate::config::Config::load()?;
            let client = repo
                .worktree()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            println!(
                "User name: {}",
                cfg.get("user.name").unwrap_or_else(|| "(user.name not set)".into())
            );
            println!(
                "User email: {}",
                cfg.get("user.email").unwrap_or_else(|| "(user.email not set)".into())
            );
            println!("Client name: {client}");
            println!("Client root: {}", repo.worktree().display());
            println!("Current directory: {}", cwd.display());
            println!(
                "Server address: {}",
                repo.remote_url()?.unwrap_or_else(|| "(none: local only)".into())
            );
            println!("Server version: vcs {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Step::ResolveAll(flag) => {
            let (repo, _) = open()?;
            let stat = repo.status_auto(Default::default())?;
            let Some(m) = stat.merging else {
                return msg("no file(s) to resolve: no merge in progress");
            };
            let un: Vec<String> = m.unresolved().iter().map(|c| c.path.clone()).collect();
            if un.is_empty() {
                println!("no file(s) to resolve");
            }
            for p in un {
                let abs = repo.worktree().join(p).to_string_lossy().into_owned();
                exec(&Step::Vcs(vec![s("resolve"), abs, s(flag)]), quiet)?;
            }
            Ok(())
        }
        Step::PublishIfRemote => {
            let (repo, _) = open()?;
            if need_remote(&repo).is_ok() {
                exec(&Step::Vcs(vec![s("publish")]), quiet)
            } else {
                eprintln!("note: no remote configured, so the commit is local only (`vcs remote set <url>` then `vcs publish` makes it visible to others)");
                Ok(())
            }
        }
    }
}

/// `(repo-relative path, block text)` for every file of a `Repo::diff` rendering.
fn diff_blocks(d: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    for line in d.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix("diff --vcs a/") {
            let rest = rest.trim_end_matches('\n');
            let n = rest.len().saturating_sub(3) / 2;
            out.push((rest[..n].to_owned(), String::new()));
        }
        if let Some(last) = out.last_mut() {
            last.1.push_str(line);
        }
    }
    out
}

fn print_list() {
    println!("p4 command\tvcs command\tstatus\ttest");
    for row in TABLE {
        println!("{}\t{}\t{}\t{}", row.p4, row.vcs, row.status.name(), row.test);
    }
}

pub fn run(a: &P4Args) -> Result<(), Error> {
    let mut args = a.args.clone();
    let (mut explain_only, mut quiet) = (false, false);
    while let Some(first) = args.first() {
        match first.as_str() {
            "--explain" => explain_only = true,
            "--list" | "--help" | "-h" | "help" => {
                print_list();
                return Ok(());
            }
            "-q" => quiet = true,
            "-d" => {
                if args.len() < 2 {
                    return msg("p4 -d needs a directory");
                }
                let dir = args.remove(1);
                std::env::set_current_dir(&dir).map_err(|e| Error::Msg(format!("cannot change to directory {dir}: {e}")))?;
            }
            x if x.starts_with('-') => {
                return nomap(
                    &format!("global flag {x}"),
                    "there is no server, user or client to select: the workspace is the directory you run in (-d DIR), the identity is user.name/user.email",
                    "vcs -C <dir> <command>; vcs config set user.name <name>",
                )
            }
            _ => break,
        }
        args.remove(0);
    }
    if args.is_empty() {
        print_list();
        return Ok(());
    }
    let cmd = args.remove(0);
    let plan = translate(&cmd, &args)?;
    if explain_only {
        for n in &plan.notes {
            println!("note: {n}");
        }
        for st in &plan.steps {
            println!("{}", explain(st));
        }
        return Ok(());
    }
    for n in &plan.notes {
        eprintln!("note: {n}");
    }
    for st in &plan.steps {
        exec(st, quiet)?;
    }
    crate::lockable_cmd::refresh(Path::new("."));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn getopt_handles_attached_combined_and_unknown() {
        let o = Opts::parse("changes", &v(&["-m5", "-ls", "pending", "x"]), "ms", "l", "n").unwrap();
        assert_eq!(o.val('m'), Some("5"));
        assert_eq!(o.val('s'), Some("pending"));
        assert!(o.has('l'));
        assert_eq!(o.rest, v(&["x"]));
        assert!(Opts::parse("changes", &v(&["-Z"]), "m", "l", "n").is_err());
        assert!(Opts::parse("changes", &v(&["--x"]), "m", "l", "n").is_err());
    }

    #[test]
    fn revisions() {
        assert_eq!(split_rev("a.txt").unwrap(), ("a.txt".into(), None));
        assert_eq!(split_rev("a.txt#head").unwrap(), ("a.txt".into(), None));
        assert_eq!(split_rev("a.txt@v1").unwrap(), ("a.txt".into(), Some("v1".into())));
        assert!(split_rev("a.txt@123").is_err());
        assert!(split_rev("a.txt#3").is_err());
        assert_eq!(unescape("a%40b%23c"), "a@b#c");
    }

    #[test]
    fn wildcards() {
        assert_eq!(strip_wild("...").unwrap(), ".");
        assert_eq!(strip_wild("src/...").unwrap(), "src");
        assert!(strip_wild("a/.../b").is_err());
    }

    #[test]
    fn diff_block_split() {
        let d = "diff --vcs a/x.txt b/x.txt\n--- a/x.txt\n+++ b/x.txt\ndiff --vcs a/d/y b/d/y\nnew file\n";
        let b = diff_blocks(d);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].0, "x.txt");
        assert_eq!(b[1].0, "d/y");
    }

    #[test]
    fn every_row_has_a_test_and_a_status() {
        for row in TABLE {
            assert!(!row.test.is_empty() && !row.p4.is_empty() && !row.vcs.is_empty());
        }
    }
}
