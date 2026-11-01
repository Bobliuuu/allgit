//! `vcs watch`: journal changed paths for [`crate::fsmonitor::JournalMonitor`] (feature `watcher`).
//!
//! Linux (inotify) is the tested backend. The macOS (FSEvents) backend is whatever the `notify`
//! crate provides; it has not been run here. One watch is added per directory (non-recursive),
//! so the daemon controls the set of watches: new directories are watched and scanned (files
//! created before the watch existed are journaled), and any directory rename, event-queue
//! overflow or `notify` error makes it write `O` and rebuild every watch. See `fsmonitor` for the
//! journal format and the barrier protocol.

use crate::error::{Error, Result};
use crate::fsmonitor::{DIR, JOURNAL, PID, PROBE_DIR};
use crate::repo::Repo;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::Duration;
use vcs_objects::names::META_DIR;

const ROTATE_AT: u64 = 32 << 20;

struct Daemon {
    root: PathBuf,
    dir: PathBuf,
    jpath: PathBuf,
    journal: File,
    jlen: u64,
    watcher: notify::RecommendedWatcher,
    rx: Receiver<notify::Result<Event>>,
    dirs: HashSet<PathBuf>,
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
    let root = repo.worktree().to_path_buf();
    let dir = repo.vcs.join(DIR);
    let probe = dir.join(PROBE_DIR);
    let _ = fs::remove_dir_all(&probe);
    fs::create_dir_all(&probe).map_err(werr)?;
    let _ = fs::remove_file(dir.join(JOURNAL));
    let (watcher, rx, dirs) = build(&root, &probe)?;
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
fn build(root: &Path, probe: &Path) -> Result<Built> {
    let (tx, rx) = channel();
    let mut w = notify::recommended_watcher(tx).map_err(werr)?;
    let mut dirs = HashSet::new();
    watch_tree(&mut w, root, root, &mut dirs, &mut |_| {})?;
    w.watch(probe, RecursiveMode::NonRecursive).map_err(werr)?;
    Ok((w, rx, dirs))
}

/// Watch `start` and every directory beneath it; `found` is called with each path seen.
fn watch_tree(
    w: &mut notify::RecommendedWatcher,
    root: &Path,
    start: &Path,
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
        let (w, rx, dirs) = build(&self.root, &self.dir.join(PROBE_DIR))?;
        self.watcher = w;
        self.rx = rx;
        self.dirs = dirs;
        self.append("O\n")
    }

    fn event_loop(&mut self, stop: &AtomicBool) -> Result<()> {
        while !stop.load(Ordering::Relaxed) {
            let first = match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(e) => e,
                Err(RecvTimeoutError::Timeout) => continue,
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
                let r = watch_tree(&mut self.watcher, &self.root, p, &mut dirs, &mut |f| found.push(f.to_path_buf()));
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
