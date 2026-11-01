//! `.vcsignore`: gitignore-style patterns for the untracked walk, `status` and `add`.
//!
//! Rules (per ignore file, relative to the directory that holds it):
//! - blank lines and `#` comments are skipped; `\#` and `\!` escape a leading `#` / `!`;
//!   trailing spaces are dropped unless escaped with `\`.
//! - `!pattern` re-includes what an earlier pattern ignored; the last matching pattern wins,
//!   and patterns from deeper ignore files come after (so override) shallower ones.
//! - a trailing `/` matches directories only.
//! - a pattern with a `/` at its start or in its middle is anchored to the ignore file's
//!   directory; otherwise it matches a name at any depth below it.
//! - `*`, `?`, `[a-z]` / `[!a-z]` never cross `/`; a `**` path segment matches any number of
//!   directories (`a/**/b`, `**/b`, and `a/**` for everything inside `a`).
//! - a file below an ignored directory is ignored and cannot be re-included.
//!
//! Tracked files are never ignored, and the `.vcs` directory is never walked at all; both are
//! enforced by the callers (this module only answers "do the patterns match").

use crate::error::{Error, Result};
use crate::worktree_fs::{FileKind, WorktreeFs};
use std::path::Path;
use std::sync::Arc;

/// Name of the ignore file looked up in every directory.
pub const IGNORE_FILE: &str = ".vcsignore";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Lit(char),
    Any,
    Star,
    Class(bool, Vec<(char, char)>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Seg {
    DoubleStar,
    Glob(Vec<Tok>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    negate: bool,
    dir_only: bool,
    anchored: bool,
    segs: Vec<Seg>,
}

fn parse_seg(s: &str) -> Seg {
    if s == "**" {
        return Seg::DoubleStar;
    }
    let cs: Vec<char> = s.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '\\' if i + 1 < cs.len() => {
                toks.push(Tok::Lit(cs[i + 1]));
                i += 2;
                continue;
            }
            '*' => {
                if toks.last() != Some(&Tok::Star) {
                    toks.push(Tok::Star);
                }
            }
            '?' => toks.push(Tok::Any),
            '[' => {
                // find the closing ']' (a ']' right after '[' or '[!' is literal)
                let mut j = i + 1;
                let neg = matches!(cs.get(j), Some('!') | Some('^'));
                if neg {
                    j += 1;
                }
                let start = j;
                let mut close = None;
                while j < cs.len() {
                    if cs[j] == ']' && j > start {
                        close = Some(j);
                        break;
                    }
                    j += 1;
                }
                match close {
                    Some(c) => {
                        let body = &cs[start..c];
                        let mut ranges = Vec::new();
                        let mut k = 0;
                        while k < body.len() {
                            if k + 2 < body.len() && body[k + 1] == '-' {
                                ranges.push((body[k], body[k + 2]));
                                k += 3;
                            } else {
                                ranges.push((body[k], body[k]));
                                k += 1;
                            }
                        }
                        toks.push(Tok::Class(neg, ranges));
                        i = c + 1;
                        continue;
                    }
                    None => toks.push(Tok::Lit('[')),
                }
            }
            c => toks.push(Tok::Lit(c)),
        }
        i += 1;
    }
    Seg::Glob(toks)
}

fn glob_match(toks: &[Tok], s: &[char]) -> bool {
    match toks.split_first() {
        None => s.is_empty(),
        Some((Tok::Star, rest)) => (0..=s.len()).any(|k| glob_match(rest, &s[k..])),
        Some((t, rest)) => match s.split_first() {
            None => false,
            Some((c, tail)) => {
                let ok = match t {
                    Tok::Lit(l) => l == c,
                    Tok::Any => true,
                    Tok::Class(neg, rs) => rs.iter().any(|(a, b)| a <= c && c <= b) != *neg,
                    Tok::Star => unreachable!(),
                };
                ok && glob_match(rest, tail)
            }
        },
    }
}

fn segs_match(segs: &[Seg], path: &[&str]) -> bool {
    match segs.split_first() {
        None => path.is_empty(),
        Some((Seg::DoubleStar, [])) => !path.is_empty(), // trailing `/**`: everything inside
        Some((Seg::DoubleStar, rest)) => (0..=path.len()).any(|k| segs_match(rest, &path[k..])),
        Some((Seg::Glob(g), rest)) => match path.split_first() {
            None => false,
            Some((p, tail)) => {
                let cs: Vec<char> = p.chars().collect();
                glob_match(g, &cs) && segs_match(rest, tail)
            }
        },
    }
}

