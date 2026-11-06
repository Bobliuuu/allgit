//! Interactive hunk selection: `vcs add -p`, `restore -p [--staged]`, `checkout -p [<rev>]` and
//! `stash push -p`.
//!
//! Every command shows the changed hunks of each file and reads one answer per hunk from standard
//! input (a terminal, or a script: one answer per line, end of input counts as `q`):
//!
//! * `y` use this hunk, `n` do not, `q` quit (this and every later hunk is not used),
//! * `a` use this and every later hunk of the file, `d` do not use this or any later hunk of the file,
//! * `s` split the hunk into smaller ones (needs an unchanged line between two changes),
//! * `e` edit the hunk in `core.editor` (only where the hunk is applied as given: `add -p` and
//!   `checkout -p <rev>`), `j`/`J` next hunk, `k`/`K` previous hunk, `g` go to a hunk, `?` help.
//!
//! Hunks are computed with the same line diff as `vcs diff` (Myers, three lines of context, changes
//! closer than six unchanged lines share a hunk). A file's result is built from the old and the new
//! lines per hunk, so nothing here depends on patch offsets.
use crate::config::Config;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use vcs_merge::diff::{matches, Algorithm};
use vcs_objects::{EntryMode, Signature};
use vcs_repo::diff::{is_binary, lines_of};
use vcs_repo::{Error, FilePair, Repo};

type R<T> = Result<T, Error>;

fn msg<T>(s: impl Into<String>) -> R<T> {
    Err(Error::Msg(s.into()))
}

const CTX: usize = 3;

/// One run of changed lines: `a[a0..a1]` became `b[b0..b1]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Block {
    a0: usize,
    a1: usize,
    b0: usize,
    b1: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Undecided,
    Yes,
    No,
    /// Used, with these lines instead of the new ones (after `e`).
    Edited(Vec<Vec<u8>>),
}

impl State {
    fn used(&self) -> bool {
        matches!(self, State::Yes | State::Edited(_))
    }
}

/// A hunk as the user sees it: a range of blocks and the decision taken on it.
#[derive(Clone, Debug)]
pub struct Unit {
    blocks: std::ops::Range<usize>,
    pub state: State,
}

/// The line-level difference between two versions of a file.
pub struct FileDiff<'a> {
    a: Vec<&'a [u8]>,
    b: Vec<&'a [u8]>,
    blocks: Vec<Block>,
}

