use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};
use vcs_objects::{ObjectId, Signature};
use vcs_remote::Outcome;
use vcs_repo::{AssistResult, ChangeKind, Error, MergeOutcome, RebaseOutcome, Repo, Resolution, SyncAction, ROOT_ZONE};

mod auth;
mod config;
mod generate;
mod json;
mod logfmt;
mod remote;

/// The assist command: `--assist`, else the opt-in `merge.assist` setting (announced, because file
/// contents are then handed to that command), unless `--no-assist`.
fn effective_assist(flag: Option<&str>, no_assist: bool) -> Result<Option<String>, Error> {
    if no_assist {
        return Ok(None);
    }
    if let Some(c) = flag {
        return Ok(Some(c.to_owned()));
    }
    let cmd = config::Config::load()?.assist_command();
    if let Some(c) = &cmd {
        println!("merge.assist is set: running `{c}` on the conflicting files (use --no-assist to skip)");
    }
    Ok(cmd)
}

/// Run the assistant over the unresolved conflicts and print what happened (proposals only).
fn run_assist(repo: &Repo, cmd: &str, secs: u64) -> Result<(), Error> {
    let rep = repo.assist_conflicts(cmd, std::time::Duration::from_secs(secs))?;
    let mut n = 0;
    for (p, r) in &rep {
        let line = match r {
            AssistResult::Proposed => {
                n += 1;
                "proposal saved".to_owned()
            }
            AssistResult::Skipped(w) => format!("skipped ({w})"),
            AssistResult::Rejected(w) => format!("assistant answer ignored ({w})"),
            AssistResult::TimedOut => format!("assistant timed out after {secs}s"),
            AssistResult::Failed(w) => format!("assistant failed ({w})"),
        };
        println!("  assist: {p}: {line}");
    }
    if n > 0 {
        println!(
            "\n{n} proposal(s) saved, none applied. Review with `vcs conflict proposals`, apply with `vcs resolve <path> --proposal`."
        );
    }
    Ok(())
}