impl Pattern {
    /// Parse one line; `None` for blank lines and comments.
    pub fn parse(line: &str) -> Option<Pattern> {
        let mut l = line.trim_end_matches(['\r', '\n']);
        // trailing spaces are dropped unless escaped
        while l.ends_with(' ') && !l.ends_with("\\ ") {
            l = &l[..l.len() - 1];
        }
        let l = l.replace("\\ ", " ");
        if l.is_empty() || l.starts_with('#') {
            return None;
        }
        let mut s = l.as_str();
        let negate = s.starts_with('!');
        if negate {
            s = &s[1..];
        }
        if s.starts_with("\\#") || s.starts_with("\\!") {
            s = &s[1..];
        }
        let dir_only = s.ends_with('/');
        let s = s.trim_end_matches('/');
        if s.is_empty() {
            return None;
        }
        let anchored = s.contains('/');
        let s = s.strip_prefix('/').unwrap_or(s);
        let mut segs: Vec<Seg> = s.split('/').filter(|x| !x.is_empty()).map(parse_seg).collect();
        if segs.is_empty() {
            return None;
        }
        if !anchored && segs == [Seg::DoubleStar] {
            segs = vec![Seg::Glob(vec![Tok::Star])];
        }
        Some(Pattern {
            negate,
            dir_only,
            anchored,
            segs,
        })
    }

    /// Does the pattern match `rel` (slash-separated, relative to the ignore file's directory)?
    pub fn matches(&self, rel: &str, is_dir: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }
        let parts: Vec<&str> = rel.split('/').collect();
        if self.anchored {
            segs_match(&self.segs, &parts)
        } else {
            segs_match(&self.segs, &parts[parts.len() - 1..])
        }
    }
}

/// The parsed patterns of one `.vcsignore`, with the directory (repo-relative) that holds it.
#[derive(Debug, Default)]
pub struct IgnoreFile {
    base: String,
    pats: Vec<Pattern>,
}

impl IgnoreFile {
    pub fn parse(base: &str, text: &str) -> IgnoreFile {
        IgnoreFile {
            base: base.to_owned(),
            pats: text.lines().filter_map(Pattern::parse).collect(),
        }
    }
}

/// The ignore files that apply to a directory: those of it and of every ancestor.
#[derive(Clone, Debug, Default)]
pub struct IgnoreStack {
    files: Vec<Arc<IgnoreFile>>,
    /// Inside an ignored directory (kept only because something below it is tracked):
    /// every path is ignored.
    inherited: bool,
}