impl<'a> FileDiff<'a> {
    pub fn new(old: &'a [u8], new: &'a [u8]) -> FileDiff<'a> {
        let a = lines_of(old);
        let b = lines_of(new);
        let mut ids: HashMap<&[u8], u32> = HashMap::new();
        let mut intern = |l: &'a [u8]| {
            let n = ids.len() as u32;
            *ids.entry(l).or_insert(n)
        };
        let ia: Vec<u32> = a.iter().map(|l| intern(l)).collect();
        let ib: Vec<u32> = b.iter().map(|l| intern(l)).collect();
        let mut blocks = Vec::new();
        let (mut pa, mut pb) = (0usize, 0usize);
        for (i, j) in matches(Algorithm::Myers, &ia, &ib)
            .into_iter()
            .chain(std::iter::once((a.len(), b.len())))
        {
            if i > pa || j > pb {
                blocks.push(Block {
                    a0: pa,
                    a1: i,
                    b0: pb,
                    b1: j,
                });
            }
            pa = i + 1;
            pb = j + 1;
        }
        FileDiff { a, b, blocks }
    }

    /// The hunks git would offer: changes with at most `2 * CTX` unchanged lines between them share one.
    pub fn units(&self) -> Vec<Unit> {
        let mut out: Vec<Unit> = Vec::new();
        for (k, bl) in self.blocks.iter().enumerate() {
            match out.last_mut() {
                Some(u) if bl.a0 - self.blocks[u.blocks.end - 1].a1 <= 2 * CTX => u.blocks.end = k + 1,
                _ => out.push(Unit {
                    blocks: k..k + 1,
                    state: State::Undecided,
                }),
            }
        }
        out
    }

    fn split(&self, u: &Unit) -> Vec<Unit> {
        u.blocks
            .clone()
            .map(|k| Unit {
                blocks: k..k + 1,
                state: State::Undecided,
            })
            .collect()
    }

    /// Unchanged lines shown before and after the hunk.
    fn context(&self, r: &std::ops::Range<usize>) -> (usize, usize) {
        let first = self.blocks[r.start];
        let last = self.blocks[r.end - 1];
        let before = if r.start == 0 {
            first.a0
        } else {
            first.a0 - self.blocks[r.start - 1].a1
        };
        let after = match self.blocks.get(r.end) {
            Some(n) => n.a0 - last.a1,
            None => self.a.len() - last.a1,
        };
        (before.min(CTX), after.min(CTX))
    }

    pub fn hunk_text(&self, u: &Unit) -> String {
        let first = self.blocks[u.blocks.start];
        let last = self.blocks[u.blocks.end - 1];
        let (cb, ca) = self.context(&u.blocks);
        let (a0, a1) = (first.a0 - cb, last.a1 + ca);
        let (b0, b1) = (first.b0 - cb, last.b1 + ca);
        let range = |s: usize, e: usize| -> String {
            match e - s {
                1 => format!("{}", s + 1),
                0 => format!("{s},0"),
                n => format!("{},{n}", s + 1),
            }
        };
        let mut out = format!("@@ -{} +{} @@\n", range(a0, a1), range(b0, b1));
        let mut line = |p: char, l: &[u8]| {
            out.push(p);
            out.push_str(&String::from_utf8_lossy(l));
            if !l.ends_with(b"\n") {
                out.push_str("\n\\ No newline at end of file\n");
            }
        };
        for l in &self.a[a0..first.a0] {
            line(' ', l);
        }
        for k in u.blocks.clone() {
            let bl = self.blocks[k];
            if k > u.blocks.start {
                for l in &self.a[self.blocks[k - 1].a1..bl.a0] {
                    line(' ', l);
                }
            }
            for l in &self.a[bl.a0..bl.a1] {
                line('-', l);
            }
            for l in &self.b[bl.b0..bl.b1] {
                line('+', l);
            }
        }
        for l in &self.a[last.a1..a1] {
            line(' ', l);
        }
        out
    }

    /// The file after the decisions. A used hunk contributes the new lines when `used_takes_new`,
    /// otherwise the old ones (the "discard this hunk" commands); an unused or undecided hunk the
    /// other side.
    pub fn render(&self, units: &[Unit], used_takes_new: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        for u in units {
            let first = self.blocks[u.blocks.start];
            let last = self.blocks[u.blocks.end - 1];
            for l in &self.a[pos..first.a0] {
                out.extend_from_slice(l);
            }
            match &u.state {
                State::Edited(lines) => lines.iter().for_each(|l| out.extend_from_slice(l)),
                s => {
                    let take_new = s.used() == used_takes_new;
                    let src = if take_new {
                        &self.b[first.b0..last.b1]
                    } else {
                        &self.a[first.a0..last.a1]
                    };
                    src.iter().for_each(|l| out.extend_from_slice(l));
                }
            }
            pos = last.a1;
        }
        for l in &self.a[pos..] {
            out.extend_from_slice(l);
        }
        out
    }

    /// The edited text of hunk `u` (context kept) as the new inner lines, or why it cannot be used.
    fn apply_edit(&self, u: &Unit, text: &[u8]) -> R<Vec<Vec<u8>>> {
        let first = self.blocks[u.blocks.start];
        let last = self.blocks[u.blocks.end - 1];
        let (cb, ca) = self.context(&u.blocks);
        // (kind, line) with the trailing newline restored
        let mut items: Vec<(u8, Vec<u8>)> = Vec::new();
        for raw in lines_of(text) {
            if raw.starts_with(b"#") || raw.starts_with(b"@@") {
                continue;
            }
            if raw.starts_with(b"\\") {
                if let Some(last) = items.last_mut() {
                    if last.1.ends_with(b"\n") {
                        last.1.pop();
                    }
                }
                continue;
            }
            let (kind, rest) = match raw.first() {
                Some(b'+') => (b'+', &raw[1..]),
                Some(b'-') => (b'-', &raw[1..]),
                Some(b' ') => (b' ', &raw[1..]),
                Some(b'\n') => (b' ', raw),
                _ => return msg("every line of the hunk must start with ' ', '-' or '+'"),
            };
            items.push((kind, rest.to_vec()));
        }
        let old: Vec<&[u8]> = items.iter().filter(|(k, _)| *k != b'+').map(|(_, l)| l.as_slice()).collect();
        let want = &self.a[first.a0 - cb..last.a1 + ca];
        if old != want {
            return msg("the edited hunk does not match the file: only '+' lines may be added or removed (make a '-' line ' ' to keep it)");
        }
        if items.len() < cb + ca || items[..cb].iter().chain(items[items.len() - ca..].iter()).any(|(k, _)| *k != b' ') {
            return msg("the context lines around the hunk must be kept");
        }
        let new_side: Vec<Vec<u8>> = items.into_iter().filter(|(k, _)| *k != b'-').map(|(_, l)| l).collect();
        Ok(new_side[cb..new_side.len() - ca].to_vec())
    }
}

