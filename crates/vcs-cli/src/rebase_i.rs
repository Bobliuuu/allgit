//! `vcs rebase -i|-x|--skip` and the editors it runs. The engine is `vcs_repo::rebase`; this file
//! builds the todo, runs `GIT_SEQUENCE_EDITOR`/`VCS_SEQUENCE_EDITOR` (else the commit editor) on
//! it, and edits messages for `reword`/`squash`.

use crate::config::Config;
use std::path::{Path, PathBuf};
use vcs_objects::Signature;
use vcs_repo::rebase_todo;
use vcs_repo::{Error, RebaseOutcome, Repo};

type Res<T> = Result<T, Error>;

fn err<T>(m: impl Into<String>) -> Res<T> {
    Err(Error::Msg(m.into()))
}

/// The editor for the todo list: `VCS_SEQUENCE_EDITOR`, `GIT_SEQUENCE_EDITOR`, `GIT_EDITOR`,
/// then core.editor / `$VISUAL` / `$EDITOR`.
fn sequence_editor(cfg: &Config) -> Option<String> {
    ["VCS_SEQUENCE_EDITOR", "GIT_SEQUENCE_EDITOR"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
        .or_else(|| message_editor(cfg))
}

/// The editor for commit messages: `GIT_EDITOR`, then core.editor / `$VISUAL` / `$EDITOR`.
fn message_editor(cfg: &Config) -> Option<String> {
    std::env::var("GIT_EDITOR")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| cfg.editor())
}

/// A private scratch directory for editor files; removed on drop.
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Res<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let d = std::env::temp_dir().join(format!("vcs-rebase-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&d).map_err(|source| Error::Io { path: d.clone(), source })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
        }
        Ok(Scratch(d))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run `editor file`; the file is read back as UTF-8.
fn run_editor(editor: &str, file: &Path) -> Res<String> {
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg("vcs-editor")
        .arg(file)
        .status()
        .map_err(|e| Error::Msg(format!("could not start editor '{editor}': {e}")))?;
    if !status.success() {
        return err(format!("editor '{editor}' exited with {status}"));
    }
    match String::from_utf8(std::fs::read(file).map_err(|source| Error::Io { path: file.into(), source })?) {
        Ok(t) => Ok(t),
        Err(_) => err("the editor left a file that is not valid UTF-8"),
    }
}

/// Edit a commit message for `reword`/`squash`. Unchanged text is returned as is; otherwise
/// `#` lines are dropped and an empty result is an error.
fn edit_msg(cfg: &Config, scratch: &Scratch, initial: &str) -> Res<String> {
    let Some(editor) = message_editor(cfg) else {
        return err("no editor configured for the commit message (set $GIT_EDITOR, core.editor or $EDITOR)");
    };
    let file = scratch.0.join("COMMIT_EDITMSG");
    let seed = format!("{initial}\n\n# Edit the commit message. Lines starting with '#' are ignored.\n# An empty message aborts.\n");
    std::fs::write(&file, &seed).map_err(|source| Error::Io {
        path: file.clone(),
        source,
    })?;
    let text = run_editor(&editor, &file)?;
    if text == seed {
        return Ok(initial.to_owned());
    }
    let kept: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
    let m = kept.join("\n").trim().to_owned();
    if m.is_empty() {
        return err("empty commit message");
    }
    Ok(m)
}

pub struct Args {
    pub interactive: bool,
    pub autosquash: bool,
    pub exec: Vec<String>,
}

/// `vcs rebase -i|-x <upstream>`.
pub fn start(repo: &Repo, zone: &str, onto: &str, a: &Args, author: Signature) -> Res<RebaseOutcome> {
    let cfg = Config::load()?;
    let plan = repo.rebase_plan(zone, onto)?;
    let lines = rebase_todo::default_todo(&plan.commits, a.autosquash, &a.exec);
    let scratch = Scratch::new()?;
    let steps = if a.interactive {
        let Some(editor) = sequence_editor(&cfg) else {
            return err("no editor for the todo list\n\nSet GIT_SEQUENCE_EDITOR (or VCS_SEQUENCE_EDITOR), core.editor or $EDITOR; scripts can use a command such as `sed -i ...`");
        };
        let dir = scratch.0.join("todo");
        std::fs::create_dir(&dir).map_err(|source| Error::Io { path: dir.clone(), source })?;
        let file = dir.join("git-rebase-todo");
        std::fs::write(&file, rebase_todo::render(&lines, &plan.commits, onto)).map_err(|source| Error::Io {
            path: file.clone(),
            source,
        })?;
        let text = run_editor(&editor, &file).map_err(|e| Error::Msg(format!("{e}; rebase not started")))?;
        let steps = rebase_todo::parse(&text, &plan.commits)?;
        if !steps.iter().any(|s| matches!(s, vcs_repo::Step::Pick(..)))
            && text.lines().all(|l| l.trim().is_empty() || l.trim_start().starts_with('#'))
        {
            return err("nothing to do: the todo list is empty; rebase not started");
        }
        steps
    } else {
        rebase_todo::parse(&rebase_todo::render(&lines, &plan.commits, onto), &plan.commits)?
    };
    let needs_msg = steps
        .iter()
        .any(|s| matches!(s, vcs_repo::Step::Pick(vcs_repo::Act::Reword | vcs_repo::Act::Squash, _)));
    if needs_msg && message_editor(&cfg).is_none() {
        return err("reword/squash need an editor for the commit message (set $GIT_EDITOR, core.editor or $EDITOR); nothing was changed");
    }
    let mut ed = |m: &str| edit_msg(&cfg, &scratch, m);
    repo.rebase_run(&plan, steps, author, &mut ed)
}

/// `--continue` / `--skip`.
pub fn resume(repo: &Repo, skip: bool, author: Signature) -> Res<RebaseOutcome> {
    let cfg = Config::load()?;
    let scratch = Scratch::new()?;
    let mut ed = |m: &str| edit_msg(&cfg, &scratch, m);
    if skip {
        repo.rebase_skip(author, &mut ed)
    } else {
        repo.rebase_continue_with(author, &mut ed)
    }
}
