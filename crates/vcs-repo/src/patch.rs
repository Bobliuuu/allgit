//! Unified-diff patches: parse (git and `vcs diff` dialects), apply hunks to bytes, reverse.
//!
//! Pure functions, no repository access. The parser accepts what `git diff`, `git format-patch`,
//! `vcs diff` and `vcs show` print: `diff --git|--vcs a/x b/x` headers with optional mode, rename,
//! copy and `new file`/`deleted file` lines, plain `--- a/x` / `+++ b/x` pairs, hunks with
//! `\ No newline at end of file`. Binary patches (`GIT binary patch`, `Binary files ... differ`)
//! are recognised and flagged, never interpreted: callers must refuse them.
//!
//! Hunks are applied exactly: every context and removed line must match, at the recorded
//! position or the nearest position (searched outward) where they all do. There is no fuzz and
//! no whitespace tolerance, so a patch either applies as written or is rejected whole.

use crate::error::{msg, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HunkLine {
    Ctx(Vec<u8>),
    Del(Vec<u8>),
    Add(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    pub lines: Vec<HunkLine>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilePatch {
    /// Path before the change (`None`: the file is created).
    pub old_path: Option<String>,
    /// Path after the change (`None`: the file is removed).
    pub new_path: Option<String>,
    /// Octal modes as in git (100644, 100755, 120000).
    pub old_mode: Option<u32>,
    pub new_mode: Option<u32>,
    pub copy: bool,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

impl FilePatch {
    /// The patch that undoes this one.
    pub fn reversed(&self) -> FilePatch {
        FilePatch {
            old_path: self.new_path.clone(),
            new_path: self.old_path.clone(),
            old_mode: self.new_mode,
            new_mode: self.old_mode,
            copy: false,
            binary: self.binary,
            hunks: self
                .hunks
                .iter()
                .map(|h| Hunk {
                    old_start: h.new_start,
                    old_len: h.new_len,
                    new_start: h.old_start,
                    new_len: h.old_len,
                    lines: h
                        .lines
                        .iter()
                        .map(|l| match l {
                            HunkLine::Ctx(x) => HunkLine::Ctx(x.clone()),
                            HunkLine::Del(x) => HunkLine::Add(x.clone()),
                            HunkLine::Add(x) => HunkLine::Del(x.clone()),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    /// Path to show in messages.
    pub fn display_path(&self) -> &str {
        self.new_path.as_deref().or(self.old_path.as_deref()).unwrap_or("?")
    }
}

fn lines_of(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut s = 0;
    for (i, b) in data.iter().enumerate() {
        if *b == b'\n' {
            out.push(&data[s..=i]);
            s = i + 1;
        }
    }
    if s < data.len() {
        out.push(&data[s..]);
    }
    out
}

fn trim_eol(l: &[u8]) -> &[u8] {
    let l = l.strip_suffix(b"\n").unwrap_or(l);
    l.strip_suffix(b"\r").unwrap_or(l)
}

/// Remove git's C-style quoting (`"a/\303\244"`) if present.
fn unquote(s: &[u8]) -> Vec<u8> {
    if s.len() >= 2 && s[0] == b'"' && s[s.len() - 1] == b'"' {
        let inner = &s[1..s.len() - 1];
        let mut out = Vec::new();
        let mut i = 0;
        while i < inner.len() {
            if inner[i] == b'\\' && i + 1 < inner.len() {
                i += 1;
                match inner[i] {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'v' => out.push(11),
                    c @ b'0'..=b'7' => {
                        let mut v = u32::from(c - b'0');
                        let mut n = 1;
                        while n < 3 && i + 1 < inner.len() && (b'0'..=b'7').contains(&inner[i + 1]) {
                            i += 1;
                            v = v * 8 + u32::from(inner[i] - b'0');
                            n += 1;
                        }
                        out.push(v as u8);
                    }
                    c => out.push(c),
                }
            } else {
                out.push(inner[i]);
            }
            i += 1;
        }
        out
    } else {
        s.to_vec()
    }
}

fn strip_components(p: &str, n: usize) -> Option<String> {
    let mut rest = p;
    for _ in 0..n {
        let (_, r) = rest.split_once('/')?;
        rest = r;
    }
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_owned())
    }
}

/// Path from a `---`/`+++` line (after the 4 characters): quoting and a trailing tab+timestamp removed.
fn header_path(raw: &[u8], strip: usize) -> Result<Option<String>> {
    let raw = trim_eol(raw);
    let raw = match raw.iter().position(|&b| b == b'\t') {
        Some(t) if raw.first() != Some(&b'"') => &raw[..t],
        _ => raw,
    };
    let raw = unquote(raw);
    let s = String::from_utf8_lossy(&raw).into_owned();
    if s == "/dev/null" {
        return Ok(None);
    }
    match strip_components(&s, strip) {
        Some(p) => Ok(Some(p)),
        None => msg(format!("cannot strip {strip} leading component(s) from {s:?}")),
    }
}

/// Parse the git-style `diff --git a/x b/y` line into (old, new) when both halves can be told apart.
fn diff_line_paths(rest: &[u8], strip: usize) -> Option<(String, String)> {
    let rest = trim_eol(rest);
    // quoted halves
    if rest.first() == Some(&b'"') {
        let mut i = 1;
        while i < rest.len() && !(rest[i] == b'"' && rest[i - 1] != b'\\') {
            i += 1;
        }
        let a = unquote(&rest[..=i.min(rest.len() - 1)]);
        let b = unquote(rest.get(i + 2..)?);
        return Some((
            strip_components(&String::from_utf8_lossy(&a), strip)?,
            strip_components(&String::from_utf8_lossy(&b), strip)?,
        ));
    }
    // plain: the two halves have the same text after stripping when the path is not renamed
    for (i, _) in rest.iter().enumerate().filter(|(_, &c)| c == b' ') {
        let (l, r) = (&rest[..i], &rest[i + 1..]);
        let (l, r) = (unquote(l), unquote(r));
        let (ls, rs) = (String::from_utf8_lossy(&l).into_owned(), String::from_utf8_lossy(&r).into_owned());
        if let (Some(a), Some(b)) = (strip_components(&ls, strip), strip_components(&rs, strip)) {
            if a == b {
                return Some((a, b));
            }
        }
    }
    // different halves (rename): split at the middle " b/" or " a/"
    let s = String::from_utf8_lossy(rest).into_owned();
    let i = s.find(" b/").or_else(|| s.find(" a/"))?;
    Some((strip_components(&s[..i], strip)?, strip_components(&s[i + 1..], strip)?))
}

fn parse_mode(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim(), 8).ok()
}

fn parse_range(tok: &str, sign: char) -> Option<(usize, usize)> {
    let t = tok.strip_prefix(sign)?;
    match t.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((t.parse().ok()?, 1)),
    }
}

/// Parse `data` (possibly with leading commit text, as `vcs show` or a mail prints it) into file
/// patches. `strip` leading path components are removed (git's `-p`, default 1).
pub fn parse(data: &[u8], strip: usize) -> Result<Vec<FilePatch>> {
    let lines = lines_of(data);
    let mut out: Vec<FilePatch> = Vec::new();
    let mut cur: Option<FilePatch> = None;
    // header facts gathered before the ---/+++ pair
    let mut is_new = false;
    let mut is_del = false;
    let mut hdr_old: Option<String> = None;
    let mut hdr_new: Option<String> = None;
    let mut i = 0;
    // Close the open patch; header-only ones (mode change, rename, empty file, binary) take their
    // names from the `diff` line.
    fn finish(cur: &mut Option<FilePatch>, out: &mut Vec<FilePatch>, hdr: (&Option<String>, &Option<String>, bool, bool)) {
        if let Some(mut p) = cur.take() {
            if p.old_path.is_none() && p.new_path.is_none() {
                p.old_path = if hdr.2 { None } else { hdr.0.clone() };
                p.new_path = if hdr.3 { None } else { hdr.1.clone() };
            }
            out.push(p);
        }
    }
    while i < lines.len() {
        let l = lines[i];
        if l.starts_with(b"diff --cc ") || l.starts_with(b"diff --combined ") {
            return msg("combined diffs (merge commits) cannot be applied");
        }
        if l.starts_with(b"diff --git ") || l.starts_with(b"diff --vcs ") {
            finish(&mut cur, &mut out, (&hdr_old, &hdr_new, is_new, is_del));
            is_new = false;
            is_del = false;
            let rest = &l[l.iter().position(|&b| b == b' ').unwrap() + 1..];
            let rest = &rest[rest.iter().position(|&b| b == b' ').map(|p| p + 1).unwrap_or(0)..];
            let (a, b) = diff_line_paths(rest, strip).unwrap_or_default();
            hdr_old = Some(a).filter(|s| !s.is_empty());
            hdr_new = Some(b).filter(|s| !s.is_empty());
            cur = Some(FilePatch::default());
            i += 1;
            continue;
        }
        if let Some(p) = cur.as_mut() {
            let t = String::from_utf8_lossy(trim_eol(l)).into_owned();
            if let Some(m) = t.strip_prefix("old mode ") {
                p.old_mode = parse_mode(m);
            } else if let Some(m) = t.strip_prefix("new mode ") {
                p.new_mode = parse_mode(m);
            } else if let Some(m) = t.strip_prefix("new file mode ") {
                is_new = true;
                p.new_mode = parse_mode(m);
            } else if t == "new file" {
                is_new = true;
            } else if let Some(m) = t.strip_prefix("deleted file mode ") {
                is_del = true;
                p.old_mode = parse_mode(m);
            } else if t == "deleted file" {
                is_del = true;
            } else if let Some(f) = t.strip_prefix("rename from ").or_else(|| t.strip_prefix("copy from ")) {
                p.copy = t.starts_with("copy");
                hdr_old = strip_components_raw(f, 0);
            } else if let Some(f) = t.strip_prefix("rename to ").or_else(|| t.strip_prefix("copy to ")) {
                hdr_new = strip_components_raw(f, 0);
            } else if t.starts_with("GIT binary patch") || (t.starts_with("Binary files ") && t.ends_with(" differ")) {
                p.binary = true;
            }
        }
        if l.starts_with(b"--- ") && lines.get(i + 1).is_some_and(|n| n.starts_with(b"+++ ")) {
            // a file header; starts a new patch unless the current one has none yet
            if cur
                .as_ref()
                .is_some_and(|p| !p.hunks.is_empty() || p.old_path.is_some() || p.new_path.is_some())
            {
                finish(&mut cur, &mut out, (&hdr_old, &hdr_new, is_new, is_del));
                hdr_old = None;
                hdr_new = None;
                is_new = false;
                is_del = false;
            }
            let p = cur.get_or_insert_with(FilePatch::default);
            let mut old = header_path(&l[4..], strip)?;
            let mut new = header_path(&lines[i + 1][4..], strip)?;
            if is_new {
                old = None;
            }
            if is_del {
                new = None;
            }
            if old.is_none() && new.is_none() {
                return msg("patch header names no file (both sides /dev/null)");
            }
            // prefer the header names for renames (they are unprefixed in --- / +++ too, but exact)
            if let (Some(h), Some(_)) = (&hdr_old, &old) {
                old = Some(h.clone());
            }
            if let (Some(h), Some(_)) = (&hdr_new, &new) {
                new = Some(h.clone());
            }
            p.old_path = old;
            p.new_path = new;
            i += 2;
            continue;
        }
        if l.starts_with(b"@@ ") {
            let Some(p) = cur.as_mut() else {
                return msg(format!("patch line {}: hunk before any file header", i + 1));
            };
            if p.old_path.is_none() && p.new_path.is_none() {
                // header-only dialect (no ---/+++): names come from the diff line
                p.old_path = if is_new { None } else { hdr_old.clone() };
                p.new_path = if is_del { None } else { hdr_new.clone() };
            }
            let t = String::from_utf8_lossy(trim_eol(l)).into_owned();
            let mut it = t[3..].split_whitespace();
            let (Some(a), Some(b)) = (it.next(), it.next()) else {
                return msg(format!("patch line {}: bad hunk header", i + 1));
            };
            let (Some((os, ol)), Some((ns, nl))) = (parse_range(a, '-'), parse_range(b, '+')) else {
                return msg(format!("patch line {}: bad hunk header {t:?}", i + 1));
            };
            let mut h = Hunk {
                old_start: os,
                old_len: ol,
                new_start: ns,
                new_len: nl,
                lines: Vec::new(),
            };
            let (mut o, mut n) = (0, 0);
            i += 1;
            while o < ol || n < nl {
                let Some(&bl) = lines.get(i) else {
                    return msg("patch is truncated in the middle of a hunk");
                };
                match bl.first() {
                    Some(b' ') => {
                        h.lines.push(HunkLine::Ctx(bl[1..].to_vec()));
                        o += 1;
                        n += 1;
                    }
                    Some(b'-') => {
                        h.lines.push(HunkLine::Del(bl[1..].to_vec()));
                        o += 1;
                    }
                    Some(b'+') => {
                        h.lines.push(HunkLine::Add(bl[1..].to_vec()));
                        n += 1;
                    }
                    Some(b'\n') | Some(b'\r') => {
                        // a context line whose single space was stripped (mail clients do this)
                        h.lines.push(HunkLine::Ctx(bl.to_vec()));
                        o += 1;
                        n += 1;
                    }
                    Some(b'\\') => {
                        no_newline(&mut h);
                        i += 1;
                        continue;
                    }
                    _ => {
                        return msg(format!(
                            "corrupt patch at line {}: {:?}",
                            i + 1,
                            String::from_utf8_lossy(trim_eol(bl))
                        ))
                    }
                }
                i += 1;
            }
            while lines.get(i).is_some_and(|x| x.starts_with(b"\\")) {
                no_newline(&mut h);
                i += 1;
            }
            p.hunks.push(h);
            continue;
        }
        i += 1;
    }
    finish(&mut cur, &mut out, (&hdr_old, &hdr_new, is_new, is_del));
    for p in &mut out {
        if p.old_path.is_none() && p.new_path.is_none() {
            return msg("patch names no file");
        }
    }
    Ok(out)
}

fn strip_components_raw(f: &str, n: usize) -> Option<String> {
    let raw = unquote(f.as_bytes());
    strip_components(&String::from_utf8_lossy(&raw), n)
}

/// `\ No newline at end of file` applies to the previous line.
fn no_newline(h: &mut Hunk) {
    if let Some(last) = h.lines.last_mut() {
        match last {
            HunkLine::Ctx(x) | HunkLine::Del(x) | HunkLine::Add(x) => {
                if x.ends_with(b"\n") {
                    x.pop();
                }
            }
        }
    }
}

/// Apply `hunks` to `old`; the error names the first hunk that does not fit.
pub fn apply_hunks(old: &[u8], hunks: &[Hunk]) -> std::result::Result<Vec<u8>, String> {
    let src = lines_of(old);
    let mut out: Vec<u8> = Vec::with_capacity(old.len());
    let mut taken = 0usize; // source lines already copied
    let mut offset: isize = 0;
    for (hi, h) in hunks.iter().enumerate() {
        let want: Vec<&[u8]> = h
            .lines
            .iter()
            .filter_map(|l| match l {
                HunkLine::Ctx(x) | HunkLine::Del(x) => Some(x.as_slice()),
                HunkLine::Add(_) => None,
            })
            .collect();
        let expected = if h.old_len == 0 {
            h.old_start
        } else {
            h.old_start.saturating_sub(1)
        };
        let guess = (expected as isize + offset).max(taken as isize) as usize;
        let fits = |pos: usize| -> bool {
            pos >= taken && pos + want.len() <= src.len() && want.iter().enumerate().all(|(k, w)| src[pos + k] == *w)
        };
        let pos = if h.old_len == 0 && want.is_empty() {
            guess.min(src.len())
        } else if fits(guess) {
            guess
        } else {
            let max = src.len();
            let mut found = None;
            for d in 1..=max {
                if guess + d <= max && fits(guess + d) {
                    found = Some(guess + d);
                    break;
                }
                if d <= guess && fits(guess - d) {
                    found = Some(guess - d);
                    break;
                }
            }
            match found {
                Some(p) => p,
                None => {
                    return Err(format!(
                        "hunk #{} (@@ -{},{} +{},{} @@) does not apply: the context or removed lines are not in the file",
                        hi + 1,
                        h.old_start,
                        h.old_len,
                        h.new_start,
                        h.new_len
                    ))
                }
            }
        };
        offset = pos as isize - expected as isize;
        for s in &src[taken..pos] {
            out.extend_from_slice(s);
        }
        let mut k = pos;
        for l in &h.lines {
            match l {
                HunkLine::Ctx(x) => {
                    out.extend_from_slice(x);
                    k += 1;
                }
                HunkLine::Del(_) => k += 1,
                HunkLine::Add(x) => out.extend_from_slice(x),
            }
        }
        taken = k;
    }
    for s in &src[taken..] {
        out.extend_from_slice(s);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -2,3 +2,3 @@\n b\n-c\n+C\n d\n";

    #[test]
    fn parse_and_apply_with_offset() {
        let ps = parse(P.as_bytes(), 1).unwrap();
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].old_path.as_deref(), Some("f"));
        let out = apply_hunks(b"a\nb\nc\nd\ne\n", &ps[0].hunks).unwrap();
        assert_eq!(out, b"a\nb\nC\nd\ne\n");
        // shifted by two lines: still found
        let out = apply_hunks(b"x\ny\na\nb\nc\nd\ne\n", &ps[0].hunks).unwrap();
        assert_eq!(out, b"x\ny\na\nb\nC\nd\ne\n");
        assert!(apply_hunks(b"a\nb\nZ\nd\n", &ps[0].hunks).is_err());
    }

    #[test]
    fn new_file_no_newline_and_reverse() {
        let p = "--- /dev/null\n+++ b/n\n@@ -0,0 +1,2 @@\n+one\n+two\n\\ No newline at end of file\n";
        let ps = parse(p.as_bytes(), 1).unwrap();
        assert!(ps[0].old_path.is_none());
        let out = apply_hunks(b"", &ps[0].hunks).unwrap();
        assert_eq!(out, b"one\ntwo");
        let back = apply_hunks(&out, &ps[0].reversed().hunks).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn vcs_dialect_rename_binary_and_junk_prefix() {
        let p = "commit abc\n\n    msg\n\ndiff --vcs a/x b/x\nnew file\n--- /dev/null\n+++ b/x\n@@ -0,0 +1 @@\n+hi\ndiff --git a/o b/n\nsimilarity index 100%\nrename from o\nrename to n\ndiff --git a/b.bin b/b.bin\nBinary files a/b.bin and b/b.bin differ\n";
        let ps = parse(p.as_bytes(), 1).unwrap();
        assert_eq!(ps.len(), 3);
        assert_eq!(ps[1].old_path.as_deref(), Some("o"));
        assert_eq!(ps[1].new_path.as_deref(), Some("n"));
        assert!(ps[2].binary);
    }

    #[test]
    fn truncated_and_corrupt_are_errors() {
        assert!(parse(b"--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n a\n", 1).is_err());
        assert!(parse(b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n?x\n", 1).is_err());
    }
}