/// Answers in, hunks out, plus the editor for `e`.
pub struct Session<'a> {
    pub inp: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
    pub editor: Option<String>,
    pub edit_file: PathBuf,
    /// `q` (or the end of input) was seen: nothing further is offered.
    pub quit: bool,
}

const HELP: &str = "y - use this hunk
n - do not use this hunk
q - quit; do not use this hunk or any of the remaining ones
a - use this hunk and all later hunks in the file
d - do not use this hunk or any of the later hunks in the file
j - leave this hunk undecided, see next undecided hunk
J - leave this hunk undecided, see next hunk
k - leave this hunk undecided, see previous undecided hunk
K - leave this hunk undecided, see previous hunk
g - select a hunk to go to
s - split the current hunk into smaller hunks
e - manually edit the current hunk
? - print help
";

impl Session<'_> {
    fn say(&mut self, s: &str) {
        let _ = self.out.write_all(s.as_bytes());
        let _ = self.out.flush();
    }

    fn answer(&mut self) -> Option<String> {
        let mut l = String::new();
        match self.inp.read_line(&mut l) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(l.trim().to_owned()),
        }
    }

    /// Ask about every hunk of one file. `verb` completes "(1/2) ___ this hunk"; `can_edit` offers `e`.
    pub fn select(&mut self, path: &str, fd: &FileDiff, verb: &str, can_edit: bool) -> Vec<Unit> {
        let mut units = fd.units();
        if units.is_empty() || self.quit {
            return units;
        }
        self.say(&format!("--- a/{path}\n+++ b/{path}\n"));
        let mut i = 0usize;
        let next_undecided = |us: &[Unit], from: usize| -> Option<usize> {
            (from..us.len())
                .chain(0..from.min(us.len()))
                .find(|&k| us[k].state == State::Undecided)
        };
        loop {
            if units.iter().all(|u| u.state != State::Undecided) {
                break;
            }
            if i >= units.len() {
                i = next_undecided(&units, 0).unwrap_or(0);
            }
            let text = fd.hunk_text(&units[i]);
            self.say(&text);
            let n = units.len();
            let splittable = units[i].blocks.len() > 1;
            let mut keys = vec!["y", "n", "q", "a", "d"];
            if i > 0 {
                keys.extend(["k", "K"]);
            }
            if i + 1 < n {
                keys.extend(["j", "J"]);
            }
            if n > 1 {
                keys.push("g");
            }
            if splittable {
                keys.push("s");
            }
            if can_edit {
                keys.push("e");
            }
            keys.push("?");
            self.say(&format!("({}/{n}) {verb} this hunk [{}]? ", i + 1, keys.join(",")));
            let Some(ans) = self.answer() else {
                self.say("\n");
                self.quit = true;
                break;
            };
            let cmd = ans.chars().next().unwrap_or(' ');
            match cmd {
                'y' | 'n' => {
                    units[i].state = if cmd == 'y' { State::Yes } else { State::No };
                    i = next_undecided(&units, i + 1).unwrap_or(units.len());
                }
                'q' => {
                    self.quit = true;
                    break;
                }
                'a' | 'd' => {
                    let st = if cmd == 'a' { State::Yes } else { State::No };
                    for u in units[i..].iter_mut().filter(|u| u.state == State::Undecided) {
                        u.state = st.clone();
                    }
                    break;
                }
                'j' => i = next_undecided(&units, i + 1).unwrap_or(i),
                'J' => i = if i + 1 < n { i + 1 } else { 0 },
                'k' | 'K' if i > 0 => {
                    i = if cmd == 'K' {
                        i - 1
                    } else {
                        (0..i).rev().find(|&k| units[k].state == State::Undecided).unwrap_or(i - 1)
                    }
                }
                'k' | 'K' => self.say("No previous hunk\n"),
                'g' => {
                    let listing: String = units
                        .iter()
                        .enumerate()
                        .map(|(k, u)| {
                            let mark = match u.state {
                                State::Undecided => ' ',
                                State::No => '-',
                                _ => '+',
                            };
                            let head = fd.hunk_text(u).lines().next().unwrap_or("").to_owned();
                            format!("{mark}{:>3} {head}\n", k + 1)
                        })
                        .collect();
                    self.say(&listing);
                    self.say("go to which hunk? ");
                    match self.answer().and_then(|a| a.parse::<usize>().ok()) {
                        Some(k) if (1..=n).contains(&k) => i = k - 1,
                        _ => self.say("invalid hunk number\n"),
                    }
                }
                's' if splittable => {
                    let parts = fd.split(&units[i]);
                    self.say(&format!("Split into {} hunks.\n", parts.len()));
                    units.splice(i..=i, parts);
                }
                's' => self.say("Sorry, cannot split this hunk\n"),
                'e' if can_edit => match self.edit(fd, &units[i]) {
                    Ok(edited) => {
                        units[i].state = edited.map_or(State::No, State::Edited);
                        i = next_undecided(&units, i + 1).unwrap_or(units.len());
                    }
                    Err(e) => self.say(&format!("error: {e}\n")),
                },
                'e' => self.say("Sorry, cannot edit this hunk\n"),
                _ => self.say(HELP),
            }
        }
        units
    }

    /// Edit one hunk: `None` when the editor left it empty (the hunk is dropped).
    fn edit(&mut self, fd: &FileDiff, u: &Unit) -> R<Option<Vec<Vec<u8>>>> {
        let Some(editor) = self.editor.clone() else {
            return msg("no editor configured (core.editor, $VISUAL or $EDITOR)");
        };
        let mut text = fd.hunk_text(u);
        text.push_str(
            "# ---\n# To remove '-' lines, make them ' ' lines (context).\n# To remove '+' lines, delete them.\n# Lines starting with # will be removed.\n# If it does not apply cleanly, you will be given an opportunity to edit again.\n# If all lines of the hunk are removed, then the edit is aborted and the hunk is left unchanged.\n",
        );
        let io = |e: std::io::Error| Error::Msg(format!("{}: {e}", self.edit_file.display()));
        std::fs::write(&self.edit_file, text).map_err(io)?;
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$@\""))
            .arg("vcs-editor")
            .arg(&self.edit_file)
            .status()
            .map_err(|e| Error::Msg(format!("could not start editor '{editor}': {e}")))?;
        if !status.success() {
            return msg(format!("editor '{editor}' exited with {status}"));
        }
        let edited = std::fs::read(&self.edit_file).map_err(io)?;
        let _ = std::fs::remove_file(&self.edit_file);
        let meaningful = lines_of(&edited)
            .iter()
            .any(|l| !l.starts_with(b"#") && !l.starts_with(b"@@") && l.iter().any(|b| !b.is_ascii_whitespace()));
        if !meaningful {
            return Ok(None);
        }
        fd.apply_edit(u, &edited).map(Some)
    }
}

