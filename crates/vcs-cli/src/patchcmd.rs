//! `vcs format-patch`, `vcs apply` and `vcs am`: patch files, no git, no mail server.
//! The engine is `vcs_repo::patch_repo` (plans everything in memory before touching the tree).

use clap::Args;
use std::io::Read;
use std::path::{Path, PathBuf};
use vcs_repo::{AmOutcome, ApplyKind, ApplyOpts, Error, Repo};

fn err<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Msg(m.into()))
}

#[derive(Args, Clone)]
pub struct FormatPatchArgs {
    /// Commits to turn into patches: `A..B` (commits of B not in A), or a revision (commits on the branch not in it)
    pub rev: Option<String>,
    /// The last N commits of the branch instead
    #[arg(short = 'n', long = "count", value_name = "N")]
    pub count: Option<usize>,
    /// Include root commits (all history up to the branch tip)
    #[arg(long)]
    pub root: bool,
    /// Directory for the patch files (default: the current directory)
    #[arg(short = 'o', long = "output-directory", value_name = "DIR")]
    pub output: Option<PathBuf>,
    /// Write all patches to standard output as one mailbox
    #[arg(long)]
    pub stdout: bool,
    /// Number the first patch from N (default 1)
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub start_number: usize,
    /// Never put `n/m` in the subject
    #[arg(long)]
    pub no_numbered: bool,
    /// Subject prefix inside the brackets (default PATCH)
    #[arg(long, value_name = "TEXT", default_value = "PATCH")]
    pub subject_prefix: String,
    /// Zone (default: the zone containing the current directory)
    #[arg(long)]
    pub zone: Option<String>,
}

#[derive(Args, Clone)]
pub struct ApplyArgs {
    /// Patch files (default: standard input; `-` is standard input)
    pub files: Vec<PathBuf>,
    /// Check that the patch applies; change nothing
    #[arg(long)]
    pub check: bool,
    /// Also stage the changes (the files must match the index)
    #[arg(long)]
    pub index: bool,
    /// Apply the patch backwards
    #[arg(short = 'R', long)]
    pub reverse: bool,
    /// Strip N leading path components (default 1)
    #[arg(short = 'p', value_name = "N")]
    pub strip: Option<usize>,
    /// Three-way application is not supported (refused)
    #[arg(short = '3', long = "3way")]
    pub three_way: bool,
    /// Print the files that were (or would be) changed
    #[arg(short = 'v', long)]
    pub verbose: bool,
}

#[derive(Args, Clone)]
pub struct AmArgs {
    /// Mailbox files, such as those `format-patch` wrote (default: standard input)
    pub files: Vec<PathBuf>,
    /// Commit the files you fixed and staged for the stopped patch, then go on
    #[arg(long = "continue")]
    pub cont: bool,
    /// Drop the stopped patch and go on
    #[arg(long)]
    pub skip: bool,
    /// Go back to the branch heads from before `vcs am`
    #[arg(long)]
    pub abort: bool,
    /// Three-way application is not supported (refused)
    #[arg(short = '3', long = "3way")]
    pub three_way: bool,
}

fn read_input(files: &[PathBuf]) -> Result<Vec<Vec<u8>>, Error> {
    let mut out = Vec::new();
    if files.is_empty() {
        out.push(read_stdin()?);
    }
    for f in files {
        if f == Path::new("-") {
            out.push(read_stdin()?);
        } else {
            out.push(std::fs::read(f).map_err(|source| Error::Io { path: f.clone(), source })?);
        }
    }
    Ok(out)
}

fn read_stdin() -> Result<Vec<u8>, Error> {
    let mut v = Vec::new();
    std::io::stdin().read_to_end(&mut v).map_err(|source| Error::Io {
        path: "<stdin>".into(),
        source,
    })?;
    Ok(v)
}

