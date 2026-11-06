//! `vcs mergetool`: run a configured merge tool on each conflicted file of a merge or rebase, then
//! mark the file resolved.
//!
//! The tool is named by `--tool` or `merge.tool`; its command line is `mergetool.<name>.cmd` (user
//! or system config only, like every setting that names a program). It runs through `sh -c` in the
//! worktree root with `$BASE`, `$LOCAL` (ours), `$REMOTE` (theirs) and `$MERGED` (the file in the
//! working tree, with the conflict markers) set; the tool writes its result to `$MERGED`.
//!
//! As in git, unless `mergetool.<name>.trustExitCode` is on, the run counts as a success when
//! `$MERGED` changed; if it did not, the user is asked ("Was the merge successful?"). A result that
//! still contains conflict markers is never accepted. Answers and the "hit return" prompt come from
//! standard input, so a script can feed them.
use crate::config::Config;
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use vcs_repo::merge::{has_conflict_markers, kind_name};
use vcs_repo::{Error, Repo, Resolution};

fn msg<T>(s: impl Into<String>) -> Result<T, Error> {
    Err(Error::Msg(s.into()))
}

pub struct Opts {
    pub tool: Option<String>,
    pub no_prompt: bool,
    pub paths: Vec<String>,
}

fn flag(cfg: &Config, key: &str, default: bool) -> bool {
    match cfg.get(key).as_deref() {
        Some("on" | "true" | "yes" | "1") => true,
        Some("off" | "false" | "no" | "0") => false,
        _ => default,
    }
}

pub fn run(repo: &Repo, cwd: &Path, opts: Opts) -> Result<(), Error> {
    let stdin = std::io::stdin();
    let mut inp = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    run_with(repo, cwd, opts, &mut inp, &mut out)
}

fn ask(inp: &mut dyn BufRead, out: &mut dyn Write, prompt: &str) -> Option<String> {
    let _ = out.write_all(prompt.as_bytes());
    let _ = out.flush();
    let mut l = String::new();
    match inp.read_line(&mut l) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(l.trim().to_owned()),
    }
}

