//! `vcs grep [-n -i -w -l -c -F] <pattern> [rev] [-- paths]`: search tracked files.
//!
//! Without `rev` the files are the tracked files of the working tree, read from disk (so
//! unstaged edits are searched; files deleted on disk are skipped). With `rev` they are the
//! files of that revision in every subscribed zone where it resolves, read from the object
//! store (chunked files are reassembled with `get_content`). Only tracked files are searched,
//! which means ignored (`.vcsignore`) files and zones that are not subscribed never appear.
//! Binary files (a NUL in the first 8000 bytes) and symlinks are skipped. Files are searched
//! on all cores; output is in path order.

use super::open;
use clap::Args;
use regex::bytes::{Regex, RegexBuilder};
use std::io::Write;
use vcs_objects::{EntryMode, ObjectId};
use vcs_repo::{Error, Repo};

#[derive(Args, Clone)]
pub struct GrepArgs {
    /// Regular expression (Rust regex syntax); use -F for a literal string
    pub pattern: String,
    /// Revision to search instead of the working tree
    pub rev: Option<String>,
    /// Prefix each line with its line number
    #[arg(short = 'n', long)]
    pub line_number: bool,
    /// Ignore case
    #[arg(short = 'i', long)]
    pub ignore_case: bool,
    /// Match whole words only
    #[arg(short = 'w', long)]
    pub word: bool,
    /// Print only the names of files with a match
    #[arg(short = 'l', long)]
    pub files_with_matches: bool,
    /// Print only the number of matching lines per file
    #[arg(short = 'c', long)]
    pub count: bool,
    /// Treat the pattern as a literal string
    #[arg(short = 'F', long)]
    pub fixed: bool,
    /// Limit the search to these paths (after `--`)
    #[arg(last = true)]
    pub paths: Vec<String>,
}

struct Hit {
    path: String,
    /// (1-based line number, line bytes)
    lines: Vec<(usize, Vec<u8>)>,
}

enum Src {
    Disk(std::path::PathBuf),
    Obj(ObjectId),
}

fn build(a: &GrepArgs) -> Result<Regex, Error> {
    let mut p = if a.fixed { regex::escape(&a.pattern) } else { a.pattern.clone() };
    if a.word {
        p = format!(r"\b(?:{p})\b");
    }
    RegexBuilder::new(&p)
        .case_insensitive(a.ignore_case)
        .build()
        .map_err(|e| Error::Msg(format!("bad pattern: {e}")))
}

fn search(re: &Regex, path: &str, data: &[u8]) -> Option<Hit> {
    if data[..data.len().min(8000)].contains(&0) {
        return None;
    }
    let mut lines = Vec::new();
    for (i, l) in data.split(|b| *b == b'\n').enumerate() {
        if re.is_match(l) {
            lines.push((i + 1, l.to_vec()));
        }
    }
    (!lines.is_empty()).then(|| Hit {
        path: path.to_owned(),
        lines,
    })
}

fn wanted(path: &str, filters: &[String]) -> bool {
    filters.is_empty()
        || filters
            .iter()
            .any(|f| f.is_empty() || path == f || path.strip_prefix(f.as_str()).is_some_and(|r| r.starts_with('/')))
}

pub fn run(a: &GrepArgs) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let re = build(a)?;
    let filters: Vec<String> = a.paths.iter().map(|p| repo.rel_path(&cwd, p)).collect::<Result<_, _>>()?;
    let mut files: Vec<(String, Src)> = Vec::new();
    match &a.rev {
        None => {
            for (p, mode, id) in repo.tracked_files()? {
                if mode == EntryMode::Symlink || !wanted(&p, &filters) {
                    continue;
                }
                let _ = id;
                let abs = repo.worktree().join(&p);
                files.push((p, Src::Disk(abs)));
            }
        }
        Some(rev) => {
            for (p, (mode, id)) in repo.rev_files(rev)? {
                if mode == EntryMode::Symlink || !wanted(&p, &filters) {
                    continue;
                }
                files.push((p, Src::Obj(id)));
            }
        }
    }
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(32);
    let per = files.len().div_ceil(threads).max(1);
    let repo_ref: &Repo = &repo;
    let re_ref = &re;
    let results: Vec<Result<Vec<Hit>, Error>> = std::thread::scope(|s| {
        let handles: Vec<_> = files
            .chunks(per)
            .map(|chunk| {
                s.spawn(move || -> Result<Vec<Hit>, Error> {
                    let mut out = Vec::new();
                    for (p, src) in chunk {
                        let data = match src {
                            Src::Disk(abs) => match std::fs::read(abs) {
                                Ok(d) => d,
                                Err(_) => continue, // deleted or unreadable on disk
                            },
                            Src::Obj(id) => repo_ref.content(id)?,
                        };
                        out.extend(search(re_ref, p, &data));
                    }
                    Ok(out)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("grep worker panicked")).collect()
    });
    let stdout = std::io::stdout();
    let mut o = std::io::BufWriter::new(stdout.lock());
    let mut any = false;
    for r in results {
        for h in r? {
            any = true;
            let w = |e: std::io::Error| Error::Io {
                path: "stdout".into(),
                source: e,
            };
            if a.files_with_matches {
                writeln!(o, "{}", h.path).map_err(w)?;
            } else if a.count {
                writeln!(o, "{}:{}", h.path, h.lines.len()).map_err(w)?;
            } else {
                for (n, l) in &h.lines {
                    if a.line_number {
                        write!(o, "{}:{n}:", h.path).map_err(w)?;
                    } else {
                        write!(o, "{}:", h.path).map_err(w)?;
                    }
                    o.write_all(l).map_err(w)?;
                    o.write_all(b"\n").map_err(w)?;
                }
            }
        }
    }
    let _ = o.flush();
    drop(o);
    if !any {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_and_binary() {
        assert!(wanted("a/b.txt", &["a".into()]));
        assert!(!wanted("ab/b.txt", &["a".into()]));
        assert!(wanted("x", &[]));
        let re = Regex::new("o").unwrap();
        assert!(search(&re, "p", b"foo\0bar").is_none());
        let h = search(&re, "p", b"a\nfoo\nb").unwrap();
        assert_eq!(h.lines, vec![(2, b"foo".to_vec())]);
    }
}