impl IgnoreStack {
    /// The stack for directory `dir` (repo-relative, "" = root) given its parent's stack.
    pub fn descend(&self, wfs: &dyn WorktreeFs, worktree: &Path, dir: &str) -> Result<IgnoreStack> {
        let rel = if dir.is_empty() {
            IGNORE_FILE.to_owned()
        } else {
            format!("{dir}/{IGNORE_FILE}")
        };
        let text = match wfs.read(&rel) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(self.clone()),
            // a directory (or unreadable path) named .vcsignore is simply not an ignore file
            Err(e) if wfs.stat(&rel).is_ok_and(|m| m.kind == FileKind::Dir) || e.kind() == std::io::ErrorKind::PermissionDenied => {
                return Ok(self.clone())
            }
            Err(source) => {
                return Err(Error::Io {
                    path: worktree.join(&rel),
                    source,
                })
            }
        };
        let mut s = self.clone();
        s.files.push(Arc::new(IgnoreFile::parse(dir, &text)));
        Ok(s)
    }

    /// Same stack, but everything below is ignored.
    pub fn inherit(&self) -> IgnoreStack {
        IgnoreStack {
            files: self.files.clone(),
            inherited: true,
        }
    }

    /// Is the entry `rel` (repo-relative) ignored? Only `rel` itself is tested against the
    /// patterns; callers test each directory on the way down.
    pub fn is_ignored(&self, rel: &str, is_dir: bool) -> bool {
        if self.inherited {
            return true;
        }
        let mut res = false;
        for f in &self.files {
            let sub = if f.base.is_empty() {
                rel
            } else {
                match rel.strip_prefix(f.base.as_str()).and_then(|r| r.strip_prefix('/')) {
                    Some(r) => r,
                    None => continue,
                }
            };
            for p in &f.pats {
                if p.matches(sub, is_dir) {
                    res = !p.negate;
                }
            }
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(files: &[(&str, &str)]) -> IgnoreStack {
        IgnoreStack {
            files: files.iter().map(|(b, t)| Arc::new(IgnoreFile::parse(b, t))).collect(),
            inherited: false,
        }
    }

    /// (ignore files, path, is_dir, expected)
    #[test]
    fn pattern_table() {
        type Row = (&'static [(&'static str, &'static str)], &'static str, bool, bool);
        let root = |t: &'static str| -> &'static [(&'static str, &'static str)] { Box::leak(Box::new([("", t)])) };
        let table: Vec<Row> = vec![
            // plain names match at any depth
            (root("target"), "target", true, true),
            (root("target"), "a/b/target", false, true),
            (root("target"), "targets", false, false),
            // globs do not cross slashes
            (root("*.o"), "x.o", false, true),
            (root("*.o"), "a/b/x.o", false, true),
            (root("*.o"), "x.c", false, false),
            (root("a?c"), "abc", false, true),
            (root("a?c"), "a/c", false, false),
            (root("[a-c]x"), "bx", false, true),
            (root("[a-c]x"), "dx", false, false),
            (root("[!a-c]x"), "dx", false, true),
            (root("[!a-c]x"), "bx", false, false),
            // directory-only
            (root("build/"), "build", true, true),
            (root("build/"), "build", false, false),
            // anchoring
            (root("/top"), "top", false, true),
            (root("/top"), "sub/top", false, false),
            (root("doc/gen"), "doc/gen", false, true),
            (root("doc/gen"), "x/doc/gen", false, false),
            // double star
            (root("**/cache"), "cache", true, true),
            (root("**/cache"), "a/b/cache", true, true),
            (root("a/**/z"), "a/z", false, true),
            (root("a/**/z"), "a/b/c/z", false, true),
            (root("a/**/z"), "b/a/z", false, false),
            (root("a/**"), "a/x", false, true),
            (root("a/**"), "a", true, false),
            // negation: last match wins
            (root("*.log\n!keep.log"), "keep.log", false, false),
            (root("*.log\n!keep.log"), "other.log", false, true),
            (root("!keep.log\n*.log"), "keep.log", false, true),
            // escapes, comments, blanks, trailing spaces
            (root("# comment\n\n\\#hash\n\\!bang"), "#hash", false, true),
            (root("# comment\n\n\\#hash\n\\!bang"), "!bang", false, true),
            (root("# comment"), "# comment", false, false),
            (root("name   "), "name", false, true),
            (root("name\\ "), "name ", false, true),
            // nested ignore files: paths are relative to their own directory
            (&[("", "*.tmp"), ("sub", "!keep.tmp\n/only")], "sub/keep.tmp", false, false),
            (&[("", "*.tmp"), ("sub", "!keep.tmp\n/only")], "keep.tmp", false, true),
            (&[("", "*.tmp"), ("sub", "!keep.tmp\n/only")], "sub/only", false, true),
            (&[("", "*.tmp"), ("sub", "!keep.tmp\n/only")], "sub/deep/only", false, false),
            (&[("", "*.tmp"), ("sub", "!keep.tmp\n/only")], "only", false, false),
            // a nested file does not apply outside its directory
            (&[("sub", "x")], "other/x", false, false),
            (&[("sub", "x")], "sub/x", false, true),
        ];
        for (files, path, is_dir, want) in table {
            assert_eq!(
                stack(files).is_ignored(path, is_dir),
                want,
                "files={files:?} path={path:?} dir={is_dir}"
            );
        }
    }

    #[test]
    fn inherited_ignores_everything() {
        let s = stack(&[("", "!keep")]).inherit();
        assert!(s.is_ignored("keep", false));
    }

    #[test]
    fn lone_double_star_matches_all() {
        assert!(stack(&[("", "**")]).is_ignored("a", false));
    }
}