#[derive(Parser, Clone)]
#[command(name = "vcs", version, about = "Native zone-aware version control")]
struct Cli {
    /// Run as if vcs was started in <DIR> (applied before repository discovery)
    #[arg(short = 'C', global = true, value_name = "DIR")]
    chdir: Option<PathBuf>,
    /// Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Clone)]
enum Cmd {
    /// Create a new repository (default: current directory)
    Init { path: Option<PathBuf> },
    /// Stage files or directories (also stages deletions)
    Add { paths: Vec<String> },
    /// Discard working-tree changes (from the index) or unstage them (--staged, from HEAD)
    Restore {
        paths: Vec<String>,
        /// Restore the index from HEAD instead of the working tree from the index
        #[arg(long)]
        staged: bool,
    },
    /// Remove tracked files from the index and working tree
    Rm {
        paths: Vec<String>,
        /// Only stop tracking; keep the files
        #[arg(long)]
        cached: bool,
        /// Remove even files with uncommitted changes
        #[arg(short, long)]
        force: bool,
    },
    /// Move or rename a tracked file or directory (within one zone)
    Mv {
        src: String,
        dst: String,
        /// Overwrite an existing destination
        #[arg(short, long)]
        force: bool,
    },
    /// Commit staged changes in all subscribed zones atomically
    Commit {
        /// Commit message (omit to write it in core.editor / $EDITOR)
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Get, set, list or unset settings (repo .vcs/config, user config, environment)
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Show staged, unstaged and untracked changes per zone
    Status,
    /// Show unstaged changes (or --staged)
    Diff {
        #[arg(long)]
        staged: bool,
        /// Per-file added/deleted line counts instead of the patch
        #[arg(long)]
        stat: bool,
    },
    /// Import a `git fast-export` stream (file, or `-` for stdin) into the root zone
    ImportGit { stream: PathBuf },
    /// Import recorded Perforce output (fixture directory, see docs/import-p4.md) into the root zone.
    /// Never run against a real server.
    ImportP4 {
        dir: PathBuf,
        /// View file mapping depot paths to zone paths
        #[arg(long)]
        view: PathBuf,
        /// Branch of the root zone to import onto
        #[arg(long, default_value = "main")]
        branch: String,
    },
    /// Write a zone branch as a Git fast-import stream (stdout, or --output)
    ExportGit {
        /// Zone (default: the zone containing the current directory)
        #[arg(long)]
        zone: Option<String>,
        /// Branch (default: main)
        branch: Option<String>,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Export the whole branch again and reset the export table (default: only commits not yet exported)
        #[arg(long)]
        full: bool,
        /// Reference commits that came from import-git by their original Git SHA (target = the origin repo)
        #[arg(long)]
        onto_origin: bool,
    },
    /// Show commit history of a zone branch
    Log {
        /// Zone (default: the zone containing the current directory)
        #[arg(long)]
        zone: Option<String>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        /// One line per commit: short id and subject
        #[arg(long)]
        oneline: bool,
        /// Draw the commit graph with ASCII lanes
        #[arg(long)]
        graph: bool,
        /// Only commits whose author name or email contains this text (case-insensitive)
        #[arg(long)]
        author: Option<String>,
        /// When to colour output
        #[arg(long, value_enum, default_value_t = logfmt::ColorWhen::Auto)]
        color: logfmt::ColorWhen,
        /// Branch (default: the checked-out one)
        branch: Option<String>,
        /// Only commits that change these paths (after `--`)
        #[arg(last = true)]
        paths: Vec<String>,
    },
    /// Generate shell completions or man pages
    #[command(subcommand)]
    Gen(generate::GenCmd),
    /// List branches of the current zone, or create one (at the checked-out head)
    Branch {
        name: Option<String>,
        #[arg(long)]
        zone: Option<String>,
        /// Delete the named branch instead of creating it (refuses the checked-out branch)
        #[arg(short = 'd', long)]
        delete: bool,
        /// With --delete: delete even if its commits are not reachable from another branch or snapshot
        #[arg(long, requires = "delete")]
        force: bool,
    },
    /// Set tracked changes aside (push, pop, list, drop); untracked files are left alone
    #[command(subcommand)]
    Stash(StashCmd),
    /// List tags of the current zone, create one (lightweight, or annotated with -m), or delete one with -d
    Tag {
        name: Option<String>,
        /// Revision to tag (default: HEAD): a tag, branch, commit id prefix, optionally with ~N or ^
        rev: Option<String>,
        #[arg(long)]
        zone: Option<String>,
        /// Make an annotated tag with this message
        #[arg(short, long)]
        message: Option<String>,
        /// Opaque signature text stored in an annotated tag (not verified)
        #[arg(long, requires = "message")]
        signature: Option<String>,
        /// Delete the named tag
        #[arg(short = 'd', long)]
        delete: bool,
    },
    /// Show a commit (default HEAD) with its diff against the first parent
    Show {
        rev: Option<String>,
        #[arg(long)]
        zone: Option<String>,
    },
    /// Attribute each line of a file to a commit, following first parents
    Blame {
        path: String,
        /// Revision to blame at (default: HEAD of the file's zone)
        #[arg(long)]
        rev: Option<String>,
    },
    /// Switch branch (in the zone containing cwd, or --zone)
    Switch {
        branch: String,
        #[arg(long)]
        zone: Option<String>,
    },
    /// Merge a branch into the checked-out branch (fast-forward when possible)
    Merge {
        /// Branch to merge (not needed with --abort)
        branch: Option<String>,
        /// Zone (default: the zone containing the current directory)
        #[arg(long)]
        zone: Option<String>,
        /// Merge the remote-tracking branch (as last fetched) instead of a local one
        #[arg(long)]
        remote: bool,
        /// Abandon the in-progress merge and restore the pre-merge state
        #[arg(long)]
        abort: bool,
        /// Do not use the structural Rust merge (only matters when built with `--features structural`)
        #[arg(long)]
        no_structural: bool,
        /// Detect renamed/moved files and merge the other side's edits into them (off by default)
        #[arg(long)]
        renames: bool,
        /// After a conflicted merge, ask this command (run via the shell) for a resolution of each
        /// text conflict; answers are saved as proposals, never applied (see `vcs conflict proposals`)
        #[arg(long, value_name = "COMMAND")]
        assist: Option<String>,
        /// Seconds the assistant gets per conflict
        #[arg(long, default_value_t = 30)]
        assist_timeout: u64,
        /// Do not run the assistant even if the `merge.assist` setting is configured
        #[arg(long, conflicts_with = "assist")]
        no_assist: bool,
    },
    /// Re-apply the change of one commit onto the checked-out branch
    #[command(name = "cherry-pick")]
    CherryPick {
        /// Commit id (or unique prefix), branch name or HEAD
        rev: String,
        /// Zone (default: the zone containing the current directory)
        #[arg(long)]
        zone: Option<String>,
    },
    /// Create a commit that undoes one commit
    Revert {
        /// Commit id (or unique prefix), branch name or HEAD
        rev: String,
        /// Zone (default: the zone containing the current directory)
        #[arg(long)]
        zone: Option<String>,
    },
    /// Replay the checked-out branch onto another branch, keeping each change id
    Rebase {
        /// Branch to rebase onto (not needed with --continue/--abort)
        onto: Option<String>,
        /// Zone (default: the zone containing the current directory)
        #[arg(long)]
        zone: Option<String>,
        /// Commit the resolved conflicts and carry on
        #[arg(long = "continue")]
        cont: bool,
        /// Abandon the in-progress rebase and restore the original branch
        #[arg(long)]
        abort: bool,
        /// Do not use the structural Rust merge (only matters when built with `--features structural`)
        #[arg(long)]
        no_structural: bool,
        /// Detect renamed/moved files and merge the other side's edits into them (off by default)
        #[arg(long)]
        renames: bool,
        /// After a conflicted step, ask this command for a resolution of each text conflict
        /// (saved as proposals, never applied; see `vcs conflict proposals`)
        #[arg(long, value_name = "COMMAND")]
        assist: Option<String>,
        /// Seconds the assistant gets per conflict
        #[arg(long, default_value_t = 30)]
        assist_timeout: u64,
        /// Do not run the assistant even if the `merge.assist` setting is configured
        #[arg(long, conflicts_with = "assist")]
        no_assist: bool,
    },
    /// Inspect conflicts of an in-progress merge
    #[command(subcommand)]
    Conflict(ConflictCmd),
    /// Resolve a conflicted path of a merge or rebase (ours = recorded left side, theirs = right)
    #[command(group(clap::ArgGroup::new("how").required(true).args(["ours", "theirs", "base", "file", "mark", "check", "proposal"])))]
    Resolve {
        /// Conflicted path (not used with --check)
        path: Option<String>,
        /// Take our side (merge: current branch; rebase: the branch being rebased onto)
        #[arg(long)]
        ours: bool,
        /// Take their side
        #[arg(long)]
        theirs: bool,
        /// Take the common ancestor's version
        #[arg(long)]
        base: bool,
        /// Take the content of this file
        #[arg(long, value_name = "FILE")]
        file: Option<std::path::PathBuf>,
        /// Accept the hand-edited working-tree file
        #[arg(long)]
        mark: bool,
        /// Apply the assistant's saved proposal for this path (see `vcs merge --assist`)
        #[arg(long)]
        proposal: bool,
        /// Fail while conflicts are unresolved or conflict markers remain in resolved files
        #[arg(long)]
        check: bool,
    },
    /// Manage zones
    #[command(subcommand)]
    Zone(ZoneCmd),
    /// Manage snapshots (pinned sets of zone versions)
    #[command(subcommand)]
    Snapshot(SnapCmd),
    /// Manage the remote
    #[command(subcommand)]
    Remote(RemoteCmd),
    /// Save a server token for an http:// remote (via the credential helper)
    Login {
        /// Remote URL (http://host:port)
        remote_url: String,
        /// Read the token from stdin instead of prompting
        #[arg(long)]
        token_stdin: bool,
    },
    /// Forget the saved token for a remote (default: the current remote)
    Logout { remote_url: Option<String> },
    /// Show authentication state
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Credential helper protocol: key=value lines on stdin/stdout
    #[command(subcommand)]
    Credential(CredCmd),
    /// Clone a remote into a new directory (sparse with --zone)
    Clone {
        url: String,
        dir: PathBuf,
        /// Only download these zones (repeatable)
        #[arg(long = "zone")]
        zones: Vec<String>,
        /// Print the number of remote requests and bytes transferred
        #[arg(long)]
        stats: bool,
    },
    /// Publish checked-out branches to the remote in one atomic transaction
    Publish {
        #[arg(long = "zone")]
        zones: Vec<String>,
        /// First re-upload any object the remote lost beneath objects it still has (checks every reachable object)
        #[arg(long)]
        repair: bool,
    },
    /// Lock files on the remote so others cannot publish changes to them
    Lock {
        paths: Vec<String>,
        /// Lease length in seconds (no default: choose per your team's workflow)
        #[arg(long = "lease-secs")]
        lease_secs: u64,
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// Release locks you hold (`--break` removes anyone's lock; unrestricted for now)
    Unlock {
        paths: Vec<String>,
        #[arg(long = "break")]
        admin_break: bool,
    },
    /// Show locks recorded on the remote (as of now; advisory until a validating writer exists)
    Locks,
    /// Download remote objects and update remote-tracking refs (local branches untouched)
    Fetch {
        #[arg(long = "zone")]
        zones: Vec<String>,
        /// Print the number of remote requests and bytes transferred
        #[arg(long)]
        stats: bool,
    },
    /// Fetch, then fast-forward local branches where safe
    Sync,
    /// Verify all objects and graph invariants
    Fsck,
    /// Move loose objects into a pack (one pack + index under .vcs/objects/pack)
    Repack,
    /// Report unreachable objects that garbage collection would delete (deletes nothing)
    Gc {
        /// Required: destructive GC is not implemented, see docs/gc.md
        #[arg(long)]
        dry_run: bool,
        /// Keep unreachable objects younger than this many seconds (default 14 days)
        #[arg(long, default_value_t = 14 * 24 * 3600)]
        grace_secs: u64,
    },
    /// Operation log: inspect, undo, restore (local branches and workspace only)
    #[command(subcommand)]
    Op(OpCmd),
}

#[derive(Subcommand, Clone)]
enum OpCmd {
    /// List operations, newest first
    Log {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Show one operation and its view
    Show { id: String },
    /// Restore the view before the current operation (recorded as a new operation)
    Undo,
    /// Restore the view as it was after the given operation (recorded as a new operation)
    Restore { id: String },
}

#[derive(Subcommand, Clone)]
enum ConflictCmd {
    /// List conflicted paths and whether they are resolved
    List,
    /// Show the assistant's saved proposals as diffs from our side
    Proposals,
    /// Show one conflict (ids and a diff3 rendering for text)
    Show { path: String },
}

#[derive(Subcommand, Clone)]
enum ZoneCmd {
    /// Create a zone owning a path prefix (zones may not overlap)
    Create {
        name: String,
        #[arg(long)]
        path: String,
    },
    List,
    Show {
        name: String,
    },
    Branches {
        name: String,
    },
    /// Create a branch in a zone
    Branch {
        zone: String,
        branch: String,
    },
    /// Check out a branch in a zone
    Checkout {
        zone: String,
        branch: String,
    },
    /// Subscribe to (and materialize) a zone
    Add {
        name: String,
        branch: Option<String>,
    },
    /// Unsubscribe from a zone (removes its files from the workspace)
    Remove {
        name: String,
    },
    /// Rename a zone (also with history: commits keep the original name as the zone's id)
    Rename {
        old: String,
        new: String,
    },
    /// Delete a zone definition (refuses a dirty zone; --force drops its branches and snapshot pins)
    Delete {
        name: String,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Clone)]
enum ConfigCmd {
    /// Print the effective value of a key (exit 1 if unset)
    Get { key: String },
    /// Set a key in the repository config (or the user config with --user)
    Set {
        key: String,
        value: String,
        #[arg(long)]
        user: bool,
    },
    /// Remove a key from the repository config (or the user config with --user)
    Unset {
        key: String,
        #[arg(long)]
        user: bool,
    },
    /// List effective settings and where each comes from
    List,
}

#[derive(Subcommand, Clone)]
enum AuthCmd {
    /// Show whether a token is available for the remote and who the server says you are
    Status { remote_url: Option<String> },
}

#[derive(Subcommand, Clone)]
enum CredCmd {
    /// Print host= and token= for the host read from stdin (nothing if unknown)
    Get,
    /// Save the host= and token= read from stdin
    Store,
    /// Forget the credential for the host read from stdin
    Erase,
}

fn auth_url(arg: Option<String>) -> Result<String, Error> {
    match arg {
        Some(u) => Ok(u),
        None => {
            let (repo, _) = open()?;
            need_remote(&repo)
        }
    }
}

#[derive(Subcommand, Clone)]
enum RemoteCmd {
    /// Set the remote location (path or file:// URL)
    Set { url: String },
    /// Show the configured remote
    Show,
}

#[derive(Subcommand, Clone)]
enum SnapCmd {
    Create {
        name: String,
        #[arg(short, long, default_value = "")]
        message: String,
    },
    Show {
        name: String,
    },
    List,
    /// Restore every subscribed zone to its pinned commit (on branch snapshot/<name>); refuses dirty zones
    Checkout {
        name: String,
    },
}

#[derive(Subcommand, Clone)]
enum StashCmd {
    /// Save staged and unstaged changes of tracked files and reset to the branch heads
    Push {
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Apply a stash entry (staged) and drop it; on conflicts it is kept
    Pop {
        /// Entry number from `stash list` (default: newest)
        index: Option<usize>,
    },
    /// List stash entries, newest first
    List,
    /// Forget a stash entry
    Drop { index: Option<usize> },
}

fn timestamp() -> u64 {
    std::env::var("VCS_TIMESTAMP")
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
}

/// Author from `user.name` / `user.email` (see `vcs config`); a helpful error when unset.
fn author() -> Result<Signature, Error> {
    let (name, email) = config::Config::load()?.identity()?;
    Ok(Signature {
        name,
        email,
        timestamp: timestamp(),
    })
}

/// Identity used for lock decisions: `VCS_PRINCIPAL`, else the author email.
fn principal() -> String {
    std::env::var("VCS_PRINCIPAL")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| config::Config::load().ok().and_then(|c| c.get("user.email")))
        .unwrap_or_else(|| "unknown@localhost".into())
}

fn open() -> Result<(Repo, PathBuf), Error> {
    let cwd = std::env::current_dir().map_err(|source| Error::Io { path: ".".into(), source })?;
    Ok((Repo::discover(&cwd)?, cwd))
}

fn zone_of_cwd(repo: &Repo, cwd: &Path, explicit: Option<String>) -> Result<String, Error> {
    if let Some(a) = explicit {
        return Ok(a);
    }
    let rel = repo.rel_path(cwd, ".")?;
    Ok(repo.zones()?.owner(if rel.is_empty() { "" } else { &rel }).to_owned())
}

fn print_pick(zone: &str, out: MergeOutcome, verb: &str) -> Result<(), Error> {
    match out {
        MergeOutcome::Merged { commit } => println!("{zone}: {verb} done, new commit {}", commit.short()),
        MergeOutcome::Conflicted { paths } => {
            println!("{verb} stopped: {} conflict(s)", paths.len());
            for p in &paths {
                println!("  conflict: {p}");
            }
            println!("\nResolve the files, `vcs add` each one, then `vcs commit -m <message>`.");
            println!("Inspect with `vcs conflict list|show <path>`; give up with `vcs merge --abort`.");
            return Err(Error::Msg(format!("{verb} has conflicts")));
        }
        MergeOutcome::UpToDate | MergeOutcome::FastForward { .. } => println!("nothing to do"),
    }
    Ok(())
}

fn kind_label(k: ChangeKind) -> &'static str {
    match k {
        ChangeKind::Added => "new file:",
        ChangeKind::Modified => "modified:",
        ChangeKind::Deleted => "deleted:",
    }
}

fn opt_short(i: Option<ObjectId>) -> String {
    i.map(|i| i.short()).unwrap_or_else(|| "(none)".into())
}

fn list_or_none(v: &[String]) -> String {
    if v.is_empty() {
        "(none)".into()
    } else {
        v.join(", ")
    }
}

fn need_remote(repo: &Repo) -> Result<String, Error> {
    repo.remote_url()?
        .or_else(|| config::Config::load().ok().and_then(|c| c.get("remote.default")))
        .ok_or_else(|| Error::Msg("no remote configured\n\nOptions:\n  vcs remote set <url>".into()))
}

fn run(cli: Cli) -> Result<(), Error> {
    if cli.json {
        let doc = json::build(&cli.cmd).ok_or_else(|| Error::Msg("--json is not supported for this command".into()))??;
        println!("{}", doc.render());
        return Ok(());
    }
    match cli.cmd {
        Cmd::Init { path } => {
            let p = path.unwrap_or_else(|| PathBuf::from("."));
            let r = Repo::init(&p)?;
            println!("Initialized empty vcs repository in {}", r.worktree().join(".vcs").display());
        }
        Cmd::Add { paths } => {
            let (repo, cwd) = open()?;
            if paths.is_empty() {
                return Err(Error::Msg("nothing specified; use `vcs add .` to stage everything".into()));
            }
            let rel: Vec<String> = paths.iter().map(|p| repo.rel_path(&cwd, p)).collect::<Result<_, _>>()?;
            let rep = repo.add(&rel)?;
            for s in &rep.skipped {
                eprintln!("warning: skipped {s}");
            }
            println!("staged {} file(s), {} removal(s)", rep.staged, rep.removed);
        }
        Cmd::Restore { paths, staged } => {
            let (repo, cwd) = open()?;
            let rel: Vec<String> = paths.iter().map(|p| repo.rel_path(&cwd, p)).collect::<Result<_, _>>()?;
            let rep = repo.restore(&rel, staged)?;
            println!("restored {} {}", rep.restored, if staged { "index entr(ies)" } else { "file(s)" });
        }
        Cmd::Rm { paths, cached, force } => {
            let (repo, cwd) = open()?;
            let rel: Vec<String> = paths.iter().map(|p| repo.rel_path(&cwd, p)).collect::<Result<_, _>>()?;
            let rep = repo.rm(&rel, cached, force)?;
            for p in &rep.removed {
                println!("rm {p}");
            }
        }
        Cmd::Mv { src, dst, force } => {
            let (repo, cwd) = open()?;
            let rep = repo.mv(&repo.rel_path(&cwd, &src)?, &repo.rel_path(&cwd, &dst)?, force)?;
            for (a, b) in &rep.moved {
                println!("{a} -> {b}");
            }
        }
        Cmd::Config(c) => {
            use config::{Config, Scope};
            let sc = |user: bool| if user { Scope::User } else { Scope::Repo };
            match c {
                ConfigCmd::Get { key } => {
                    config::validate_key(&key)?;
                    match Config::load()?.get(&key) {
                        Some(v) => println!("{v}"),
                        None => return Err(Error::Msg(format!("{key} is not set"))),
                    }
                }
                ConfigCmd::Set { key, value, user } => config::set(sc(user), &key, &value)?,
                ConfigCmd::Unset { key, user } => {
                    if !config::unset(sc(user), &key)? {
                        return Err(Error::Msg(format!(
                            "{key} is not set in the {} config",
                            if user { "user" } else { "repository" }
                        )));
                    }
                }
                ConfigCmd::List => {
                    for (k, v, src) in config::list(&Config::load()?) {
                        println!("{k}={v}  ({})", src.label());
                    }
                }
            }
        }
        Cmd::Commit { message } => {
            let (repo, _) = open()?;
            let cfg = config::Config::load()?;
            cfg.identity()?;
            let message = match message {
                Some(m) => m,
                None => config::edit_message(&cfg, &repo.worktree().join(".vcs"))?,
            };
            let rep = repo.commit(&message, author()?)?;
            let defs = repo.zones()?;
            for (zone, branch, id) in &rep.commits {
                let zone = defs.label(zone);
                println!("[{zone}/{branch} {}] {}", id.short(), message.lines().next().unwrap_or(""));
            }
            if rep.commits.len() > 1 {
                println!(
                    "atomic transaction {} covering {} zones",
                    rep.transaction.short(),
                    rep.commits.len()
                );
            }
        }
        Cmd::Status => {
            let (repo, _) = open()?;
            let st = repo.status()?;
            let defs = repo.zones()?;
            for a in &st.zones {
                let loc = if a.path.is_empty() {
                    String::new()
                } else {
                    format!(" ({}/)", a.path)
                };
                println!("zone {}{loc}: branch {} @ {}", defs.label(&a.zone), a.branch, opt_short(a.head));
                if !a.staged.is_empty() {
                    println!("  staged:");
                    for c in &a.staged {
                        println!("    {:<10} {}", kind_label(c.kind), c.path);
                    }
                }
                if !a.unstaged.is_empty() {
                    println!("  not staged:");
                    for c in &a.unstaged {
                        println!("    {:<10} {}", kind_label(c.kind), c.path);
                    }
                }
            }
            if let Some(m) = &st.merging {
                println!(
                    "merging {} into {}/{}: {} unresolved conflict(s)",
                    m.theirs_label,
                    m.zone,
                    m.branch,
                    m.unresolved().len()
                );
                for c in &m.conflicts {
                    println!(
                        "    {:<10} {} ({})",
                        if c.resolved { "resolved" } else { "unresolved" },
                        c.path,
                        vcs_repo::merge::kind_name(c.kind)
                    );
                }
                if repo.rebase_state()?.is_some() {
                    println!("  (rebase in progress: fix, `vcs add`, `vcs rebase --continue`; or `vcs rebase --abort`)");
                } else {
                    println!("  (fix, `vcs add`, `vcs commit`; or `vcs merge --abort`)");
                }
            }
            if !st.untracked.is_empty() {
                println!("untracked:");
                for u in &st.untracked {
                    println!("    {u}");
                }
            }
            if !st.unsubscribed.is_empty() {
                println!(
                    "not subscribed (not downloaded/materialized, NOT deleted): {}",
                    st.unsubscribed.join(", ")
                );
            }
            for u in &st.unsupported {
                println!("unsupported (special file): {u}");
            }
            if st.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty()) && st.untracked.is_empty() {
                println!("clean");
            }
        }
        Cmd::Diff { staged, stat } => {
            let (repo, _) = open()?;
            let d = repo.diff(staged)?;
            if stat {
                let stats = json::diff_stat(&d);
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
        }
        Cmd::ImportGit { stream } => {
            let (repo, _) = open()?;
            let r = if stream.as_os_str() == "-" {
                repo.import_git(std::io::stdin().lock())?
            } else {
                let f = std::fs::File::open(&stream).map_err(|source| Error::Io {
                    path: stream.clone(),
                    source,
                })?;
                repo.import_git(std::io::BufReader::new(f))?
            };
            println!(
                "imported {} commit(s), {} already present; {} blob(s); {} new object(s)",
                r.commits_imported, r.commits_skipped, r.blobs, r.objects_written
            );
            for (b, id) in &r.branches_updated {
                println!("  branch {b} -> {}", id.short());
            }
            for x in &r.refs_ignored {
                println!("ignored ref: {x}");
            }
            for x in &r.paths_skipped {
                println!("skipped path: {x}");
            }
            for x in &r.submodules {
                println!("skipped submodule: {x}");
            }
            if r.symlinks > 0 || r.tags_imported > 0 {
                println!("{} symlink entr(ies), {} tag(s) imported", r.symlinks, r.tags_imported);
            }
            for x in &r.tags_skipped {
                println!("skipped tag: {x}");
            }
        }
        Cmd::ImportP4 { dir, view, branch } => {
            let (repo, _) = open()?;
            let text = std::fs::read_to_string(&view).map_err(|source| Error::Io {
                path: view.clone(),
                source,
            })?;
            let v = vcs_repo::P4View::parse(&text)?;
            let r = repo.import_p4(&dir, &v, &branch)?;
            println!(
                "imported {} changelist(s), {} already present; {} new object(s)",
                r.changes_imported, r.changes_skipped, r.objects_written
            );
            for n in &r.changes_not_imported {
                println!("not imported (no file in view): change {n}");
            }
            for x in &r.paths_skipped {
                println!("skipped path: {x}");
            }
            for x in &r.type_warnings {
                println!("type warning: {x}");
            }
        }
        Cmd::ExportGit {
            zone,
            branch,
            output,
            full,
            onto_origin,
        } => {
            let (repo, cwd) = open()?;
            let zone = zone_of_cwd(&repo, &cwd, zone)?;
            let branch = branch.unwrap_or_else(|| "main".to_string());
            let opts = vcs_repo::ExportOptions {
                incremental: !full,
                onto_origin,
                record: true,
            };
            let rep = match output {
                Some(p) => {
                    let mut w =
                        std::io::BufWriter::new(std::fs::File::create(&p).map_err(|e| Error::Msg(format!("{}: {e}", p.display())))?);
                    let r = repo.export_git_with(&zone, &branch, &mut w, opts)?;
                    std::io::Write::flush(&mut w).map_err(|e| Error::Msg(e.to_string()))?;
                    r
                }
                None => repo.export_git_with(&zone, &branch, &mut std::io::BufWriter::new(std::io::stdout().lock()), opts)?,
            };
            eprintln!(
                "exported {} commit(s) ({} already exported), {} blob(s), {} tag(s) to {}",
                rep.commits, rep.commits_known, rep.blobs, rep.tags, rep.git_ref
            );
            for x in &rep.tags_skipped {
                eprintln!("skipped tag: {x}");
            }
        }
        Cmd::Log {
            zone,
            limit,
            oneline,
            graph,
            author,
            color,
            branch,
            paths,
        } => {
            let (repo, cwd) = open()?;
            let zone = zone_of_cwd(&repo, &cwd, zone)?;
            let oneline = oneline || config::Config::load()?.oneline_log()?;
            let opts = logfmt::Opts {
                oneline,
                graph,
                author,
                color: color.enabled(),
                paths,
            };
            print!("{}", logfmt::run(&repo, &cwd, &zone, branch.as_deref(), limit, &opts)?);
        }
        Cmd::Tag {
            name,
            rev,
            zone,
            message,
            signature,
            delete,
        } => {
            let (repo, cwd) = open()?;
            let zone = zone_of_cwd(&repo, &cwd, zone)?;
            match name {
                None if delete => return Err(Error::Msg("--delete needs a tag name".into())),
                None => {
                    let tags = repo.tag_list(&zone)?;
                    if tags.is_empty() {
                        println!("no tags in zone {}", repo.zones()?.label(&repo.resolve_zone(&zone)?));
                    }
                    for t in tags {
                        match t.annotation {
                            Some(a) => println!(
                                "{:<24} {}  annotated: {}",
                                t.name,
                                t.commit.short(),
                                a.message.lines().next().unwrap_or("")
                            ),
                            None => println!("{:<24} {}", t.name, t.commit.short()),
                        }
                    }
                }
                Some(n) if delete => {
                    let c = repo.tag_delete(&zone, &n)?;
                    println!("deleted tag {n} (was {})", c.short());
                }
                Some(n) => {
                    let ann = match message.as_deref() {
                        Some(m) => Some((m, author()?, signature.unwrap_or_default().into_bytes())),
                        None => None,
                    };
                    let c = repo.tag_create(&zone, &n, rev.as_deref(), ann)?;
                    println!("tagged {n} at {}", c.short());
                }
            }
        }
        Cmd::Show { rev, zone } => {
            let (repo, cwd) = open()?;
            let zone = zone_of_cwd(&repo, &cwd, zone)?;
            let rep = repo.show(&zone, rev.as_deref().unwrap_or("HEAD"))?;
            let defs = repo.zones()?;
            if let Some(t) = &rep.tag {
                println!("tag {}", t.name);
                println!("Tagger: {} <{}>", t.tagger.name, t.tagger.email);
                println!("Date:   {}", t.tagger.timestamp);
                if !t.signature.is_empty() {
                    println!("Signature: {} bytes (not verified)", t.signature.len());
                }
                for l in t.message.lines() {
                    println!("    {l}");
                }
                println!();
            }
            let c = &rep.commit;
            println!("commit {}", rep.id);
            println!("change {}   zone {}", c.change_id, defs.label(&c.zone));
            for p in &c.parents {
                println!("parent {p}");
            }
            println!("Author: {} <{}>", c.author.name, c.author.email);
            println!("Date:   {}", c.author.timestamp);
            for l in c.message.lines() {
                println!("    {l}");
            }
            println!();
            print!("{}", rep.diff);
        }
        Cmd::Blame { path, rev } => {
            let (repo, cwd) = open()?;
            let rel = repo.rel_path(&cwd, &path)?;
            let lines = repo.blame(&rel, rev.as_deref())?;
            let w = lines.len().to_string().len();
            for l in lines {
                println!("{} ({:>w$}) {}", l.commit.short(), l.line_no, l.text);
            }
        }
        Cmd::Gen(g) => generate::run(g, <Cli as clap::CommandFactory>::command())?,
        Cmd::Branch { name, zone, delete, force } => {
            let (repo, cwd) = open()?;
            let zone = zone_of_cwd(&repo, &cwd, zone)?;
            match name {
                Some(n) if delete => {
                    let h = repo.branch_delete(&zone, &n, force)?;
                    println!("deleted {zone}/{n} (was {})", h.short());
                }
                None if delete => return Err(Error::Msg("--delete needs a branch name".into())),
                Some(n) => {
                    let h = repo.create_branch(&zone, &n)?;
                    println!("created {zone}/{n} at {}", h.short());
                }
                None => print_branches(&repo, &zone)?,
            }
        }
        Cmd::Switch { branch, zone } => {
            let (repo, cwd) = open()?;
            let zone = zone_of_cwd(&repo, &cwd, zone)?;
            repo.checkout(&zone, &branch)?;
            println!("zone {zone}: switched to {branch}");
        }
        Cmd::Merge {
            branch,
            zone,
            remote,
            abort,
            no_structural,
            renames,
            assist,
            assist_timeout,
            no_assist,
        } => {
            let (mut repo, cwd) = open()?;
            repo.set_structural(!no_structural && config::Config::load()?.structural()?);
            repo.set_renames(renames || config::Config::load()?.renames()?);
            if abort {
                repo.merge_abort()?;
                println!("merge aborted; working tree restored");
                return Ok(());
            }
            let Some(branch) = branch else {
                return Err(Error::Msg(
                    "usage: vcs merge <branch> [--zone A] [--remote] | vcs merge --abort".into(),
                ));
            };
            let zone = zone_of_cwd(&repo, &cwd, zone)?;
            let out = if remote {
                repo.merge_remote(&zone, &branch, author()?)?
            } else {
                repo.merge(&zone, &branch, author()?)?
            };
            match out {
                MergeOutcome::UpToDate => println!("already up to date"),
                MergeOutcome::FastForward { from, to } => {
                    println!("{zone}: fast-forward {} -> {}", opt_short(from), to.short())
                }
                MergeOutcome::Merged { commit } => println!("{zone}: merged, new commit {}", commit.short()),
                MergeOutcome::Conflicted { paths } => {
                    println!("automatic merge stopped: {} conflict(s)", paths.len());
                    for p in &paths {
                        println!("  conflict: {p}");
                    }
                    if let Some(cmd) = effective_assist(assist.as_deref(), no_assist)? {
                        run_assist(&repo, &cmd, assist_timeout)?;
                    }
                    println!("\nResolve the files, `vcs add` each one, then `vcs commit`.");
                    println!("Inspect with `vcs conflict list|show <path>`; give up with `vcs merge --abort`.");
                    return Err(Error::Msg("merge has conflicts".into()));
                }
            }
        }
        Cmd::CherryPick { rev, zone } => {
            let (repo, cwd) = open()?;
            let zone = zone_of_cwd(&repo, &cwd, zone)?;
            let out = repo.cherry_pick(&zone, &rev, author()?)?;
            print_pick(&zone, out, "cherry-pick")?;
        }
        Cmd::Revert { rev, zone } => {
            let (repo, cwd) = open()?;
            let zone = zone_of_cwd(&repo, &cwd, zone)?;
            let out = repo.revert(&zone, &rev, author()?)?;
            print_pick(&zone, out, "revert")?;
        }
        Cmd::Rebase {
            onto,
            zone,
            cont,
            abort,
            no_structural,
            renames,
            assist,
            assist_timeout,
            no_assist,
        } => {
            let (mut repo, cwd) = open()?;
            repo.set_structural(!no_structural && config::Config::load()?.structural()?);
            repo.set_renames(renames || config::Config::load()?.renames()?);
            if abort {
                repo.rebase_abort()?;
                println!("rebase aborted; original branch restored");
                return Ok(());
            }
            let out = if cont {
                repo.rebase_continue(author()?)?
            } else {
                let Some(onto) = onto else {
                    return Err(Error::Msg("usage: vcs rebase <branch> [--zone A] | --continue | --abort".into()));
                };
                let zone = zone_of_cwd(&repo, &cwd, zone)?;
                repo.rebase(&zone, &onto, author()?)?
            };
            match out {
                RebaseOutcome::UpToDate => println!("already up to date"),
                RebaseOutcome::Done { from, to, replayed } => {
                    println!("rebased {replayed} commit(s): {} -> {}", from.short(), to.short())
                }
                RebaseOutcome::Conflicted { commit, paths } => {
                    println!("rebase stopped at {}: {} conflict(s)", commit.short(), paths.len());
                    for p in &paths {
                        println!("  conflict: {p}");
                    }
                    if let Some(cmd) = effective_assist(assist.as_deref(), no_assist)? {
                        run_assist(&repo, &cmd, assist_timeout)?;
                    }
                    println!("\nResolve the files, `vcs add` each one, then `vcs rebase --continue`.");
                    println!("Inspect with `vcs conflict list|show <path>`; give up with `vcs rebase --abort`.");
                    return Err(Error::Msg("rebase has conflicts".into()));
                }
            }
        }
        Cmd::Resolve {
            path,
            ours,
            theirs,
            base,
            file,
            mark: _,
            proposal,
            check,
        } => {
            let (repo, cwd) = open()?;
            if check {
                if path.is_some() {
                    return Err(Error::Msg("--check takes no path".into()));
                }
                let r = repo.resolve_check()?;
                for p in &r.unresolved {
                    println!("unresolved: {p}");
                }
                for p in &r.with_markers {
                    println!("conflict markers remain: {p}");
                }
                if !r.is_clean() {
                    return Err(Error::Msg(format!(
                        "{} unresolved, {} with conflict markers",
                        r.unresolved.len(),
                        r.with_markers.len()
                    )));
                }
                println!("all conflicts resolved, no conflict markers left");
            } else {
                let path = path.ok_or_else(|| Error::Msg("resolve needs a <path>".into()))?;
                let rel = repo.rel_path(&cwd, &path)?;
                if proposal {
                    repo.accept_proposal(&rel)?;
                    println!("resolved {rel} from the assistant's proposal");
                    if let Some(k) = repo.merge_state()? {
                        println!("{} conflict(s) left", k.unresolved().len());
                    }
                    return Ok(());
                }
                let how = if ours {
                    Resolution::Ours
                } else if theirs {
                    Resolution::Theirs
                } else if base {
                    Resolution::Base
                } else if let Some(f) = file {
                    Resolution::File(cwd.join(f))
                } else {
                    Resolution::Mark
                };
                repo.resolve(&rel, &how)?;
                println!("resolved {rel}");
                if let Some(k) = repo.merge_state()? {
                    println!("{} conflict(s) left", k.unresolved().len());
                }
            }
        }
        Cmd::Stash(c) => {
            let (repo, _) = open()?;
            match c {
                StashCmd::Push { message } => {
                    let zs = repo.stash_push(message.as_deref(), author()?)?;
                    println!("stashed changes in zone(s): {}", zs.join(", "));
                }
                StashCmd::Pop { index } => match repo.stash_pop(index.unwrap_or(0))? {
                    vcs_repo::StashPopOutcome::Applied => println!("applied and dropped stash entry"),
                    vcs_repo::StashPopOutcome::Conflicted { paths } => {
                        println!("conflicts in {} file(s); the stash entry was kept:", paths.len());
                        for p in &paths {
                            println!("    {p}");
                        }
                        println!("fix the markers, `vcs add` the files, then `vcs stash drop`");
                    }
                },
                StashCmd::List => {
                    for (i, e) in repo.stash_list()?.iter().enumerate() {
                        println!("stash@{{{i}}}: {}", e.message);
                    }
                }
                StashCmd::Drop { index } => {
                    let e = repo.stash_drop(index.unwrap_or(0))?;
                    println!("dropped stash entry: {}", e.message);
                }
            }
        }
        Cmd::Conflict(c) => {
            let (repo, cwd) = open()?;
            match c {
                ConflictCmd::List => {
                    for c in repo.conflicts()? {
                        println!(
                            "{:<10} {:<14} {}",
                            if c.resolved { "resolved" } else { "unresolved" },
                            vcs_repo::merge::kind_name(c.kind),
                            c.path
                        );
                    }
                }
                ConflictCmd::Proposals => {
                    let props = repo.proposals()?;
                    if props.is_empty() {
                        println!("no proposals");
                    }
                    for p in props {
                        println!("proposal for {}", p.path);
                        if !p.explanation.is_empty() {
                            println!("  assistant says: {}", p.explanation.replace('\n', " "));
                        }
                        println!("  diff from our side:");
                        for l in p.diff.lines() {
                            println!("    {l}");
                        }
                        println!("  apply with: vcs resolve {} --proposal", p.path);
                    }
                }
                ConflictCmd::Show { path } => {
                    let rel = repo.rel_path(&cwd, &path)?;
                    print!("{}", repo.conflict_show(&rel)?);
                }
            }
        }
        Cmd::Zone(a) => {
            let (repo, _) = open()?;
            match a {
                ZoneCmd::Create { name, path } => {
                    repo.create_zone(&name, &path)?;
                    println!("created zone {name} owning {path}/");
                }
                ZoneCmd::List => {
                    let zones = repo.zones()?;
                    let ws = repo.workspace()?;
                    let refs = repo.refs()?;
                    for n in zones.names() {
                        let p = zones.path_of(n).unwrap();
                        let shown = zones.label(n);
                        let sub = match ws.subs.get(n) {
                            Some(b) => format!("subscribed, on {b}"),
                            None => "not subscribed".into(),
                        };
                        let nb = refs.branches_of(n).len();
                        println!(
                            "{shown:<14} {:<20} {sub}; {nb} branch(es)",
                            if p.is_empty() { "(rest of repo)" } else { p }
                        );
                    }
                }
                ZoneCmd::Show { name } => {
                    let zones = repo.zones()?;
                    let ws = repo.workspace()?;
                    let Some(id) = zones.resolve(&name) else {
                        return Err(Error::Msg(format!("no such zone {name:?}")));
                    };
                    let p = zones.path_of(&id).unwrap();
                    let name = id;
                    println!(
                        "zone:   {}{}\npath:   {}",
                        zones.label(&name),
                        if zones.label(&name) != name {
                            format!(" (id {name})")
                        } else {
                            String::new()
                        },
                        if p.is_empty() {
                            "(everything not owned by another zone)"
                        } else {
                            p
                        }
                    );
                    match ws.subs.get(&name) {
                        Some(b) => println!("state:  subscribed, branch {b}"),
                        None => println!("state:  not subscribed"),
                    }
                    print_branches(&repo, &name)?;
                }
                ZoneCmd::Branches { name } => print_branches(&repo, &name)?,
                ZoneCmd::Branch { zone, branch } => {
                    let h = repo.create_branch(&zone, &branch)?;
                    println!("created {zone}/{branch} at {}", h.short());
                }
                ZoneCmd::Checkout { zone, branch } => {
                    repo.checkout(&zone, &branch)?;
                    println!("zone {zone}: switched to {branch}");
                }
                ZoneCmd::Add { name, branch } => {
                    match repo.remote_url()? {
                        Some(url) => {
                            let backend = remote::open_backend(&url)?;
                            repo.zone_add_fetching(backend.as_ref(), &name, branch.as_deref())?;
                        }
                        None => repo.zone_add(&name, branch.as_deref())?,
                    }
                    println!("subscribed to zone {name}");
                }
                ZoneCmd::Remove { name } => {
                    repo.zone_remove(&name)?;
                    println!("unsubscribed from zone {name}");
                }
                ZoneCmd::Rename { old, new } => {
                    repo.zone_rename(&old, &new)?;
                    println!("renamed zone {old} to {new}");
                }
                ZoneCmd::Delete { name, force } => {
                    repo.zone_delete(&name, force)?;
                    println!("deleted zone {name}");
                }
            }
        }
        Cmd::Snapshot(s) => {
            let (repo, _) = open()?;
            match s {
                SnapCmd::Create { name, message } => {
                    let id = repo.snapshot_create(&name, &message, author()?)?;
                    println!("snapshot {name} = {}", id.short());
                }
                SnapCmd::Show { name } => {
                    let (id, s) = repo.snapshot_get(&name)?;
                    println!("snapshot {name} ({id})");
                    for (a, c) in &s.zones {
                        println!("  {a:<14} {c}");
                    }
                }
                SnapCmd::Checkout { name } => {
                    let rep = repo.snapshot_checkout(&name)?;
                    for (a, b, c) in &rep.switched {
                        println!("zone {a}: now on {b} at {}", c.short());
                    }
                    for a in &rep.unchanged {
                        println!("zone {a}: already at the snapshot");
                    }
                    for a in &rep.skipped {
                        println!("zone {a}: not subscribed, left alone");
                    }
                }
                SnapCmd::List => {
                    for (n, id) in repo.refs()?.snapshots {
                        println!("{n:<20} {}", id.short());
                    }
                }
            }
        }
        Cmd::Login { remote_url, token_stdin } => auth::login(&remote_url, token_stdin)?,
        Cmd::Logout { remote_url } => auth::logout(&auth_url(remote_url)?)?,
        Cmd::Auth(AuthCmd::Status { remote_url }) => auth::status(&auth_url(remote_url)?)?,
        Cmd::Credential(c) => auth::credential(match c {
            CredCmd::Get => auth::Op::Get,
            CredCmd::Store => auth::Op::Store,
            CredCmd::Erase => auth::Op::Erase,
        })?,
        Cmd::Remote(r) => {
            let (repo, cwd) = open()?;
            match r {
                RemoteCmd::Set { url } => {
                    let url = remote::normalize_url(&url, &cwd);
                    repo.set_remote(&url)?;
                    println!("remote set to {url}");
                }
                RemoteCmd::Show => match repo.remote_url()? {
                    Some(u) => {
                        println!("remote: {u}");
                        let rr = repo.remote_refs()?;
                        if rr.branches.is_empty() {
                            println!("no remote-tracking refs yet (run `vcs fetch`)");
                        }
                        for ((a, b), id) in &rr.branches {
                            println!("  {a}/{b} @ {}", id.short());
                        }
                    }
                    None => println!("no remote configured (use `vcs remote set <url>`)"),
                },
            }
        }
        Cmd::Clone { url, dir, zones, stats } => {
            let cwd = std::env::current_dir().map_err(|source| Error::Io { path: ".".into(), source })?;
            let url = remote::normalize_url(&url, &cwd);
            let backend = remote::open_backend(&url)?;
            let counting = vcs_remote::CountingBackend::new(backend.as_ref());
            let want = (!zones.is_empty()).then_some(zones.as_slice());
            let repo = Repo::clone_from(&counting, &url, &dir, want)?;
            if stats {
                println!("{}", counting.stats());
            }
            let defs = repo.zones()?;
            let ws = repo.workspace()?;
            let (got, skipped): (Vec<_>, Vec<_>) = defs.names().map(str::to_owned).partition(|n| ws.subs.contains_key(n));
            println!("Cloned {url} into {}", dir.display());
            println!("downloaded zones: {}", list_or_none(&got));
            println!("skipped zones (not subscribed): {}", list_or_none(&skipped));
            if !skipped.is_empty() {
                println!("  add one later with `vcs zone add <name>` then `vcs fetch --zone <name>`");
            }
        }
        Cmd::Publish { zones, repair } => {
            let (repo, _) = open()?;
            let backend = remote::open_backend(&need_remote(&repo)?)?;
            if repair {
                println!("repaired {} object(s) on the remote", repo.repair_remote(backend.as_ref())?);
            }
            let want = (!zones.is_empty()).then_some(zones.as_slice());
            let rep = repo.publish_as(backend.as_ref(), want, author()?, &principal())?;
            if rep.updated.is_empty() && rep.tags.is_empty() {
                println!("nothing to publish");
            }
            for (a, b, old, new) in &rep.updated {
                println!("{a}/{b} {} -> {}", opt_short(*old), new.short());
            }
            for (z, n) in &rep.tags {
                println!("tag {z}/{n}");
            }
            for (z, n) in &rep.tag_conflicts {
                eprintln!("warning: tag {z}/{n} exists on the remote at another target; not moved");
            }
            match rep.outcome {
                Some(Outcome::Applied { generation }) => println!("published atomically (remote generation {generation})"),
                Some(Outcome::AlreadyApplied) => println!("transaction was already applied on the remote"),
                None => {}
            }
        }
        Cmd::Lock { paths, lease_secs, reason } => {
            let (repo, cwd) = open()?;
            let backend = remote::open_backend(&need_remote(&repo)?)?;
            let rel: Vec<String> = paths.iter().map(|p| repo.rel_path(&cwd, p)).collect::<Result<_, _>>()?;
            let got = repo.lock_paths(
                backend.as_ref(),
                &rel,
                lease_secs.saturating_mul(1000),
                &reason,
                author()?,
                &principal(),
            )?;
            let defs = repo.zones()?;
            for (z, p, t) in got {
                println!("locked {p} (zone {}) token {t}", defs.label(&z));
            }
        }
        Cmd::Unlock { paths, admin_break } => {
            let (repo, cwd) = open()?;
            let backend = remote::open_backend(&need_remote(&repo)?)?;
            let rel: Vec<String> = paths.iter().map(|p| repo.rel_path(&cwd, p)).collect::<Result<_, _>>()?;
            repo.unlock_paths(backend.as_ref(), &rel, admin_break, author()?, &principal())?;
            println!("unlocked {} path(s)", rel.len());
        }
        Cmd::Locks => {
            let (repo, _) = open()?;
            let backend = remote::open_backend(&need_remote(&repo)?)?;
            let rows = repo.list_locks(backend.as_ref())?;
            if rows.is_empty() {
                println!("no locks");
            }
            for (z, p, l) in rows {
                let now = std::time::SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let state = if l.expires_at > now { "held" } else { "expired" };
                println!(
                    "{p} (zone {z}) {state} by {} until {} (unix ms) token {}  {}",
                    l.holder, l.expires_at, l.token, l.reason
                );
            }
        }
        Cmd::Fetch { zones, stats } => {
            let (repo, _) = open()?;
            let backend = remote::open_backend(&need_remote(&repo)?)?;
            let counting = vcs_remote::CountingBackend::new(backend.as_ref());
            let want = (!zones.is_empty()).then_some(zones.as_slice());
            let got = repo.fetch(&counting, want)?;
            if stats {
                println!("{}", counting.stats());
            }
            if got.is_empty() {
                println!("nothing fetched");
            }
            for (a, b, id) in got {
                println!("fetched {a}/{b} @ {}", id.short());
            }
        }
        Cmd::Sync => {
            let (repo, _) = open()?;
            let backend = remote::open_backend(&need_remote(&repo)?)?;
            let rep = repo.sync(backend.as_ref())?;
            for (a, b, act) in &rep.actions {
                let what = match act {
                    SyncAction::UpToDate => "up to date".to_owned(),
                    SyncAction::FastForwarded { from, to } => {
                        format!("fast-forwarded {} -> {}", opt_short(*from), to.short())
                    }
                    SyncAction::Ahead => "ahead of remote (run `vcs publish`)".into(),
                    SyncAction::Diverged => {
                        let r = if a == ROOT_ZONE { String::new() } else { format!(" --zone {a}") };
                        format!("diverged; merge with `vcs merge --remote {b}{r}`")
                    }
                    SyncAction::Dirty => "dirty (left untouched; commit or discard changes first)".into(),
                    SyncAction::NoRemote => "not on the remote yet (run `vcs publish`)".into(),
                };
                println!("{a}/{b}: {what}");
            }
        }
        Cmd::Op(c) => {
            let (repo, _) = open()?;
            match c {
                OpCmd::Log { limit } => {
                    let head = repo.op_head()?;
                    for (id, op) in repo.op_log(limit)? {
                        let mark = if Some(id) == head { "@" } else { " " };
                        println!(
                            "{mark} {}  {}  {} <{}>",
                            id.short(),
                            op.author.timestamp,
                            op.author.name,
                            op.author.email
                        );
                        println!("    {}", op.description);
                    }
                }
                OpCmd::Show { id } => {
                    let id = repo.op_resolve(&id)?;
                    let op = repo.op_get(&id)?;
                    println!("operation {id}");
                    println!("description: {}", op.description);
                    println!("author:      {} <{}> @ {}", op.author.name, op.author.email, op.author.timestamp);
                    for p in &op.parents {
                        println!("parent:      {p}");
                    }
                    println!("workspace:");
                    for (a, b) in &op.view.workspace {
                        println!("  {a:<14} on {b}");
                    }
                    println!("branches:");
                    for ((a, b), c) in &op.view.branches {
                        println!("  {a}/{b:<24} {}", c.short());
                    }
                }
                OpCmd::Undo => {
                    let id = repo.op_undo()?;
                    println!("undone; recorded operation {}", id.short());
                }
                OpCmd::Restore { id } => {
                    let target = repo.op_resolve(&id)?;
                    let id = repo.op_restore(&target)?;
                    println!("restored view of {}; recorded operation {}", target.short(), id.short());
                }
            }
        }
        Cmd::Gc { dry_run, grace_secs } => {
            if !dry_run {
                return Err(Error::Msg(
                    "only `vcs gc --dry-run` is implemented; nothing is ever deleted (see docs/gc.md)".into(),
                ));
            }
            let (repo, _) = open()?;
            let r = repo.gc_plan(std::time::Duration::from_secs(grace_secs))?;
            println!("{} objects: {} reachable", r.total, r.reachable);
            println!(
                "{} unreachable but younger than the {grace_secs}s grace period (kept)",
                r.kept_young.len()
            );
            if r.missing_referenced > 0 {
                println!("{} referenced objects are missing (run `vcs fsck`)", r.missing_referenced);
            }
            println!("{} unreachable and old enough: would delete", r.would_delete.len());
            for (id, kind) in &r.would_delete {
                println!("  would delete {} {kind:?}", id.short());
            }
            println!("dry run: nothing was deleted");
        }
        Cmd::Repack => {
            let (repo, _) = open()?;
            let n = repo.repack()?;
            println!("packed {n} object(s)");
        }
        Cmd::Fsck => {
            let (repo, _) = open()?;
            let r = repo.fsck()?;
            println!("checked {} objects", r.objects);
            if !r.evictable.is_empty() {
                println!(
                    "{} object(s) not cached locally; they are on the remote and will be refetched on demand",
                    r.evictable.len()
                );
            }
            if r.problems.is_empty() {
                println!("ok");
            } else {
                for p in &r.problems {
                    println!("PROBLEM: {p}");
                }
                return Err(Error::Msg(format!("{} problem(s) found", r.problems.len())));
            }
        }
    }
    Ok(())
}

fn print_branches(repo: &Repo, zone: &str) -> Result<(), Error> {
    for (b, id, cur) in repo.branches(zone)? {
        println!("{} {zone}/{b:<24} {}", if cur { "*" } else { " " }, opt_short(id));
    }
    Ok(())
}

/// Run the command; if it fails because a cached object was evicted, fetch that one object
/// (hash-verified) from the remote and run it again. See docs/cache.md.
fn run_lazy(cli: Cli) -> Result<(), Error> {
    let mut fetched = std::collections::BTreeSet::new();
    loop {
        match run(cli.clone()) {
            Err(Error::Store(vcs_store::StoreError::NotFound(id))) if fetched.insert(id) => {
                let (repo, _) = open()?;
                let Some(url) = repo.remote_url()? else {
                    return Err(Error::Store(vcs_store::StoreError::NotFound(id)));
                };
                let backend = remote::open_backend(&url)?;
                repo.fetch_missing(backend.as_ref(), &id)?;
                eprintln!("fetched missing object {}", id.short());
            }
            r => return r,
        }
    }
}

fn main() -> ExitCode {
    let _ = ROOT_ZONE;
    let cli = Cli::parse();
    if let Some(dir) = &cli.chdir {
        if let Err(e) = std::env::set_current_dir(dir) {
            eprintln!("error: cannot change to directory {}: {e}", dir.display());
            return ExitCode::FAILURE;
        }
    }
    match run_lazy(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