// ---------------------------------------------------------------- commands

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// index <- index + chosen hunks of the working tree
    Add,
    /// working tree <- index, for the chosen hunks
    RestoreWorktree,
    /// index <- HEAD, for the chosen hunks
    RestoreStaged,
    /// index and working tree <- `rev`, for the chosen hunks
    CheckoutRev(String),
    /// stash the chosen hunks of the working-tree changes against HEAD
    Stash,
}

fn in_specs(path: &str, specs: &[String]) -> bool {
    specs
        .iter()
        .any(|s| s.is_empty() || path == s || path.strip_prefix(s.as_str()).is_some_and(|r| r.starts_with('/')))
}

fn read_work(repo: &Repo, p: &str) -> R<Option<Vec<u8>>> {
    let abs = repo.worktree().join(p);
    match std::fs::symlink_metadata(&abs) {
        Ok(m) if m.is_file() => std::fs::read(&abs).map(Some).map_err(|source| Error::Io { path: abs, source }),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io { path: abs, source }),
    }
}

fn write_work(repo: &Repo, p: &str, data: Option<&[u8]>, mode: Option<EntryMode>) -> R<()> {
    let abs = repo.worktree().join(p);
    let io = |source| Error::Io { path: abs.clone(), source };
    match data {
        None => match std::fs::remove_file(&abs) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(e)),
        },
        Some(d) => {
            let existed = abs.exists();
            if let Some(par) = abs.parent() {
                std::fs::create_dir_all(par).map_err(io)?;
            }
            std::fs::write(&abs, d).map_err(io)?;
            #[cfg(unix)]
            if !existed && mode == Some(EntryMode::Executable) {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&abs, std::fs::Permissions::from_mode(0o755)).map_err(io)?;
            }
            #[cfg(not(unix))]
            let _ = (existed, mode);
        }
    }
    Ok(())
}