pub fn run_with(repo: &Repo, cwd: &Path, opts: Opts, inp: &mut dyn BufRead, out: &mut dyn Write) -> Result<(), Error> {
    let state = repo
        .merge_state()?
        .ok_or_else(|| Error::Msg("no merge or rebase in progress".into()))?;
    let specs: Vec<String> = opts.paths.iter().map(|p| repo.rel_path(cwd, p)).collect::<Result<_, _>>()?;
    let todo: Vec<_> = state
        .unresolved()
        .into_iter()
        .filter(|c| {
            specs.is_empty()
                || specs
                    .iter()
                    .any(|s| s.is_empty() || c.path == *s || c.path.starts_with(&format!("{s}/")))
        })
        .cloned()
        .collect();
    if todo.is_empty() {
        let _ = writeln!(out, "No files need merging");
        return Ok(());
    }
    let cfg = Config::load()?;
    let name = match opts.tool.clone().or_else(|| cfg.get("merge.tool")) {
        Some(n) => n,
        None => {
            return msg("no merge tool configured\n\nName one and give its command:\n  vcs config set --user merge.tool mytool\n  vcs config set --user mergetool.mytool.cmd 'mytool \"$LOCAL\" \"$BASE\" \"$REMOTE\" -o \"$MERGED\"'");
        }
    };
    let Some(cmd) = cfg.mergetool_cmd(&name) else {
        return msg(format!(
            "merge tool '{name}' has no command; set it with `vcs config set --user mergetool.{name}.cmd '<command using $BASE $LOCAL $REMOTE $MERGED>'`"
        ));
    };
    let trust = flag(&cfg, &format!("mergetool.{name}.trustExitCode"), false);
    let keep = flag(&cfg, "mergetool.keepBackup", true);
    let prompt = !opts.no_prompt && flag(&cfg, "mergetool.prompt", true);
    let tmp = repo.worktree().join(".vcs").join(format!("mergetool-{}", std::process::id()));
    let io = |p: &Path, source| Error::Io {
        path: p.to_owned(),
        source,
    };
    std::fs::create_dir_all(&tmp).map_err(|e| io(&tmp, e))?;
    let _ = writeln!(out, "Merging:");
    for c in &todo {
        let _ = writeln!(out, "{}", c.path);
    }
    let mut failed = 0usize;
    for c in &todo {
        let (Some(left), Some(right)) = (c.left, c.right) else {
            let _ = writeln!(
                out,
                "\n{}: {} conflict, one side has no file; choose with `vcs resolve --ours|--theirs {}`; skipped",
                c.path,
                kind_name(c.kind),
                c.path
            );
            failed += 1;
            continue;
        };
        let _ = writeln!(
            out,
            "\nNormal merge conflict for '{}':\n  {{local}}: modified file\n  {{remote}}: modified file",
            c.path
        );
        if prompt {
            ask(inp, out, &format!("Hit return to start merge resolution tool ({name}): "));
        }
        let file = Path::new(&c.path)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (stem, ext) = match file.rfind('.') {
            Some(i) if i > 0 => (file[..i].to_owned(), file[i..].to_owned()),
            _ => (file.clone(), String::new()),
        };
        let pid = std::process::id();
        let side = |tag: &str, data: &[u8]| -> Result<std::path::PathBuf, Error> {
            let p = tmp.join(format!("{stem}_{tag}_{pid}{ext}"));
            std::fs::write(&p, data).map_err(|e| io(&p, e))?;
            Ok(p)
        };
        let base_data = match c.base {
            Some(b) => repo.content(&b)?,
            None => Vec::new(),
        };
        let (base, local, remote) = (
            side("BASE", &base_data)?,
            side("LOCAL", &repo.content(&left)?)?,
            side("REMOTE", &repo.content(&right)?)?,
        );
        let merged = repo.worktree().join(&c.path);
        let before = std::fs::read(&merged).map_err(|e| io(&merged, e))?;
        let backup = merged.with_file_name(format!("{file}.orig"));
        if keep {
            std::fs::write(&backup, &before).map_err(|e| io(&backup, e))?;
        }
        let mut sh = std::process::Command::new("sh");
        sh.arg("-c")
            .arg(&cmd)
            .current_dir(repo.worktree())
            .env("BASE", &base)
            .env("LOCAL", &local)
            .env("REMOTE", &remote)
            .env("MERGED", &merged);
        if !std::io::stdin().is_terminal() {
            sh.stdin(std::process::Stdio::null());
        }
        let status = sh
            .status()
            .map_err(|e| Error::Msg(format!("could not start merge tool '{name}': {e}")))?;
        let after = std::fs::read(&merged).unwrap_or_default();
        let ok = if trust {
            status.success()
        } else if after != before {
            true
        } else {
            matches!(
                ask(
                    inp,
                    out,
                    &format!("{} seems unchanged.\nWas the merge successful [{}]? [y/n] ", c.path, name)
                )
                .as_deref(),
                Some("y" | "Y" | "yes")
            )
        };
        let resolved = if !ok {
            let _ = writeln!(out, "{}: merge tool '{name}' did not resolve it", c.path);
            false
        } else if has_conflict_markers(&after) {
            let _ = writeln!(out, "{}: conflict markers remain; not marked resolved", c.path);
            false
        } else {
            match repo.resolve(&c.path, &Resolution::Mark) {
                Ok(()) => {
                    let _ = writeln!(out, "resolved {}", c.path);
                    true
                }
                Err(e) => {
                    let _ = writeln!(out, "{}: {e}", c.path);
                    false
                }
            }
        };
        if resolved && !keep {
            let _ = std::fs::remove_file(&backup);
        }
        if !resolved {
            failed += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    if failed > 0 {
        return msg(format!("{failed} file(s) still unresolved"));
    }
    if let Some(k) = repo.merge_state()? {
        let _ = writeln!(out, "{} conflict(s) left", k.unresolved().len());
    }
    Ok(())
}
