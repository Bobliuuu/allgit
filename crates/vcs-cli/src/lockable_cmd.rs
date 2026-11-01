//! CLI glue for lockable files (`.vcsattributes`, docs/locks.md): the add/commit guard, the
//! read-only refresh after commands that write the worktree, lock bookkeeping and `vcs locks`.

use crate::{config, remote};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use vcs_repo::{ChangeKind, Error, Repo};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn msg<T>(s: impl Into<String>) -> Result<T, Error> {
    Err(Error::Msg(s.into()))
}

/// Which directory to refresh after this command ran (commands that write worktree files).
pub fn refresh_target(cmd: &crate::Cmd) -> Option<PathBuf> {
    use crate::Cmd::*;
    match cmd {
        Clone { dir, .. } => Some(dir.clone()),
        Commit { .. }
        | Restore { .. }
        | Switch { .. }
        | Merge { .. }
        | CherryPick { .. }
        | Revert { .. }
        | Rebase { .. }
        | Apply(_)
        | Am(_)
        | Resolve { .. }
        | Zone(_)
        | Worktree(_)
        | Snapshot(_)
        | Sparse(_)
        | Stash(_)
        | Sync
        | Op(_) => Some(PathBuf::from(".")),
        _ => None,
    }
}

/// Best effort: a failure here never fails the command that already ran.
pub fn refresh(dir: &Path) {
    if let Ok(repo) = Repo::discover(dir) {
        let _ = repo.lockable_refresh_all();
    }
}

fn offline_policy() -> Result<bool, Error> {
    match config::Config::load()?.get("lock.offline").as_deref() {
        None | Some("warn") => Ok(false),
        Some("refuse") => Ok(true),
        Some(o) => msg(format!("lock.offline must be 'warn' or 'refuse', got '{o}'")),
    }
}

/// Ask the remote whether the locks we believe we hold are still ours. Returns the lost ones.
/// Unreachable remote: `lock.offline=warn` (default) trusts the local record and warns,
/// `refuse` fails.
fn verify_held(repo: &Repo, held: &[String]) -> Result<Vec<String>, Error> {
    let Some(url) = repo.remote_url()? else { return Ok(vec![]) };
    let rows = match remote::open_backend(&url).and_then(|b| repo.list_locks(b.as_ref())) {
        Ok(r) => r,
        Err(e) => {
            if offline_policy()? {
                return msg(format!(
                    "cannot reach the remote to verify your locks ({e}) and lock.offline=refuse\n\nOptions:\n  retry when online\n  vcs config set lock.offline warn"
                ));
            }
            eprintln!("warning: cannot reach the remote ({e}); trusting your local lock record (lock.offline=warn). The server re-checks at publish.");
            return Ok(vec![]);
        }
    };
    let defs = repo.zones()?;
    let me = crate::principal();
    let local = repo.lockable_held()?;
    let now = now_ms();
    let mut lost = Vec::new();
    for p in held {
        let z = defs.owner(p).to_owned();
        let rel = defs.relative(&z, p).to_owned();
        let tok = local.get(p).map(|h| h.token);
        let ok = rows
            .iter()
            .any(|(zl, rp, l)| zl == defs.label(&z) && *rp == rel && l.holder == me && Some(l.token) == tok && l.expires_at > now);
        if !ok {
            lost.push(p.clone());
        }
    }
    Ok(lost)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Add,
    Commit,
}

/// Refuse to stage/commit a change to a lockable tracked file unless its lock is held.
/// `under`: repo-relative path prefixes the `add` was given (empty = everything).
pub fn guard(repo: &Repo, stage: Stage, under: &[String], force: bool) -> Result<(), Error> {
    if repo.attrs()?.is_empty() {
        return Ok(());
    }
    let st = repo.status()?;
    let mut changed: Vec<String> = Vec::new();
    for z in &st.zones {
        let list = if stage == Stage::Add { &z.unstaged } else { &z.staged };
        for c in list {
            if c.kind == ChangeKind::Added {
                continue; // a new file has no committed content to lose; nothing to lock yet
            }
            let covered = under.is_empty()
                || under
                    .iter()
                    .any(|u| u.is_empty() || c.path == *u || c.path.starts_with(&format!("{u}/")));
            if covered {
                changed.push(c.path.clone());
            }
        }
    }
    let mut unlocked = repo.lockable_unlocked(&changed)?;
    let lockable_changed: Vec<String> = {
        let attrs = repo.attrs()?;
        changed.into_iter().filter(|p| attrs.lookup(p).lockable).collect()
    };
    let held = repo.lockable_held_among(&lockable_changed)?;
    if force {
        if !unlocked.is_empty() {
            eprintln!(
                "warning: --force-no-lock: changing lockable file(s) without a lock: {}",
                unlocked.join(", ")
            );
        }
        return Ok(());
    }
    let mut lost = Vec::new();
    if !held.is_empty() {
        lost = verify_held(repo, &held)?;
        if !lost.is_empty() {
            repo.lockable_note_released(&lost)?;
            unlocked.extend(lost.iter().cloned());
        }
    }
    if unlocked.is_empty() {
        return Ok(());
    }
    unlocked.sort();
    unlocked.dedup();
    if repo.remote_url()?.is_none() {
        eprintln!(
            "warning: lockable file(s) changed without a lock: {} (no remote is configured, so locks cannot be taken; advisory only)",
            unlocked.join(", ")
        );
        return Ok(());
    }
    let verb = if stage == Stage::Add { "stage" } else { "commit" };
    let cmd = if stage == Stage::Add { "add" } else { "commit" };
    let had = repo.lockable_held()?;
    let mut s = format!("refusing to {verb} lockable file(s) without a lock (see .vcsattributes):\n");
    for p in &unlocked {
        let why = if lost.contains(p) || had.contains_key(p) {
            "  (your lock expired or was broken)"
        } else {
            ""
        };
        s.push_str(&format!("  {p}{why}\n"));
    }
    s.push_str("\nOptions:\n  vcs lock <path> --lease-secs <n>   take the lock first (also makes the file writable)\n");
    s.push_str(&format!(
        "  vcs {cmd} --force-no-lock ...        override locally; the server's require-lock policy can still refuse the publish"
    ));
    msg(s)
}