/// One file offered for selection: both sides (None: the file does not exist there).
struct Cand {
    path: String,
    a: Option<Vec<u8>>,
    b: Option<Vec<u8>>,
    mode: Option<EntryMode>,
}

fn skip_note(sess: &mut Session, path: &str, why: &str) {
    sess.say(&format!("{path}: {why}, not offered\n"));
}

fn from_pairs(pairs: Vec<FilePair>, specs: &[String], sess: &mut Session) -> Vec<Cand> {
    let mut out = Vec::new();
    for p in pairs.into_iter().filter(|p| in_specs(&p.path, specs)) {
        if p.old_mode == Some(EntryMode::Symlink) || p.new_mode == Some(EntryMode::Symlink) {
            skip_note(sess, &p.path, "symbolic link");
            continue;
        }
        if p.old.as_deref().is_some_and(is_binary) || p.new.as_deref().is_some_and(is_binary) {
            skip_note(sess, &p.path, "binary file");
            continue;
        }
        out.push(Cand {
            mode: p.new_mode.or(p.old_mode),
            path: p.path,
            a: p.old,
            b: p.new,
        });
    }
    out
}

/// The pathspecs relative to the repository root; empty means everything.
fn specs_of(repo: &Repo, cwd: &Path, paths: &[String]) -> R<Vec<String>> {
    if paths.is_empty() {
        return Ok(vec![String::new()]);
    }
    paths.iter().map(|p| repo.rel_path(cwd, p)).collect()
}

pub struct Args<'a> {
    pub kind: Kind,
    pub paths: &'a [String],
    pub message: Option<&'a str>,
}

pub fn run(repo: &Repo, cwd: &Path, args: Args, author: impl FnOnce() -> R<Signature>) -> R<()> {
    let stdin = std::io::stdin();
    let mut inp = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    run_with(repo, cwd, args, author, &mut inp, &mut out)
}