pub fn format_patch(repo: &Repo, cwd: &Path, a: FormatPatchArgs) -> Result<(), Error> {
    let zone = crate::zone_of_cwd(repo, cwd, a.zone)?;
    if a.rev.is_none() && a.count.is_none() && !a.root {
        return err("give what to format: `-n N` (last N commits), `<rev>` (commits not in it), `A..B`, or `--root`");
    }
    let (since, head) = match a.rev.as_deref() {
        Some(r) if r.contains("...") => return err("`A...B` is not supported; use `vcs merge-base`, then `A..B`"),
        Some(r) => match r.split_once("..") {
            Some((x, y)) => (
                Some(if x.is_empty() { "HEAD" } else { x }),
                Some(if y.is_empty() { "HEAD" } else { y }),
            ),
            None => (Some(r), None),
        },
        None => (None, None),
    };
    let ids = repo.patch_range(&zone, since, head, a.count, a.root)?;
    if ids.is_empty() {
        return err("no commits to format");
    }
    let files = repo.format_patches(&zone, &ids, !a.no_numbered, a.start_number, &a.subject_prefix)?;
    if a.stdout {
        use std::io::Write;
        let mut so = std::io::stdout().lock();
        for (_, text) in &files {
            so.write_all(text).map_err(|source| Error::Io {
                path: "<stdout>".into(),
                source,
            })?;
        }
        return Ok(());
    }
    let dir = a.output.unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir).map_err(|source| Error::Io { path: dir.clone(), source })?;
    // refuse to overwrite before writing anything
    for (name, _) in &files {
        let p = dir.join(name);
        if p.exists() {
            return err(format!(
                "{} already exists; remove it or pick another directory with -o",
                p.display()
            ));
        }
    }
    for (name, text) in &files {
        let p = dir.join(name);
        std::fs::write(&p, text).map_err(|source| Error::Io { path: p.clone(), source })?;
        println!("{}", p.display());
    }
    Ok(())
}

pub fn apply(repo: &Repo, a: ApplyArgs) -> Result<(), Error> {
    if a.three_way {
        return err("apply -3/--3way is not supported: a patch carries no blob ids to merge against. Apply it with `vcs apply`, or fetch the commits (`vcs fetch`, `vcs bundle`) and use `vcs merge`/`vcs cherry-pick`");
    }
    let opts = ApplyOpts {
        check: a.check,
        index: a.index,
        reverse: a.reverse,
        strip: a.strip,
    };
    let mut total = 0;
    for data in read_input(&a.files)? {
        let rep = repo.apply_patch(&data, &opts)?;
        if a.verbose {
            for (p, k) in &rep.files {
                let k = match k {
                    ApplyKind::Created => "create",
                    ApplyKind::Modified => "modify",
                    ApplyKind::Deleted => "delete",
                };
                println!("{k} {p}");
            }
        }
        total += rep.files.len();
    }
    if a.verbose {
        println!("{} file(s) {}", total, if a.check { "would apply" } else { "applied" });
    }
    Ok(())
}

fn show_am(out: AmOutcome) -> Result<(), Error> {
    let (applied, stop) = match out {
        AmOutcome::Done { applied } => (applied, None),
        AmOutcome::Stopped {
            applied,
            number,
            total,
            subject,
            reason,
        } => (applied, Some((number, total, subject, reason))),
    };
    for (s, id) in &applied {
        println!("Applying: {s}  ({})", id.short());
    }
    if let Some((n, t, subject, reason)) = stop {
        eprintln!("patch {n}/{t} ({subject}) did not apply: {reason}");
        eprintln!("The working tree is as before this patch. Make the change by hand, `vcs add` it, then `vcs am --continue`;");
        eprintln!("or `vcs am --skip` to drop this patch, or `vcs am --abort` to go back.");
        return Err(Error::Msg("am stopped".into()));
    }
    Ok(())
}

pub fn am(repo: &Repo, a: AmArgs) -> Result<(), Error> {
    if a.three_way {
        return err("am -3/--3way is not supported (see `vcs apply --help`)");
    }
    if a.abort {
        repo.am_abort()?;
        println!("am aborted; branches and working tree restored");
        return Ok(());
    }
    if a.cont {
        return show_am(repo.am_continue()?);
    }
    if a.skip {
        return show_am(repo.am_skip()?);
    }
    let inputs = read_input(&a.files)?;
    show_am(repo.am_start(&inputs)?)
}
