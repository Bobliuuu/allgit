use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};
use vcs_objects::{ObjectId, Signature};
use vcs_remote::Outcome;
use vcs_repo::{ChangeKind, Error, Repo, SyncAction, ROOT_ZONE};

mod remote;

#[derive(Parser)]
#[command(name = "vcs", version, about = "Native zone-aware version control")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new repository (default: current directory)
    Init { path: Option<PathBuf> },
    /// Stage files or directories (also stages deletions)
    Add { paths: Vec<String> },
    /// Commit staged changes in all subscribed zones atomically
    Commit {
        #[arg(short, long)]
        message: String,
    },
    /// Show staged, unstaged and untracked changes per zone
    Status,
    /// Show unstaged changes (or --staged)
    Diff {
        #[arg(long)]
        staged: bool,
    },
    /// Show commit history of a zone branch
    Log {
        /// Zone (default: the zone containing the current directory)
        #[arg(long)]
        zone: Option<String>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        /// Branch (default: the checked-out one)
        branch: Option<String>,
    },
    /// List branches of the current zone, or create one (at the checked-out head)
    Branch {
        name: Option<String>,
        #[arg(long)]
        zone: Option<String>,
    },
    /// Switch branch (in the zone containing cwd, or --zone)
    Switch {
        branch: String,
        #[arg(long)]
        zone: Option<String>,
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
    /// Clone a remote into a new directory (sparse with --zone)
    Clone {
        url: String,
        dir: PathBuf,
        /// Only download these zones (repeatable)
        #[arg(long = "zone")]
        zones: Vec<String>,
    },
    /// Publish checked-out branches to the remote in one atomic transaction
    Publish {
        #[arg(long = "zone")]
        zones: Vec<String>,
    },
    /// Download remote objects and update remote-tracking refs (local branches untouched)
    Fetch {
        #[arg(long = "zone")]
        zones: Vec<String>,
    },
    /// Fetch, then fast-forward local branches where safe
    Sync,
    /// Verify all objects and graph invariants
    Fsck,
    /// Operation log: inspect, undo, restore (local branches and workspace only)
    #[command(subcommand)]
    Op(OpCmd),
}

#[derive(Subcommand)]
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

#[derive(Subcommand)]
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
}

#[derive(Subcommand)]
enum RemoteCmd {
    /// Set the remote location (path or file:// URL)
    Set { url: String },
    /// Show the configured remote
    Show,
}

#[derive(Subcommand)]
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
}

fn author() -> Signature {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let name = env("VCS_AUTHOR_NAME").or_else(|| env("USER")).unwrap_or_else(|| "unknown".into());
    let email = env("VCS_AUTHOR_EMAIL").unwrap_or_else(|| format!("{name}@localhost"));
    let timestamp = env("VCS_TIMESTAMP")
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    Signature { name, email, timestamp }
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
        .ok_or_else(|| Error::Msg("no remote configured\n\nOptions:\n  vcs remote set <url>".into()))
}