pub fn run_with(
    repo: &Repo,
    cwd: &Path,
    args: Args,
    author: impl FnOnce() -> R<Signature>,
    inp: &mut dyn BufRead,
    out: &mut dyn Write,
) -> R<()> {
    let specs = specs_of(repo, cwd, args.paths)?;
    let mut sess = Session {
        inp,
        out,
        editor: Config::load()?.editor(),
        edit_file: repo.worktree().join(".vcs").join("ADD_EDIT.patch"),
        quit: false,
    };
    let (verb, can_edit, used_takes_new) = match &args.kind {
        Kind::Add => ("Stage", true, true),
        Kind::RestoreWorktree => ("Discard", false, false),
        Kind::RestoreStaged => ("Unstage", false, false),
        Kind::CheckoutRev(r) if r == "HEAD" => ("Discard", false, false),
        Kind::CheckoutRev(_) => ("Apply", true, true),
        Kind::Stash => ("Stash", false, true),
    };
    let cands = candidates(repo, &args.kind, &specs, &mut sess)?;
    if cands.is_empty() {
        sess.say("No changes.\n");
        return Ok(());
    }
    // checkout <rev> changes the index as well as the file: both must agree first
    let unstaged: std::collections::BTreeSet<String> = if matches!(args.kind, Kind::CheckoutRev(_)) {
        repo.diff_pairs(false)?.into_iter().map(|p| p.path).collect()
    } else {
        Default::default()
    };
    let mut chosen: Vec<(usize, Vec<Unit>)> = Vec::new();
    for (ci, c) in cands.iter().enumerate() {
        if sess.quit {
            break;
        }
        let (a, b) = (c.a.as_deref().unwrap_or(&[]), c.b.as_deref().unwrap_or(&[]));
        let fd = FileDiff::new(a, b);
        let units = sess.select(&c.path, &fd, verb, can_edit);
        if units.iter().any(|u| u.state.used()) {
            if matches!(args.kind, Kind::CheckoutRev(_)) && unstaged.contains(&c.path) {
                return msg(format!(
                    "{}: the index and the working tree differ; stage the file or restore it before `checkout -p <rev>`",
                    c.path
                ));
            }
            chosen.push((ci, units));
        }
    }
    if chosen.is_empty() {
        sess.say("No hunks selected.\n");
        return Ok(());
    }
    let hunks: usize = chosen.iter().map(|(_, u)| u.iter().filter(|u| u.state.used()).count()).sum();
    let results: Vec<(usize, Option<Vec<u8>>)> = chosen
        .iter()
        .map(|(ci, units)| {
            let c = &cands[*ci];
            let fd = FileDiff::new(c.a.as_deref().unwrap_or(&[]), c.b.as_deref().unwrap_or(&[]));
            // a file that is wholly used ends up as the used side (which may be "absent")
            let all = units.iter().all(|u| u.state.used()) && !units.iter().any(|u| matches!(u.state, State::Edited(_)));
            let used_side = if used_takes_new { &c.b } else { &c.a };
            let data = if all {
                used_side.clone()
            } else {
                Some(fd.render(units, used_takes_new))
            };
            (*ci, data)
        })
        .collect();
    match &args.kind {
        Kind::Add => {
            for (ci, data) in &results {
                let c = &cands[*ci];
                match data {
                    Some(d) => repo.stage_content(&c.path, d, c.mode)?,
                    None => {
                        repo.add(std::slice::from_ref(&c.path))?;
                    }
                }
            }
            sess.say(&format!("staged {hunks} hunk(s) in {} file(s)\n", results.len()));
        }
        Kind::RestoreWorktree => {
            for (ci, data) in &results {
                let c = &cands[*ci];
                write_work(repo, &c.path, data.as_deref(), c.mode)?;
            }
            sess.say(&format!("discarded {hunks} hunk(s) in {} file(s)\n", results.len()));
        }
        Kind::RestoreStaged => {
            for (ci, data) in &results {
                let c = &cands[*ci];
                match data {
                    Some(d) => repo.stage_content(&c.path, d, c.mode)?,
                    None => {
                        repo.restore(std::slice::from_ref(&c.path), true)?;
                    }
                }
            }
            sess.say(&format!("unstaged {hunks} hunk(s) in {} file(s)\n", results.len()));
        }
        Kind::CheckoutRev(_) => {
            for (ci, data) in &results {
                let c = &cands[*ci];
                write_work(repo, &c.path, data.as_deref(), c.mode)?;
                repo.add(std::slice::from_ref(&c.path))?;
            }
            sess.say(&format!(
                "updated {hunks} hunk(s) in {} file(s), index and working tree\n",
                results.len()
            ));
        }
        Kind::Stash => {
            stash(repo, &cands, &chosen, &results, args.message, author()?)?;
            sess.say(&format!("stashed {hunks} hunk(s) from {} file(s)\n", results.len()));
        }
    }
    Ok(())
}