fn map_lock_err(e: Error) -> Error {
    let t = e.to_string();
    if t.contains("Io") || t.contains("connect") || t.contains("unreachable") || t.contains("No such file") {
        return Error::Msg(format!(
            "cannot reach the remote: {t}\n\nLocks are remote operations and cannot be taken offline."
        ));
    }
    e
}

/// After `vcs lock`: record held locks and make the files writable.
pub fn after_lock(repo: &Repo, rel: &[String], got: &[(String, String, u64)], lease_ms: u64) -> Result<(), Error> {
    let defs = repo.zones()?;
    let mut v = Vec::new();
    for p in rel {
        let z = defs.owner(p).to_owned();
        let r = defs.relative(&z, p).to_owned();
        if let Some((_, _, t)) = got.iter().find(|(gz, gp, _)| *gz == z && *gp == r) {
            v.push((p.clone(), *t));
        }
    }
    repo.lockable_note_acquired(&v, lease_ms)
}

pub fn wrap_lock<T>(r: Result<T, Error>) -> Result<T, Error> {
    r.map_err(map_lock_err)
}

/// After `vcs unlock`: forget the locks and make lockable files read-only again.
pub fn after_unlock(repo: &Repo, rel: &[String]) -> Result<(), Error> {
    repo.lockable_note_released(rel)
}

/// `lock.releaseOnPublish=on`: after a successful publish, release held locks whose files have
/// no uncommitted changes left.
pub fn after_publish(repo: &Repo) -> Result<(), Error> {
    if config::Config::load()?.get("lock.releaseOnPublish").as_deref() != Some("on") {
        return Ok(());
    }
    let held: Vec<String> = repo.lockable_held()?.into_keys().collect();
    if held.is_empty() {
        return Ok(());
    }
    let st = repo.status()?;
    let dirty: std::collections::BTreeSet<&str> = st
        .zones
        .iter()
        .flat_map(|z| z.staged.iter().chain(z.unstaged.iter()))
        .map(|c| c.path.as_str())
        .collect();
    let release: Vec<String> = held.into_iter().filter(|p| !dirty.contains(p.as_str())).collect();
    if release.is_empty() {
        return Ok(());
    }
    let Some(url) = repo.remote_url()? else { return Ok(()) };
    let backend = remote::open_backend(&url)?;
    // locks already gone on the remote are only forgotten locally
    let rows = repo.list_locks(backend.as_ref())?;
    let defs = repo.zones()?;
    let me = crate::principal();
    let mine: Vec<String> = release
        .iter()
        .filter(|p| {
            let z = defs.owner(p).to_owned();
            let r = defs.relative(&z, p);
            rows.iter().any(|(zl, rp, l)| zl == defs.label(&z) && rp == r && l.holder == me)
        })
        .cloned()
        .collect();
    if !mine.is_empty() {
        repo.unlock_paths(backend.as_ref(), &mine, false, crate::author()?, &me)?;
    }
    repo.lockable_note_released(&release)?;
    println!("released {} lock(s) after publish (lock.releaseOnPublish)", release.len());
    Ok(())
}

/// `vcs status`: lockable files modified without holding the lock.
pub fn status_section(repo: &Repo, st: &vcs_repo::Status) -> Result<(), Error> {
    let mut paths: Vec<String> = st
        .zones
        .iter()
        .flat_map(|z| z.staged.iter().chain(z.unstaged.iter()))
        .filter(|c| c.kind != ChangeKind::Added)
        .map(|c| c.path.clone())
        .collect();
    paths.sort();
    paths.dedup();
    let un = repo.lockable_unlocked(&paths)?;
    if !un.is_empty() {
        println!("lockable files modified without a lock:");
        for p in un {
            println!("    {p}");
        }
        println!("  (take the lock with `vcs lock <path> --lease-secs <n>`, or the publish may be refused)");
    }
    Ok(())
}

fn human(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h{}m", s / 3600, s % 3600 / 60),
        _ => format!("{}d{}h", s / 86400, s % 86400 / 3600),
    }
}

/// One line of `vcs locks`.
pub fn lock_line(zone: &str, path: &str, l: &vcs_remote::Lock, now: u64) -> String {
    let state = if l.expires_at > now { "held" } else { "expired" };
    let age = human(now.saturating_sub(l.acquired_at));
    let exp = if l.expires_at > now {
        format!("expires in {}", human(l.expires_at - now))
    } else {
        format!(
            "expired {} ago (stale; anyone can take it, or an admin can `vcs unlock --break`)",
            human(now - l.expires_at)
        )
    };
    format!(
        "{path} (zone {zone}) {state} by {} age {age}, {exp} (until {} unix ms) token {}  {}",
        l.holder, l.expires_at, l.token, l.reason
    )
}