fn run(cli: Cli) -> Result<(), Error> {
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
        Cmd::Commit { message } => {
            let (repo, _) = open()?;
            let rep = repo.commit(&message, author())?;
            for (zone, branch, id) in &rep.commits {
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
            for a in &st.zones {
                let loc = if a.path.is_empty() {
                    String::new()
                } else {
                    format!(" ({}/)", a.path)
                };
                println!("zone {}{loc}: branch {} @ {}", a.zone, a.branch, opt_short(a.head));
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
                println!("unsupported (symlink/special): {u}");
            }
            if st.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty()) && st.untracked.is_empty() {
                println!("clean");
            }
        }
        Cmd::Diff { staged } => {
            let (repo, _) = open()?;
            print!("{}", repo.diff(staged)?);
        }
        Cmd::Log { zone, limit, branch } => {
            let (repo, cwd) = open()?;
            let zone = zone_of_cwd(&repo, &cwd, zone)?;
            let entries = repo.log(&zone, branch.as_deref(), limit)?;
            if entries.is_empty() {
                println!("zone {zone}: no commits yet");
            }
            for (id, c) in entries {
                println!("commit {id}");
                println!("change {}   zone {}", c.change_id, c.zone);
                println!("Author: {} <{}>", c.author.name, c.author.email);
                println!("Date:   {}", c.author.timestamp);
                for l in c.message.lines() {
                    println!("    {l}");
                }
                println!();
            }
        }
        Cmd::Branch { name, zone } => {
            let (repo, cwd) = open()?;
            let zone = zone_of_cwd(&repo, &cwd, zone)?;
            match name {
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
                        let sub = match ws.subs.get(n) {
                            Some(b) => format!("subscribed, on {b}"),
                            None => "not subscribed".into(),
                        };
                        let nb = refs.branches_of(n).len();
                        println!(
                            "{n:<14} {:<20} {sub}; {nb} branch(es)",
                            if p.is_empty() { "(rest of repo)" } else { p }
                        );
                    }
                }
                ZoneCmd::Show { name } => {
                    let zones = repo.zones()?;
                    let ws = repo.workspace()?;
                    let Some(p) = zones.path_of(&name) else {
                        return Err(Error::Msg(format!("no such zone {name:?}")));
                    };
                    println!(
                        "zone:   {name}\npath:   {}",
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
                    repo.zone_add(&name, branch.as_deref())?;
                    println!("subscribed to zone {name}");
                }
                ZoneCmd::Remove { name } => {
                    repo.zone_remove(&name)?;
                    println!("unsubscribed from zone {name}");
                }
            }
        }
        Cmd::Snapshot(s) => {
            let (repo, _) = open()?;
            match s {
                SnapCmd::Create { name, message } => {
                    let id = repo.snapshot_create(&name, &message, author())?;
                    println!("snapshot {name} = {}", id.short());
                }
                SnapCmd::Show { name } => {
                    let (id, s) = repo.snapshot_get(&name)?;
                    println!("snapshot {name} ({id})");
                    for (a, c) in &s.zones {
                        println!("  {a:<14} {c}");
                    }
                }
                SnapCmd::List => {
                    for (n, id) in repo.refs()?.snapshots {
                        println!("{n:<20} {}", id.short());
                    }
                }
            }
        }
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
        Cmd::Clone { url, dir, zones } => {
            let cwd = std::env::current_dir().map_err(|source| Error::Io { path: ".".into(), source })?;
            let url = remote::normalize_url(&url, &cwd);
            let backend = remote::open_backend(&url)?;
            let want = (!zones.is_empty()).then_some(zones.as_slice());
            let repo = Repo::clone_from(backend.as_ref(), &url, &dir, want)?;
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
        Cmd::Publish { zones } => {
            let (repo, _) = open()?;
            let backend = remote::open_backend(&need_remote(&repo)?)?;
            let want = (!zones.is_empty()).then_some(zones.as_slice());
            let rep = repo.publish(backend.as_ref(), want, author())?;
            if rep.updated.is_empty() {
                println!("nothing to publish");
            }
            for (a, b, old, new) in &rep.updated {
                println!("{a}/{b} {} -> {}", opt_short(*old), new.short());
            }
            match rep.outcome {
                Some(Outcome::Applied { generation }) => println!("published atomically (remote generation {generation})"),
                Some(Outcome::AlreadyApplied) => println!("transaction was already applied on the remote"),
                None => {}
            }
        }
        Cmd::Fetch { zones } => {
            let (repo, _) = open()?;
            let backend = remote::open_backend(&need_remote(&repo)?)?;
            let want = (!zones.is_empty()).then_some(zones.as_slice());
            let got = repo.fetch(backend.as_ref(), want)?;
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
                    SyncAction::Diverged => "diverged (needs merge, not implemented)".into(),
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
        Cmd::Fsck => {
            let (repo, _) = open()?;
            let r = repo.fsck()?;
            println!("checked {} objects", r.objects);
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

fn main() -> ExitCode {
    let _ = ROOT_ZONE;
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