fn candidates(repo: &Repo, kind: &Kind, specs: &[String], sess: &mut Session) -> R<Vec<Cand>> {
    match kind {
        Kind::Add | Kind::RestoreWorktree => Ok(from_pairs(repo.diff_pairs(false)?, specs, sess)),
        Kind::RestoreStaged => Ok(from_pairs(repo.diff_pairs(true)?, specs, sess)),
        Kind::CheckoutRev(rev) => {
            let tree = repo.rev_files(rev)?;
            let mut paths: std::collections::BTreeSet<String> = tree.keys().cloned().collect();
            paths.extend(repo.tracked_files()?.into_iter().map(|t| t.0));
            for sp in specs.iter().filter(|s| !s.is_empty()) {
                if !paths.iter().any(|p| in_specs(p, std::slice::from_ref(sp))) {
                    return msg(format!("pathspec {sp:?} did not match any tracked file"));
                }
            }
            let mut pairs = Vec::new();
            for p in paths.into_iter().filter(|p| in_specs(p, specs)) {
                let old = tree.get(&p).map(|e| repo.content(&e.1)).transpose()?;
                let new = read_work(repo, &p)?;
                if old == new {
                    continue;
                }
                let mode = tree.get(&p).map(|e| e.0);
                // `Apply` shows the working tree -> `rev` direction, the "discard" form rev -> working tree
                let (a, b) = if rev == "HEAD" { (old, new) } else { (new, old) };
                pairs.push(FilePair {
                    path: p,
                    old: a,
                    new: b,
                    old_mode: mode,
                    new_mode: mode,
                });
            }
            Ok(from_pairs(pairs, specs, sess))
        }
        Kind::Stash => {
            let head = repo
                .rev_files("HEAD")
                .map_err(|_| Error::Msg("cannot stash: no commits yet".into()))?;
            let tracked = repo.tracked_files()?;
            if let Some(t) = tracked.iter().find(|t| !head.contains_key(&t.0)) {
                return msg(format!(
                    "stash -p: {} is staged as a new file; commit it, or unstage it with `vcs restore --staged`, first",
                    t.0
                ));
            }
            let mut pairs = Vec::new();
            for (p, (mode, id)) in head.iter() {
                let old = repo.content(id)?;
                let new = read_work(repo, p)?;
                if new.as_deref() == Some(&old[..]) {
                    continue;
                }
                pairs.push(FilePair {
                    path: p.clone(),
                    old: Some(old),
                    new,
                    old_mode: Some(*mode),
                    new_mode: Some(*mode),
                });
            }
            Ok(from_pairs(pairs, specs, sess))
        }
    }
}

