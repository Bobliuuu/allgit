//! `vcs watch`: journal changed paths for [`vcs_repo::fsmonitor::JournalMonitor`] (crate `vcs-watchd`).
//!
//! Linux (inotify) is the tested backend. The macOS (FSEvents) backend is whatever the `notify`
//! crate provides; it has not been run here. One watch is added per directory (non-recursive),
//! so the daemon controls the set of watches: new directories are watched and scanned (files
//! created before the watch existed are journaled), and any directory rename, event-queue
//! overflow or `notify` error makes it write `O` and rebuild every watch. See `vcs_repo::fsmonitor` for the
//! journal format and the barrier protocol.

use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::Duration;
use vcs_objects::names::META_DIR;
use vcs_repo::error::{Error, Result};
use vcs_repo::fsmonitor::{DIR, JOURNAL, PID, PROBE_DIR};
use vcs_repo::Repo;

const ROTATE_AT: u64 = 32 << 20;

/// One recursive watch on the root instead of one watch per directory. FSEvents is recursive by
/// nature and every extra stream costs a thread-synchronised setup: per-directory watches took
/// minutes to start on a 2,000-directory tree (macOS, loaded host) where the single stream starts at
/// once. Directories created or renamed need no bookkeeping then. inotify has no recursive mode
/// (notify would add the per-directory watches itself, without the new-directory scan), so Linux keeps
/// the explicit per-directory set. `VCS_WATCH_PER_DIR=1` forces the explicit set (tests).
fn recursive() -> bool {
    cfg!(target_os = "macos") && std::env::var_os("VCS_WATCH_PER_DIR").is_none()
}

/// Which directories get a watch. A directory owned by a zone this worktree is not subscribed to is
/// never walked by `status` (its untracked scan skips it), so watching it only costs descriptors and
/// events. Sparse sets do NOT narrow this: the untracked walk still reports new names there.
#[derive(Clone, Default)]
struct Scope {
    zones: Option<vcs_repo::meta::Zones>,
    subs: std::collections::BTreeSet<String>,
    key: String,
}

impl Scope {
    fn load(repo: &Repo) -> Scope {
        match (repo.zones(), repo.workspace()) {
            (Ok(z), Ok(w)) if z.names().count() > 1 => {
                let subs: std::collections::BTreeSet<String> = w.subs.keys().cloned().collect();
                let mut key = String::new();
                for n in z.names() {
                    key.push_str(&format!("{n}={}\n", z.path_of(n).unwrap_or("")));
                }
                key.push_str(&format!("{subs:?}"));
                Scope { zones: Some(z), subs, key }
            }
            _ => Scope::default(),
        }
    }
    /// Should the directory at repo-relative `rel` (empty for the root) be watched?
    fn watches(&self, rel: &str) -> bool {
        match &self.zones {
            Some(z) if !rel.is_empty() => self.subs.contains(z.owner(rel)),
            _ => true,
        }
    }
}

struct Daemon {
    root: PathBuf,
    dir: PathBuf,
    jpath: PathBuf,
    journal: File,
    jlen: u64,
    watcher: notify::RecommendedWatcher,
    rx: Receiver<notify::Result<Event>>,
    dirs: HashSet<PathBuf>,
    idle: Option<Duration>,
    last_query: std::time::Instant,
    scope: Scope,
    last_scope_check: std::time::Instant,
    repo: PathBuf,
}

fn werr(e: impl std::fmt::Display) -> Error {
    Error::Msg(format!("watcher: {e}"))
}

fn new_epoch() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{n:x}-{}", std::process::id())
}

/// Run the watcher for `repo` until `stop` is set. Removes the journal on the way out.
pub fn run(repo: &Repo, stop: &AtomicBool) -> Result<()> {
    run_idle(repo, stop, None)
}

