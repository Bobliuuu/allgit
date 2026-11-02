//! `vcs p4 <p4 args...>`: every everyday Perforce command mapped one to one onto a vcs command,
//! in process, with no `p4` binary and no server. The map is documented in docs/p4-to-vcs.md and
//! printed by `vcs p4 --list`.
//!
//! The front-end only translates arguments into [`Step`]s (vcs argv, or a few small computed
//! steps that need the repository), prints the vcs command it runs (stderr, unless `-q`), and
//! runs it through the normal CLI. `vcs p4 --explain <p4 args>` prints the translation and runs
//! nothing. Where p4 and vcs differ in meaning, a `note:` line says so; where there is no
//! equivalent the command fails and names the native vcs command.

#[path = "p4changes.rs"]
mod p4changes;

use crate::{need_remote, open};
use clap::Args;
use p4changes::Ledger;
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
    r(
        "sync path@rev / @label / @N",
        "(computed) write the files of that commit into the workspace",
        D,
        "sync_rev",
    ),
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
    r(
        "copy source-branch",
        "(computed) make the tracked files equal to that branch, staged",
        D,
        "copy",
    ),
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
    r(
        "change -m desc / -i / -d N / -o N",
        "(local ledger) numbered pending changelists",
        D,
        "change_numbered",
    ),
    r(
        "add / edit / delete / revert / opened -c N",
        "(local ledger) open files in changelist N",
        D,
        "numbered_open",
    ),
    r("reopen -c N", "(local ledger) move files between changelists", D, "reopen"),
    r("submit -c N", "add (the changelist's files); commit; publish", D, "submit_numbered"),
    r("describe N / @N", "show <commit recorded for N>", D, "changelist_numbers"),
    r("verify", "fsck", R, "verify"),
    r("obliterate / jobs / fix", "(none)", M, "missing_cmds"),
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
    /// open files in numbered changelist `n` (ledger only; nothing is staged)
    Open {
        n: u64,
        action: &'static str,
        paths: Vec<String>,
    },
    RevertChange(u64, Vec<String>),
    OpenedChange(u64),
    Reopen {
        n: Option<u64>,
        paths: Vec<String>,
    },
    ChangeNew(String),
    ChangeInput,
    ChangeDelete(u64),
    ChangeShow(u64),
    PendingList,
    NumberedList,
    CheckSubmit(u64),
    MarkHeads,
    Record(Option<u64>),
    SyncRev {
        rev: String,
        only: Vec<String>,
        preview: bool,
    },
    Copy {
        branch: String,
        zone: Option<String>,
        preview: bool,
    },
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
    /// `-c <change>` for commands that cannot take a numbered changelist.
    fn default_change_only(&self) -> Result<(), Error> {
        match self.val('c') {
            None | Some("default") => Ok(()),
            Some(n) => nomap(
                &self.cmd,
                &format!(
                    "pending changelist {n}: `p4 {}` does not take -c N (add, edit, delete, revert, opened, reopen, submit do; the default changelist is the index)",
                    self.cmd
                ),
                "vcs p4 reopen -c default <files>; vcs add <files> to stage a subset, `vcs shelve <name>` to park work",
            ),
        }
    }
    /// `-c <change>`: `None` for the default changelist (the index), `Some(n)` for an existing
    /// numbered pending changelist of the local ledger (docs/p4-to-vcs.md).
    fn change_id(&self) -> Result<Option<u64>, Error> {
        match self.val('c') {
            None | Some("default") => Ok(None),
            Some(n) => {
                let Ok(num) = n.parse::<u64>() else {
                    return msg(format!("p4 {}: bad changelist {n:?} (a number or `default`)", self.cmd));
                };
                let (repo, _) = open()?;
                if Ledger::load(repo.worktree()).pending.contains_key(&num) {
                    Ok(Some(num))
                } else {
                    nomap(
                        &self.cmd,
                        &format!("pending changelist {n} does not exist (numbered changelists are kept in this repository's ledger; create one with `vcs p4 change -m <description>`)"),
                        "vcs p4 change -m <description>",
                    )
                }
            }
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
        let n: u64 = r.parse().unwrap_or(0);
        let (repo, _) = open()?;
        return match Ledger::load(repo.worktree()).submitted.get(&n) {
            Some(id) => Ok(id.clone()),
            None => nomap(
                "revision",
                &format!("changelist number {r}: not a changelist submitted through `vcs p4 submit` in this repository (vcs commit ids are content hashes, there is no global counter; `vcs p4 changes` lists the numbered ones)"),
                "vcs log --oneline (ids), then use an id prefix, a tag or a branch name",
            ),
        };
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

const OPEN_NOTE: &str =
    "numbered changelist: the files are recorded in this repository's ledger (.vcs/p4-changes), nothing is staged until `p4 submit -c N`";

// ---------- translation ----------

fn translate(cmd: &str, a: &[String]) -> Result<Plan, Error> {
    match cmd {
        "add" => {
            let o = Opts::parse("add", a, "c", "nf", "vcs add --help")?;
            o.need_files()?;
            let paths = plain_paths(&o.rest)?;
            if let Some(n) = o.change_id()? {
                return Ok(Plan::default().step(Step::Open { n, action: "add", paths }).note(OPEN_NOTE));
            }
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
            let change = o.change_id()?;
            o.need_files()?;
            let (repo, cwd) = open()?;
            let attrs = repo.attrs()?;
            let mut plan = Plan::default();
            let mut locks = vec![];
            let all = plain_paths(&o.rest)?;
            for f in all.clone() {
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
            if let Some(n) = change {
                plan = plan.step(Step::Open { n, action: "edit", paths: all }).note(OPEN_NOTE);
            }
            Ok(plan)
        }
        "delete" => {
            let o = Opts::parse("delete", a, "c", "kv", "vcs rm --help")?;
            let change = o.change_id()?;
            o.need_files()?;
            if let Some(n) = change {
                if o.has('k') {
                    return nomap("delete -k", "-k (keep the file) cannot be recorded in a changelist", "vcs rm --cached <files>");
                }
                return Ok(Plan::default()
                    .step(Step::Open { n, action: "delete", paths: plain_paths(&o.rest)? })
                    .note(OPEN_NOTE));
            }
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
            let change = o.change_id()?;
            if o.has('k') {
                return nomap("revert -k", "there is no have list to keep", "vcs restore --staged <files> (unstage only)");
            }
            if o.has('a') {
                return Ok(Plan::default().step(Step::Say(
                    "revert -a: nothing to do (vcs never stages a file whose content equals HEAD, so there are no opened-but-unchanged files)".into(),
                )));
            }
            if let Some(n) = change {
                return Ok(Plan::default().step(Step::RevertChange(n, plain_paths(&o.rest)?)).note(
                    "revert -c N: the files of changelist N (or the ones named) are restored and leave the changelist",
                ));
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
            if o.val('C').is_some() || o.val('u').is_some() {
                return nomap("opened -C/-u", "no clients or per-user open lists", "vcs status");
            }
            if let Some(n) = o.change_id()? {
                return Ok(Plan::default().step(Step::OpenedChange(n)));
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
            let change = o.change_id()?;
            let only = plain_paths(&o.rest)?;
            if let Some(n) = change {
                if !only.is_empty() {
                    return nomap("submit -c N <files>", "a numbered changelist is submitted whole", "vcs p4 reopen -c default <files>, or `vcs p4 revert -c N <files>`");
                }
                let (repo, _) = open()?;
                let l = Ledger::load(repo.worktree());
                let ch = &l.pending[&n];
                if ch.files.is_empty() {
                    return msg(format!("p4 submit: changelist {n} has no open files"));
                }
                let desc = o.val('d').map(str::to_owned).or_else(|| Some(ch.desc.clone()).filter(|d| !d.trim().is_empty()));
                let Some(desc) = desc else {
                    return msg(format!("p4 submit: changelist {n} has no description: give -d <msg> or set it with `vcs p4 change -i`"));
                };
                let mut add = vec![s("add")];
                add.extend(ch.files.keys().map(|f| repo.worktree().join(f).to_string_lossy().into_owned()));
                return Ok(Plan::default()
                    .step(Step::CheckSubmit(n))
                    .step(Step::MarkHeads)
                    .argv(add)
                    .argv(vec![s("commit"), s("-m"), desc])
                    .step(Step::PublishIfRemote)
                    .step(Step::Record(Some(n)))
                    .note("submit -c N: stages exactly the files of changelist N (adds, edits, deletions), commits them, publishes when a remote is configured, and records N -> commit so `@N` works"));
            }
            let mut commit = vec![s("commit")];
            if let Some(m) = o.val('d') {
                commit.extend([s("-m"), m.to_owned()]);
            }
            let mut plan = Plan::default()
                .step(Step::MarkHeads)
                .step(Step::Stage {
                    adds: false,
                    edits: true,
                    deletes: true,
                    only: if only.is_empty() { vec![s(".")] } else { only.clone() },
                    preview: false,
                })
                .argv(commit)
                .step(Step::PublishIfRemote)
                .step(Step::Record(None))
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
                    return Ok(Plan::default()
                        .step(Step::PendingList)
                        .vcs(&["status"])
                        .note("changes -s pending: numbered changelists come from the local ledger; the default changelist is the index, shown by `vcs status`"))
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
            Ok(Plan::default()
                .argv(v)
                .step(Step::NumberedList)
                .note("changes are listed as commit ids; the changelist numbers `vcs p4 submit` assigned in this repository are listed after them"))
        }
        "describe" => {
            let o = Opts::parse("describe", a, "", "sduSa", "vcs show <rev>")?;
            if o.has('S') {
                return nomap("describe -S", "shelved diffs cannot be shown without applying them", "vcs unshelve <shelf>; vcs diff --staged");
            }
            if o.rest.is_empty() {
                return msg("p4 describe: no change given (a changelist number, commit id prefix, tag or branch)");
            }
            let mut plan = Plan::default();
            for r in &o.rest {
                if let Ok(n) = r.parse::<u64>() {
                    let (repo, _) = open()?;
                    if Ledger::load(repo.worktree()).pending.contains_key(&n) {
                        plan = plan.step(Step::ChangeShow(n));
                        continue;
                    }
                }
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
        "copy" => {
            let o = Opts::parse("copy", a, "S", "n", "vcs merge <branch>")?;
            let src = o.val('S').map(str::to_owned).or_else(|| o.rest.first().cloned());
            let Some(src) = src else {
                return msg("p4 copy: name the source branch: `p4 copy //depot/dev/...` or `-S //stream`");
            };
            let (zone, branch) = branch_of(&src)?;
            let target = if o.val('S').is_some() { o.rest.first() } else { o.rest.get(1) };
            if o.rest.len() > if o.val('S').is_some() { 1 } else { 2 } {
                return msg("p4 copy: too many arguments (source [target])");
            }
            if let Some(tg) = target {
                let (tz, tb) = branch_of(tg)?;
                let (repo, _) = open()?;
                let zid = match tz.as_deref().or(zone.as_deref()) {
                    Some(z) => repo.zones()?.resolve(z).unwrap_or_else(|| ROOT_ZONE.to_owned()),
                    None => ROOT_ZONE.to_owned(),
                };
                if repo.current_branch(&zid)?.as_deref() != Some(tb.as_str()) {
                    return nomap(
                        "copy <source> <target>",
                        "the target is always the checked-out branch of the zone",
                        "vcs switch <target-branch>; vcs p4 copy <source-branch>",
                    );
                }
            }
            Ok(Plan::default().step(Step::Copy { branch, zone, preview: o.has('n') }).note(
                "p4 copy makes the target tree equal to the source and stages it; no integration record is kept, so a later `p4 merge` sees plain edits. Whole branch only, no path mappings. Submit with `p4 submit`",
            ))
        }
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
        "change" | "changelist" => {
            let o = Opts::parse(cmd, a, "m", "odi", "vcs p4 change -m <description>")?;
            let num = |o: &Opts| -> Result<u64, Error> {
                match o.rest.first().map(|n| n.parse::<u64>()) {
                    Some(Ok(n)) => Ok(n),
                    _ => msg(format!("p4 {}: give a changelist number", o.cmd)),
                }
            };
            if o.has('d') {
                return Ok(Plan::default().step(Step::ChangeDelete(num(&o)?)));
            }
            if o.has('i') {
                return Ok(Plan::default()
                    .step(Step::ChangeInput)
                    .note("change -i: reads a spec from stdin; only `Change:` (new or a number) and `Description:` are used, `Files:` is ignored (use -c N on add/edit/delete/reopen)"));
            }
            if let Some(d) = o.val('m') {
                return Ok(Plan::default()
                    .step(Step::ChangeNew(d.to_owned()))
                    .note("change -m is a vcs extension (p4 opens an editor): it creates numbered pending changelist N in the local ledger"));
            }
            if o.has('o') && !o.rest.is_empty() {
                return Ok(Plan::default().step(Step::ChangeShow(num(&o)?)));
            }
            Ok(Plan::default().step(Step::PendingList).vcs(&["status"]).note(
                "change: the default changelist is the index (`vcs status`); numbered pending changelists are created with `change -m <description>` (or `-i`), filled with `-c N` on add/edit/delete/reopen and submitted with `submit -c N`; they live in this repository only (.vcs/p4-changes)",
            ))
        }
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
            let mut rev: Option<String> = None;
            let mut only = vec![];
            for f in &o.rest {
                let (p, r) = path_arg(f)?;
                if let Some(r) = r {
                    if rev.as_ref().is_some_and(|x| *x != r) {
                        return msg("p4 sync: all paths must use the same revision");
                    }
                    rev = Some(r);
                }
                only.push(if p.is_empty() { s(".") } else { p });
            }
            if let Some(rev) = rev {
                if o.has('f') {
                    return nomap("sync -f @rev", "-f is not combined with a revision", "vcs p4 sync <path>@<rev>");
                }
                return Ok(Plan::default().step(Step::SyncRev { rev, only, preview: o.has('n') }).note(
                    "sync @rev writes the files of that commit into the workspace; HEAD does not move, so `vcs status` shows them as edits and `vcs restore .` returns to HEAD. It refuses over local changes. Revision: tag, branch, commit id prefix or a changelist number recorded by `p4 submit`",
                ));
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
        "reopen" => {
            let o = Opts::parse("reopen", a, "ct", "", "vcs p4 reopen -c <N|default> <files>")?;
            if o.val('t').is_some() {
                return nomap("reopen -t", "vcs has no file types per open file (use .vcsattributes)", "vcs p4 reopen -c <N> <files>");
            }
            if o.val('c').is_none() {
                return msg("p4 reopen: -c <changelist|default> is required");
            }
            let n = o.change_id()?;
            o.need_files()?;
            Ok(Plan::default()
                .step(Step::Reopen { n, paths: plain_paths(&o.rest)? })
                .note("reopen: moves files between numbered changelists of the local ledger; `-c default` hands them back to the default changelist (the index)"))
        }
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
        Step::Open { n, action, paths } => format!("(ledger) open {} for {action} in changelist {n}", paths.join(" ")),
        Step::RevertChange(n, p) => format!("(ledger) vcs restore --staged/restore the files of changelist {n} {}", p.join(" ")),
        Step::OpenedChange(n) => format!("(ledger) list the files of changelist {n}"),
        Step::Reopen { n, paths } => match n {
            Some(n) => format!("(ledger) move {} to changelist {n}", paths.join(" ")),
            None => format!("(ledger) move {} back to the default changelist", paths.join(" ")),
        },
        Step::ChangeNew(d) => format!("(ledger) new numbered changelist {d:?}"),
        Step::ChangeInput => "(ledger) read a change spec on stdin".into(),
        Step::ChangeDelete(n) => format!("(ledger) delete empty changelist {n}"),
        Step::ChangeShow(n) => format!("(ledger) show changelist {n}"),
        Step::PendingList => "(ledger) list pending numbered changelists".into(),
        Step::NumberedList => "(ledger) list numbered submitted changelists".into(),
        Step::CheckSubmit(n) => format!("(check) nothing is staged outside changelist {n}"),
        Step::MarkHeads => "(remember the branch heads)".into(),
        Step::Record(Some(n)) => format!("(ledger) record changelist {n} -> the new commit"),
        Step::Record(None) => "(ledger) assign the next changelist number to the new commit".into(),
        Step::SyncRev { rev, only, preview } => format!(
            "({}) write the files of {rev} under {} into the workspace",
            if *preview {
                "list, change nothing"
            } else {
                "refuses over local changes"
            },
            only.join(" ")
        ),
        Step::Copy { branch, preview, .. } => format!(
            "({}) make the tracked files equal to branch {branch}, then vcs add",
            if *preview {
                "list, change nothing"
            } else {
                "refuses over local changes"
            }
        ),
    }
}

static HEADS_BEFORE: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

fn heads(repo: &Repo) -> Vec<(String, String)> {
    let Ok(ws) = repo.workspace() else { return vec![] };
    ws.subs
        .keys()
        .filter_map(|z| repo.head_commit(z).ok().flatten().map(|id| (z.clone(), id.to_hex())))
        .collect()
}

/// Subscribed zones in which `rev` resolves (restricted to `zone` when given).
fn rev_zones(repo: &Repo, rev: &str, zone: Option<&str>) -> Result<std::collections::BTreeSet<String>, Error> {
    let only = match zone {
        Some(z) => Some(repo.zones()?.resolve(z).ok_or_else(|| Error::Msg(format!("unknown zone {z}")))?),
        None => None,
    };
    Ok(repo
        .workspace()?
        .subs
        .keys()
        .filter(|z| only.as_ref().is_none_or(|o| o == *z) && repo.resolve_rev(z, rev).is_ok())
        .cloned()
        .collect())
}

/// Paths with staged or unstaged changes that `pred` selects.
fn dirty(repo: &Repo, pred: &dyn Fn(&str) -> bool) -> Result<Vec<String>, Error> {
    let stat = repo.status_tracked_auto(Default::default())?;
    let mut out = vec![];
    for z in &stat.zones {
        for c in z.staged.iter().chain(z.unstaged.iter()) {
            if pred(&c.path) && !out.contains(&c.path) {
                out.push(c.path.clone());
            }
        }
    }
    Ok(out)
}

fn write_tree_file(repo: &Repo, rel: &str, mode: vcs_objects::EntryMode, id: &vcs_objects::ObjectId) -> Result<bool, Error> {
    if mode == vcs_objects::EntryMode::Symlink {
        return Ok(false);
    }
    let data = repo.content(id)?;
    let abs = repo.worktree().join(rel);
    if std::fs::read(&abs).ok().as_deref() == Some(&data[..]) {
        return Ok(false);
    }
    if let Some(d) = abs.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::Msg(format!("{}: {e}", d.display())))?;
    }
    if abs.symlink_metadata().is_ok() {
        let _ = std::fs::remove_file(&abs);
    }
    std::fs::write(&abs, &data).map_err(|e| Error::Msg(format!("{}: {e}", abs.display())))?;
    #[cfg(unix)]
    if mode == vcs_objects::EntryMode::Executable {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&abs, std::fs::Permissions::from_mode(0o755));
    }
    Ok(true)
}

/// Write the tree of `rev` into the workspace for the paths `sel` selects, delete tracked files
/// the revision lacks (only in zones where `rev` resolves). Returns the changed paths.
fn materialize(
    repo: &Repo,
    rev: &str,
    zone: Option<&str>,
    sel: &dyn Fn(&str) -> bool,
    preview: bool,
) -> Result<Vec<(String, bool)>, Error> {
    let zones = rev_zones(repo, rev, zone)?;
    if zones.is_empty() {
        return msg(format!("unknown revision {rev:?} in any subscribed zone"));
    }
    let zmap = repo.zones()?;
    let in_scope = |p: &str| sel(p) && zones.contains(zmap.owner(p));
    let files = repo.rev_files(rev)?;
    let bad = dirty(repo, &in_scope)?;
    if !bad.is_empty() && !preview {
        return msg(format!(
            "refusing to overwrite local changes: {} (commit them, or `vcs p4 revert` / `vcs restore`)",
            bad.join(", ")
        ));
    }
    let mut changed = vec![];
    for (path, (mode, id)) in &files {
        if !in_scope(path) {
            continue;
        }
        let differs = preview && std::fs::read(repo.worktree().join(path)).ok() != Some(repo.content(id)?);
        if preview {
            if differs {
                changed.push((path.clone(), false));
            }
        } else if write_tree_file(repo, path, *mode, id)? {
            changed.push((path.clone(), false));
        }
    }
    for (path, _, _) in repo.tracked_files()? {
        if in_scope(&path) && !files.contains_key(&path) {
            if !preview {
                let _ = std::fs::remove_file(repo.worktree().join(&path));
            }
            changed.push((path, true));
        }
    }
    Ok(changed)
}

fn action_of(k: ChangeKind) -> &'static str {
    match k {
        ChangeKind::Added => "add",
        ChangeKind::Modified => "edit",
        ChangeKind::Deleted => "delete",
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
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
            // The untracked walk is only needed to find files to add.
            let stat = if *adds {
                repo.status_auto(Default::default())?
            } else {
                repo.status_tracked_auto(Default::default())?
            };
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
            if !*preview {
                let l = Ledger::load(repo.worktree());
                found.retain(|(_, p)| l.holder(p).is_none());
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
            let stat = repo.status_tracked_auto(Default::default())?;
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
            let stat = repo.status_tracked_auto(Default::default())?;
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
        Step::Open { n, action, paths } => {
            let (repo, cwd) = open()?;
            let mut l = Ledger::load(repo.worktree());
            let tracked: std::collections::BTreeSet<String> = repo.tracked_files()?.into_iter().map(|t| t.0).collect();
            let mut todo = vec![];
            for p in paths {
                let rel = repo.rel_path(&cwd, p)?;
                let abs = repo.worktree().join(&rel);
                if abs.is_dir() {
                    return msg(format!("p4: {p}: name files, not directories, when opening into changelist {n}"));
                }
                match *action {
                    "add" => {
                        if !abs.is_file() {
                            return msg(format!("p4 add: {p}: file not found in the workspace"));
                        }
                        if tracked.contains(&rel) {
                            return msg(format!("p4 add: {p}: already under version control"));
                        }
                    }
                    "edit" => {
                        if !abs.is_file() {
                            return msg(format!("p4 edit: {p}: file not found in the workspace"));
                        }
                    }
                    _ => {
                        if !tracked.contains(&rel) {
                            return msg(format!("p4 delete: {p}: not under version control"));
                        }
                    }
                }
                if let Some(h) = l.holder(&rel).filter(|h| h != n) {
                    return msg(format!(
                        "p4: {p}: already opened in changelist {h} (move it with `vcs p4 reopen -c {n} {p}`)"
                    ));
                }
                todo.push((rel, abs));
            }
            for (rel, abs) in todo {
                if *action == "delete" {
                    let _ = std::fs::remove_file(&abs);
                }
                println!("{} - opened for {action} in change {n}", depot_path(&repo, &rel));
                l.pending.entry(*n).or_default().files.insert(rel, (*action).to_owned());
            }
            l.save(repo.worktree())
        }
        Step::RevertChange(n, paths) => {
            let (repo, cwd) = open()?;
            let mut l = Ledger::load(repo.worktree());
            let only = only_prefixes(&repo, &cwd, paths)?;
            let members: Vec<String> = l
                .pending
                .get(n)
                .map(|c| c.files.keys().filter(|f| covered(&only, f)).cloned().collect())
                .unwrap_or_default();
            if members.is_empty() {
                println!("File(s) not opened in change {n}.");
                return Ok(());
            }
            let abs: Vec<String> = members
                .iter()
                .map(|m| repo.worktree().join(m).to_string_lossy().into_owned())
                .collect();
            for f in abs {
                // a recorded add was never staged, so there is nothing to unstage
                match exec(&Step::Revert(vec![f]), quiet) {
                    Err(Error::Msg(m)) if m.contains("did not match any tracked files") => {}
                    r => r?,
                }
            }
            if let Some(c) = l.pending.get_mut(n) {
                for m in &members {
                    c.files.remove(m);
                }
            }
            l.save(repo.worktree())
        }
        Step::OpenedChange(n) => {
            let (repo, _) = open()?;
            let l = Ledger::load(repo.worktree());
            let c = l.pending.get(n).ok_or_else(|| Error::Msg(format!("no pending changelist {n}")))?;
            if c.files.is_empty() {
                println!("File(s) not opened in change {n}.");
            }
            for (f, act) in &c.files {
                println!("{} - {act} change {n}", depot_path(&repo, f));
            }
            Ok(())
        }
        Step::Reopen { n, paths } => {
            let (repo, cwd) = open()?;
            let mut l = Ledger::load(repo.worktree());
            let stat = repo.status_tracked_auto(Default::default())?;
            for p in paths {
                let rel = repo.rel_path(&cwd, p)?;
                let from = l.holder(&rel);
                let action = match from {
                    Some(h) => l.pending[&h].files[&rel].clone(),
                    None => {
                        let k = stat
                            .zones
                            .iter()
                            .flat_map(|z| z.staged.iter().chain(z.unstaged.iter()))
                            .find(|c| c.path == rel)
                            .map(|c| c.kind);
                        match k {
                            Some(k) => action_of(k).to_owned(),
                            None => return msg(format!("p4 reopen: {p}: file(s) not opened")),
                        }
                    }
                };
                if let Some(h) = from {
                    if let Some(c) = l.pending.get_mut(&h) {
                        c.files.remove(&rel);
                    }
                }
                let d = depot_path(&repo, &rel);
                match n {
                    Some(n) => {
                        l.pending.entry(*n).or_default().files.insert(rel, action);
                        println!("{d} - reopened; change {n}");
                    }
                    None => println!("{d} - reopened; default change (staged by `p4 add`/`p4 reconcile`/`p4 submit`)"),
                }
            }
            l.save(repo.worktree())
        }
        Step::ChangeNew(desc) => {
            let (repo, _) = open()?;
            let mut l = Ledger::load(repo.worktree());
            let n = l.alloc();
            l.pending.insert(
                n,
                p4changes::Pending {
                    desc: desc.clone(),
                    ..Default::default()
                },
            );
            l.save(repo.worktree())?;
            println!("Change {n} created.");
            Ok(())
        }
        Step::ChangeInput => {
            use std::io::Read;
            let mut spec = String::new();
            std::io::stdin()
                .read_to_string(&mut spec)
                .map_err(|e| Error::Msg(format!("cannot read stdin: {e}")))?;
            let (mut which, mut desc, mut in_desc) = (None::<String>, vec![], false);
            for line in spec.lines() {
                if let Some(v) = line.strip_prefix("Change:") {
                    which = Some(v.trim().to_owned());
                    in_desc = false;
                } else if line.starts_with("Description:") {
                    in_desc = true;
                    let rest = line["Description:".len()..].trim();
                    if !rest.is_empty() {
                        desc.push(rest.to_owned());
                    }
                } else if in_desc && (line.starts_with(char::is_whitespace) || line.is_empty()) {
                    desc.push(line.trim().to_owned());
                } else {
                    in_desc = false;
                }
            }
            let desc = desc.join("\n").trim().to_owned();
            let Some(which) = which else {
                return msg("p4 change -i: the spec has no `Change:` line (`Change: new` or a number)");
            };
            let (repo, _) = open()?;
            let mut l = Ledger::load(repo.worktree());
            let n = if which == "new" {
                let n = l.alloc();
                l.pending.insert(n, Default::default());
                n
            } else {
                let n: u64 = which
                    .parse()
                    .map_err(|_| Error::Msg(format!("p4 change -i: bad Change: {which:?}")))?;
                if !l.pending.contains_key(&n) {
                    return msg(format!("p4 change -i: pending changelist {n} does not exist"));
                }
                n
            };
            if let Some(c) = l.pending.get_mut(&n) {
                c.desc = desc;
            }
            l.save(repo.worktree())?;
            println!("Change {n} {}.", if which == "new" { "created" } else { "updated" });
            Ok(())
        }
        Step::ChangeDelete(n) => {
            let (repo, _) = open()?;
            let mut l = Ledger::load(repo.worktree());
            match l.pending.get(n) {
                None => return msg(format!("p4 change -d: pending changelist {n} does not exist")),
                Some(c) if !c.files.is_empty() => return msg(format!(
                    "p4 change -d: changelist {n} has {} open file(s): `vcs p4 revert -c {n}` or `vcs p4 reopen -c default <files>` first",
                    c.files.len()
                )),
                Some(_) => {}
            }
            l.pending.remove(n);
            l.save(repo.worktree())?;
            println!("Change {n} deleted.");
            Ok(())
        }
        Step::ChangeShow(n) => {
            let (repo, _) = open()?;
            let l = Ledger::load(repo.worktree());
            let c = l.pending.get(n).ok_or_else(|| Error::Msg(format!("no pending changelist {n}")))?;
            println!("Change {n} pending");
            println!("Description:\n\t{}", c.desc.replace('\n', "\n\t"));
            println!("Files:");
            for (f, act) in &c.files {
                println!("\t{}\t# {act}", depot_path(&repo, f));
            }
            Ok(())
        }
        Step::PendingList => {
            let (repo, _) = open()?;
            for (n, c) in &Ledger::load(repo.worktree()).pending {
                println!("Change {n} pending ({} file(s)) '{}'", c.files.len(), first_line(&c.desc));
            }
            Ok(())
        }
        Step::NumberedList => {
            let (repo, _) = open()?;
            let l = Ledger::load(repo.worktree());
            if !l.submitted.is_empty() {
                println!("numbered changelists (assigned by `vcs p4 submit` in this repository):");
                for (n, id) in l.submitted.iter().rev() {
                    println!("Change {n} = commit {}", &id[..id.len().min(12)]);
                }
            }
            Ok(())
        }
        Step::CheckSubmit(n) => {
            let (repo, _) = open()?;
            let l = Ledger::load(repo.worktree());
            let c = l.pending.get(n).ok_or_else(|| Error::Msg(format!("no pending changelist {n}")))?;
            let stat = repo.status_tracked_auto(Default::default())?;
            let extra: Vec<&str> = stat
                .zones
                .iter()
                .flat_map(|z| z.staged.iter())
                .map(|c| c.path.as_str())
                .filter(|p| !c.files.contains_key(*p))
                .collect();
            if !extra.is_empty() {
                return msg(format!(
                    "p4 submit -c {n}: the index holds files outside the changelist ({}): `vcs restore --staged <file>` them, or `vcs p4 submit` them first",
                    extra.join(", ")
                ));
            }
            Ok(())
        }
        Step::MarkHeads => {
            let (repo, _) = open()?;
            *HEADS_BEFORE.lock().unwrap() = heads(&repo);
            Ok(())
        }
        Step::Record(n) => {
            let (repo, _) = open()?;
            let before = HEADS_BEFORE.lock().unwrap().clone();
            let after = heads(&repo);
            let mut changed: Vec<&(String, String)> = after.iter().filter(|h| !before.contains(h)).collect();
            changed.sort_by_key(|(z, _)| z != ROOT_ZONE);
            let Some((_, id)) = changed.first() else {
                eprintln!("note: no new commit, so no changelist number was assigned");
                return Ok(());
            };
            let mut l = Ledger::load(repo.worktree());
            let num = match n {
                Some(n) => {
                    l.pending.remove(n);
                    *n
                }
                None => l.alloc(),
            };
            l.submitted.insert(num, id.clone());
            l.save(repo.worktree())?;
            println!("Change {num} submitted (commit {}).", &id[..id.len().min(12)]);
            if changed.len() > 1 {
                eprintln!(
                    "note: the commit touched {} zones; `@{num}` names the one in {}",
                    changed.len(),
                    changed[0].0
                );
            }
            Ok(())
        }
        Step::SyncRev { rev, only, preview } => {
            let (repo, cwd) = open()?;
            let only = only_prefixes(&repo, &cwd, only)?;
            let changed = materialize(&repo, rev, None, &|p| covered(&only, p), *preview)?;
            if changed.is_empty() {
                println!("File(s) up-to-date at {rev}.");
            }
            for (p, del) in &changed {
                println!(
                    "{} - {} {rev}",
                    depot_path(&repo, p),
                    match (preview, del) {
                        (true, true) => "would delete for",
                        (true, false) => "would update to",
                        (false, true) => "deleted for",
                        (false, false) => "updated to",
                    }
                );
            }
            Ok(())
        }
        Step::Copy { branch, zone, preview } => {
            let (repo, _) = open()?;
            let changed = materialize(&repo, branch, zone.as_deref(), &|_| true, *preview)?;
            if changed.is_empty() {
                println!("branch {branch}: all tracked files already equal; nothing to copy.");
                return Ok(());
            }
            for (p, del) in &changed {
                println!(
                    "{} - {} from {branch}",
                    depot_path(&repo, p),
                    match (preview, del) {
                        (true, true) => "would delete",
                        (true, false) => "would copy",
                        (false, true) => "deleted",
                        (false, false) => "copied",
                    }
                );
            }
            if !*preview {
                let mut v = vec![s("add")];
                v.extend(changed.iter().map(|(p, _)| repo.worktree().join(p).to_string_lossy().into_owned()));
                exec(&Step::Vcs(v), quiet)?;
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
    // Commands that change no file have no read-only bits to restore: skip the second repo open.
    if !READ_ONLY.contains(&cmd.as_str()) {
        crate::lockable_cmd::refresh(Path::new("."));
    }
    Ok(())
}

/// p4 commands whose plan never writes a working-tree file.
const READ_ONLY: &[&str] = &[
    "info",
    "where",
    "opened",
    "filelog",
    "annotate",
    "fstat",
    "print",
    "changes",
    "describe",
    "files",
    "have",
    "users",
    "depots",
    "clients",
    "workspaces",
    "labels",
    "branches",
    "streams",
    "diff",
    "diff2",
    "grep",
    "verify",
    "locks",
];

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