/// `stash push -p`: set the unselected changes aside in the working tree, stash everything else,
/// put the unselected changes back. Every changed tracked file is restored to what it held before
/// if anything goes wrong in between.
fn stash(
    repo: &Repo,
    cands: &[Cand],
    chosen: &[(usize, Vec<Unit>)],
    results: &[(usize, Option<Vec<u8>>)],
    message: Option<&str>,
    author: Signature,
) -> R<()> {
    let head = repo.rev_files("HEAD")?;
    // what every changed file holds now, so that it can be put back
    let mut original: Vec<(String, Option<Vec<u8>>, EntryMode)> = Vec::new();
    for (p, (mode, id)) in head.iter() {
        let work = read_work(repo, p)?;
        if work.as_deref() != Some(&repo.content(id)?[..]) {
            original.push((p.clone(), work, *mode));
        }
    }
    let selected: HashMap<&str, (&Vec<Unit>, &Option<Vec<u8>>)> = chosen
        .iter()
        .zip(results)
        .map(|((ci, units), (_, data))| (cands[*ci].path.as_str(), (units, data)))
        .collect();
    let go = || -> R<()> {
        // 1. the working tree holds exactly what goes into the stash
        for (p, _, mode) in &original {
            let head_content = repo.content(&head[p].1)?;
            match selected.get(p.as_str()) {
                Some((_, data)) => write_work(repo, p, data.as_deref(), Some(*mode))?,
                None => write_work(repo, p, Some(&head_content), Some(*mode))?,
            }
        }
        repo.stash_push(message, author)?;
        // 2. what was not selected comes back as plain working-tree changes
        for (p, work, mode) in &original {
            let rest = match selected.get(p.as_str()) {
                Some((units, _)) => {
                    let c = cands.iter().find(|c| &c.path == p).expect("candidate");
                    let fd = FileDiff::new(c.a.as_deref().unwrap_or(&[]), c.b.as_deref().unwrap_or(&[]));
                    let all = units.iter().all(|u| u.state.used());
                    if all {
                        c.a.clone()
                    } else {
                        Some(fd.render(units, false))
                    }
                }
                None => work.clone(),
            };
            write_work(repo, p, rest.as_deref(), Some(*mode))?;
        }
        Ok(())
    };
    if let Err(e) = go() {
        for (p, work, mode) in &original {
            let _ = write_work(repo, p, work.as_deref(), Some(*mode));
        }
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_script(old: &str, new: &str, answers: &str, takes_new: bool) -> String {
        let fd = FileDiff::new(old.as_bytes(), new.as_bytes());
        let mut inp = answers.as_bytes();
        let mut out = Vec::new();
        let mut s = Session {
            inp: &mut inp,
            out: &mut out,
            editor: None,
            edit_file: PathBuf::from("/nonexistent"),
            quit: false,
        };
        let units = s.select("f", &fd, "Stage", false);
        String::from_utf8(fd.render(&units, takes_new)).unwrap()
    }

    fn lines(n: usize) -> String {
        (1..=n).map(|i| format!("l{i}\n")).collect()
    }

    #[test]
    fn far_changes_are_two_hunks_near_ones_one() {
        let old = lines(20);
        let far = old.replace("l2\n", "L2\n").replace("l18\n", "L18\n");
        assert_eq!(FileDiff::new(old.as_bytes(), far.as_bytes()).units().len(), 2);
        let near = old.replace("l2\n", "L2\n").replace("l6\n", "L6\n");
        assert_eq!(FileDiff::new(old.as_bytes(), near.as_bytes()).units().len(), 1);
    }

    #[test]
    fn y_n_q_a_d() {
        let old = lines(30);
        let new = old.replace("l2\n", "L2\n").replace("l15\n", "L15\n").replace("l28\n", "L28\n");
        assert_eq!(
            run_script(&old, &new, "y\nn\ny\n", true),
            old.replace("l2\n", "L2\n").replace("l28\n", "L28\n")
        );
        assert_eq!(run_script(&old, &new, "n\nq\n", true), old);
        assert_eq!(run_script(&old, &new, "y\nq\n", true), old.replace("l2\n", "L2\n"));
        assert_eq!(
            run_script(&old, &new, "n\na\n", true),
            old.replace("l15\n", "L15\n").replace("l28\n", "L28\n")
        );
        assert_eq!(run_script(&old, &new, "y\nd\n", true), old.replace("l2\n", "L2\n"));
        // end of input is q
        assert_eq!(run_script(&old, &new, "y\n", true), old.replace("l2\n", "L2\n"));
        // discard form: used hunks revert to the old text
        assert_eq!(run_script(&old, &new, "y\nn\nn\n", false), new.replace("L2\n", "l2\n"));
    }

    #[test]
    fn split_makes_separate_hunks() {
        let old = lines(10);
        let new = old.replace("l3\n", "L3\n").replace("l6\n", "L6\n");
        let fd = FileDiff::new(old.as_bytes(), new.as_bytes());
        assert_eq!(fd.units().len(), 1);
        assert_eq!(run_script(&old, &new, "s\nn\ny\n", true), old.replace("l6\n", "L6\n"));
        // an unsplittable hunk says so and asks again
        assert_eq!(run_script(&old, &new, "s\ny\ns\nn\n", true), old.replace("l3\n", "L3\n"));
    }

    #[test]
    fn hunk_header_and_no_newline() {
        let fd = FileDiff::new(b"a\nb\nc", b"a\nB\nc");
        let u = fd.units();
        let t = fd.hunk_text(&u[0]);
        assert_eq!(t, "@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n\\ No newline at end of file\n");
    }

    #[test]
    fn whole_file_added_or_removed_is_one_hunk() {
        let fd = FileDiff::new(b"", b"x\ny\n");
        assert_eq!(fd.units().len(), 1);
        assert!(fd.hunk_text(&fd.units()[0]).starts_with("@@ -0,0 +1,2 @@\n+x\n+y\n"));
        let fd = FileDiff::new(b"x\ny\n", b"");
        assert!(fd.hunk_text(&fd.units()[0]).starts_with("@@ -1,2 +0,0 @@\n-x\n-y\n"));
    }

    #[test]
    fn edit_keeps_context_and_only_changes_plus_lines() {
        let fd = FileDiff::new(b"a\nb\nc\n", b"a\nB\nc\n");
        let u = &fd.units()[0];
        // keep the '-' line as context, drop the '+' line: nothing changes
        let got = fd.apply_edit(u, b"@@ -1,3 +1,3 @@\n a\n b\n c\n").unwrap();
        assert_eq!(got.concat(), b"b\n");
        // change the '+' text
        let got = fd.apply_edit(u, b" a\n-b\n+X\n c\n").unwrap();
        assert_eq!(got.concat(), b"X\n");
        // touching the old side is refused
        assert!(fd.apply_edit(u, b" a\n-bb\n+B\n c\n").is_err());
        assert!(fd.apply_edit(u, b"-a\n-b\n+B\n c\n").is_err());
    }
}