/// [`run`] that also stops once no client has queried the journal (dropped a probe) for `idle`.
/// A watcher started on its own by a command (not by the user) uses this so it does not outlive
/// the work that wanted it.
pub fn run_idle(repo: &Repo, stop: &AtomicBool, idle: Option<Duration>) -> Result<()> {
    let root = repo.worktree().to_path_buf();
    let dir = repo.worktree().join(META_DIR).join(DIR);
    let probe = dir.join(PROBE_DIR);
    let _ = fs::remove_dir_all(&probe);
    fs::create_dir_all(&probe).map_err(werr)?;
    let _ = fs::remove_file(dir.join(JOURNAL));
    let scope = Scope::load(repo);
    let (watcher, rx, dirs) = build(&root, &probe, &scope)?;
    let jpath = dir.join(JOURNAL);
    let (journal, jlen) = fresh_journal(&jpath)?;
    fs::write(dir.join(PID), std::process::id().to_string()).map_err(werr)?;
    let mut d = Daemon {
        root,
        dir,
        jpath,
        journal,
        jlen,
        watcher,
        rx,
        dirs,
        idle,
        last_query: std::time::Instant::now(),
        scope,
        last_scope_check: std::time::Instant::now(),
        repo: repo.worktree().to_path_buf(),
    };
    let r = d.event_loop(stop);
    let _ = fs::remove_file(d.dir.join(PID));
    let _ = fs::remove_file(&d.jpath);
    r
}

fn fresh_journal(path: &Path) -> Result<(File, u64)> {
    let tmp = path.with_extension("new");
    let head = format!("E {}\n", new_epoch());
    fs::write(&tmp, &head).map_err(werr)?;
    fs::rename(&tmp, path).map_err(werr)?;
    let f = OpenOptions::new().append(true).open(path).map_err(werr)?;
    Ok((f, head.len() as u64))
}

type Built = (notify::RecommendedWatcher, Receiver<notify::Result<Event>>, HashSet<PathBuf>);

/// A watcher with one non-recursive watch per directory of the worktree (not `.vcs`) plus the
/// probe directory.
fn build(root: &Path, probe: &Path, scope: &Scope) -> Result<Built> {
    let (tx, rx) = channel();
    let mut w = notify::recommended_watcher(tx).map_err(werr)?;
    let mut dirs = HashSet::new();
    if recursive() {
        w.watch(root, RecursiveMode::Recursive).map_err(werr)?;
    } else {
        watch_tree(&mut w, root, root, scope, &mut dirs, &mut |_| {})?;
    }
    w.watch(probe, RecursiveMode::NonRecursive).map_err(werr)?;
    Ok((w, rx, dirs))
}

/// Watch `start` and every directory beneath it; `found` is called with each path seen.
fn watch_tree(
    w: &mut notify::RecommendedWatcher,
    root: &Path,
    start: &Path,
    scope: &Scope,
    dirs: &mut HashSet<PathBuf>,
    found: &mut dyn FnMut(&Path),
) -> Result<()> {
    let mut stack = vec![start.to_path_buf()];
    while let Some(d) = stack.pop() {
        if d == root.join(META_DIR) {
            continue;
        }
        if d != start {
            found(&d);
        }
        if !d
            .strip_prefix(root)
            .ok()
            .and_then(|r| r.to_str())
            .is_none_or(|r| scope.watches(&r.replace(std::path::MAIN_SEPARATOR, "/")))
        {
            continue;
        }
        w.watch(&d, RecursiveMode::NonRecursive).map_err(werr)?;
        dirs.insert(d.clone());
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(_) => found(&p),
                Err(_) => {}
            }
        }
    }
    Ok(())
}

impl Daemon {
    fn append(&mut self, buf: &str) -> Result<()> {
        if self.jlen > ROTATE_AT {
            // New epoch: every saved token becomes invalid, which is exactly right.
            let (f, n) = fresh_journal(&self.jpath)?;
            self.journal = f;
            self.jlen = n;
        }
        self.journal.write_all(buf.as_bytes()).map_err(werr)?;
        self.jlen += buf.len() as u64;
        Ok(())
    }

    fn rel(&self, p: &Path) -> Option<String> {
        p.strip_prefix(&self.root)
            .ok()
            .and_then(|r| r.to_str())
            .map(|s| s.replace(std::path::MAIN_SEPARATOR, "/"))
    }

    /// Replace every watch and say that events may have been lost.
    fn rebuild(&mut self) -> Result<()> {
        let (w, rx, dirs) = build(&self.root, &self.dir.join(PROBE_DIR), &self.scope)?;
        self.watcher = w;
        self.rx = rx;
        self.dirs = dirs;
        self.append("O\n")
    }

    fn event_loop(&mut self, stop: &AtomicBool) -> Result<()> {
        while !stop.load(Ordering::Relaxed) {
            let first = match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(e) => e,
                Err(RecvTimeoutError::Timeout) => {
                    if self.last_scope_check.elapsed() > Duration::from_millis(500) {
                        self.last_scope_check = std::time::Instant::now();
                        if let Ok(r) = Repo::discover(&self.repo) {
                            let now = Scope::load(&r);
                            if now.key != self.scope.key {
                                self.scope = now;
                                if let Err(e) = self.rebuild() {
                                    let _ = self.append("X\n");
                                    return Err(e);
                                }
                            }
                        }
                    }
                    if self.idle.is_some_and(|i| self.last_query.elapsed() > i) {
                        return Ok(());
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.append("X\n")?;
                    return Err(werr("event channel closed"));
                }
            };
            let mut batch = vec![first];
            while let Ok(e) = self.rx.try_recv() {
                batch.push(e);
            }
            let mut out = String::new();
            let mut rebuild = false;
            for ev in batch {
                match ev {
                    Ok(ev) => self.handle(&ev, &mut out, &mut rebuild),
                    Err(_) => rebuild = true,
                }
            }
            if rebuild {
                // Journal what we know, then the loss marker once the new watches are live.
                self.append(&out)?;
                if let Err(e) = self.rebuild() {
                    let _ = self.append("X\n");
                    return Err(e);
                }
            } else if !out.is_empty() {
                self.append(&out)?;
            }
            if out.contains("\nB ") || out.starts_with("B ") {
                self.last_query = std::time::Instant::now();
            }
        }
        Ok(())
    }

    fn handle(&mut self, ev: &Event, out: &mut String, rebuild: &mut bool) {
        if ev.need_rescan() {
            *rebuild = true;
        }
        if matches!(ev.kind, EventKind::Access(_)) {
            return;
        }
        for p in &ev.paths {
            let Some(rel) = self.rel(p) else {
                // Probe files live under .vcs/fsmon/probe: answer the barrier.
                if let (Some(name), true) = (p.file_name().and_then(|n| n.to_str()), p.starts_with(self.dir.join(PROBE_DIR))) {
                    if !matches!(ev.kind, EventKind::Remove(_)) {
                        out.push_str(&format!("B {name}\n"));
                    }
                }
                continue;
            };
            if rel.is_empty() || rel.eq_ignore_ascii_case(META_DIR) || rel.to_ascii_lowercase().starts_with(".vcs/") {
                if p.starts_with(self.dir.join(PROBE_DIR)) && !matches!(ev.kind, EventKind::Remove(_)) {
                    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                        out.push_str(&format!("B {name}\n"));
                    }
                }
                continue;
            }
            if rel.contains('\n') {
                *rebuild = true;
                continue;
            }
            if recursive() {
                // Events from a directory owned by an unsubscribed zone are not wanted (status never
                // looks there); the explicit-watch mode never sees them because it does not watch them.
                if !self.scope.watches(rel.rsplit_once('/').map_or("", |(d, _)| d)) {
                    continue;
                }
                out.push_str(&format!("P {rel}\n"));
                continue;
            }
            out.push_str(&format!("P {rel}\n"));
            let was_dir = self.dirs.contains(p);
            let is_dir = fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
            let gone = !matches!(ev.kind, EventKind::Create(_)) && fs::symlink_metadata(p).is_err();
            if was_dir && (gone || matches!(ev.kind, EventKind::Modify(notify::event::ModifyKind::Name(_)))) {
                // A watched directory moved or vanished: our path -> watch map is stale.
                *rebuild = true;
            } else if is_dir && !was_dir && !*rebuild {
                let mut found = Vec::new();
                let mut dirs = std::mem::take(&mut self.dirs);
                let r = watch_tree(&mut self.watcher, &self.root, p, &self.scope, &mut dirs, &mut |f| {
                    found.push(f.to_path_buf())
                });
                self.dirs = dirs;
                if r.is_err() {
                    *rebuild = true;
                }
                for f in found {
                    if let Some(r) = self.rel(&f) {
                        if !r.contains('\n') {
                            out.push_str(&format!("P {r}\n"));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsubscribed_zone_directories_are_not_watched() {
        let d = std::env::temp_dir().join(format!("watchd-scope-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let r = Repo::init(&d).unwrap();
        r.create_zone("assets", "assets/big").unwrap();
        let mut s = Scope::load(&r);
        assert!(s.watches("") && s.watches("src") && s.watches("assets") && s.watches("assets/big/x"));
        s.subs.remove("assets");
        assert!(s.watches("") && s.watches("src") && s.watches("assets"));
        assert!(!s.watches("assets/big") && !s.watches("assets/big/x/y"));
        let _ = fs::remove_dir_all(&d);
    }
}
