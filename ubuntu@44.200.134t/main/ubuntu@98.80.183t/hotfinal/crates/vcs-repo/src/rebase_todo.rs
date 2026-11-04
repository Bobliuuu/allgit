//! The todo list of `rebase -i`: generation (with `--autosquash` and `--exec`), the text an
//! editor sees, and parsing it back. Pure functions; the replay lives in `rebase.rs`.
//!
//! Text format (git's): one `<command> <commit> <subject>` per line, `#` comments and blank
//! lines ignored; commands `pick`/`p`, `reword`/`r`, `edit`/`e`, `squash`/`s`, `fixup`/`f`,
//! `drop`/`d`, `exec`/`x <shell command>`. A commit left out of the list is dropped. Commits are
//! named by any unique hex prefix. `break`, `label`, `reset`, `merge` and `fixup -C/-c` are
//! refused with a message, never ignored.

use crate::error::{msg, Result};
use crate::rebase::{Act, PlanCommit, Step};
use vcs_objects::ObjectId;

/// A todo line as generated or parsed (`Drop` is only seen in text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TodoLine {
    Pick(Act, usize),
    Drop(usize),
    Exec(String),
}

/// The default todo: every commit as `pick`, in order; with `autosquash`, `fixup! X` and
/// `squash! X` commits moved right after the commit whose subject starts with `X` (or whose id
/// starts with it) and turned into `fixup`/`squash`; with `exec`, each command after every
/// commit (after the last of a fold chain).
pub fn default_todo(commits: &[PlanCommit], autosquash: bool, exec: &[String]) -> Vec<TodoLine> {
    let n = commits.len();
    let mut act = vec![Act::Pick; n];
    let mut order: Vec<usize> = Vec::with_capacity(n);
    if autosquash {
        // attach[i] = target index i is folded into (resolved to the chain's root)
        let mut attach: Vec<Option<usize>> = vec![None; n];
        for i in 0..n {
            let subj = commits[i].subject.as_str();
            let (a, target) = if let Some(t) = subj.strip_prefix("fixup! ") {
                (Act::Fixup, t)
            } else if let Some(t) = subj.strip_prefix("squash! ") {
                (Act::Squash, t)
            } else {
                continue;
            };
            let target = target.trim();
            if target.is_empty() {
                continue;
            }
            let hex = |j: usize| commits[j].id.to_string();
            let found = (0..i)
                .rev()
                .find(|&j| commits[j].subject == target || hex(j) == target)
                .or_else(|| {
                    (0..i)
                        .rev()
                        .find(|&j| commits[j].subject.starts_with(target) || hex(j).starts_with(target))
                });
            if let Some(mut j) = found {
                while let Some(r) = attach[j] {
                    j = r;
                }
                attach[i] = Some(j);
                act[i] = a;
            }
        }
        for i in 0..n {
            if attach[i].is_some() {
                continue;
            }
            order.push(i);
            order.extend((0..n).filter(|&k| attach[k] == Some(i)));
        }
    } else {
        order.extend(0..n);
    }
    let mut out = Vec::new();
    for (pos, &i) in order.iter().enumerate() {
        out.push(TodoLine::Pick(act[i], i));
        let next_folds = order.get(pos + 1).is_some_and(|&k| matches!(act[k], Act::Squash | Act::Fixup));
        if !next_folds {
            out.extend(exec.iter().map(|c| TodoLine::Exec(c.clone())));
        }
    }
    out
}

/// The text shown in the editor.
pub fn render(lines: &[TodoLine], commits: &[PlanCommit], onto_label: &str) -> String {
    let mut s = String::new();
    for l in lines {
        match l {
            TodoLine::Pick(a, i) => s.push_str(&format!("{} {} {}\n", a.name(), commits[*i].id.short(), commits[*i].subject)),
            TodoLine::Drop(i) => s.push_str(&format!("drop {} {}\n", commits[*i].id.short(), commits[*i].subject)),
            TodoLine::Exec(c) => s.push_str(&format!("exec {c}\n")),
        }
    }
    s.push_str(&format!(
        "\n# Rebase onto {onto_label} ({} command(s))\n#\n# Commands:\n# p, pick <commit> = use commit\n# r, reword <commit> = use commit, but edit the commit message\n# e, edit <commit> = use commit, but stop for amending\n# s, squash <commit> = use commit, but meld into previous commit\n# f, fixup <commit> = like \"squash\", but discard this commit's log message\n# x, exec <command> = run command (the rest of the line) using shell\n# d, drop <commit> = remove commit\n#\n# These lines can be re-ordered; they are executed from top to bottom.\n# If you remove a line here THAT COMMIT WILL BE LOST.\n# If you remove everything, the rebase is aborted.\n",
        lines.len()
    ));
    s
}

/// Parse the edited text into steps (drops removed). `commits` is the range being rebased.
pub fn parse(text: &str, commits: &[PlanCommit]) -> Result<Vec<Step>> {
    let mut steps = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (word, rest) = match line.split_once(char::is_whitespace) {
            Some((w, r)) => (w, r.trim()),
            None => (line, ""),
        };
        let at = |m: String| -> Result<Vec<Step>> { msg(format!("todo line {}: {m}", n + 1)) };
        let act = match word {
            "pick" | "p" => Some(Act::Pick),
            "reword" | "r" => Some(Act::Reword),
            "edit" | "e" => Some(Act::Edit),
            "squash" | "s" => Some(Act::Squash),
            "fixup" | "f" => Some(Act::Fixup),
            "drop" | "d" => None,
            "exec" | "x" => {
                if rest.is_empty() {
                    return at("exec needs a command".into()).map(|_| vec![]);
                }
                steps.push(Step::Exec(rest.to_owned()));
                continue;
            }
            "break" | "b" | "label" | "l" | "reset" | "t" | "merge" | "m" | "update-ref" | "u" | "noop" => {
                if word == "noop" {
                    continue;
                }
                return at(format!(
                    "{word:?} is not supported by this rebase (only pick, reword, edit, squash, fixup, drop, exec)"
                ))
                .map(|_| vec![]);
            }
            _ => return at(format!("unknown command {word:?}")).map(|_| vec![]),
        };
        let mut it = rest.split_whitespace();
        let mut id_tok = it.next().unwrap_or("");
        if matches!(act, Some(Act::Fixup)) && (id_tok == "-C" || id_tok == "-c") {
            return at("fixup -C/-c is not supported".into()).map(|_| vec![]);
        }
        if id_tok.is_empty() {
            return at(format!("{word} needs a commit")).map(|_| vec![]);
        }
        id_tok = id_tok.trim();
        let low = id_tok.to_ascii_lowercase();
        let hits: Vec<&PlanCommit> = commits.iter().filter(|c| c.id.to_string().starts_with(&low)).collect();
        let id: ObjectId = match hits.as_slice() {
            [one] if low.len() >= 4 => one.id,
            [] => return at(format!("{id_tok:?} is not a commit of the range being rebased")).map(|_| vec![]),
            _ if low.len() < 4 => return at(format!("{id_tok:?} is too short to name a commit")).map(|_| vec![]),
            _ => return at(format!("{id_tok:?} is ambiguous")).map(|_| vec![]),
        };
        if let Some(a) = act {
            steps.push(Step::Pick(a, id));
        }
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pc(n: u8, s: &str) -> PlanCommit {
        PlanCommit {
            id: ObjectId::from_hex(&format!("{n:02x}").repeat(32)).unwrap(),
            subject: s.into(),
        }
    }

    #[test]
    fn autosquash_moves_fixups() {
        let cs = vec![
            pc(1, "add a"),
            pc(2, "add b"),
            pc(3, "fixup! add a"),
            pc(4, "squash! add b"),
            pc(5, "fixup! fixup! add a"),
        ];
        let t = default_todo(&cs, true, &[]);
        assert_eq!(
            t,
            vec![
                TodoLine::Pick(Act::Pick, 0),
                TodoLine::Pick(Act::Fixup, 2),
                TodoLine::Pick(Act::Fixup, 4),
                TodoLine::Pick(Act::Pick, 1),
                TodoLine::Pick(Act::Squash, 3),
            ]
        );
        let plain = default_todo(&cs, false, &["true".into()]);
        assert_eq!(plain.len(), 10);
    }

    #[test]
    fn render_parse_roundtrip_and_errors() {
        let cs = vec![pc(1, "one"), pc(2, "two")];
        let text = render(&default_todo(&cs, false, &[]), &cs, "main");
        let steps = parse(&text, &cs).unwrap();
        assert_eq!(steps, vec![Step::Pick(Act::Pick, cs[0].id), Step::Pick(Act::Pick, cs[1].id)]);
        let st = parse("s 0101\nd 0202\nexec make test\n", &cs).unwrap();
        assert_eq!(st, vec![Step::Pick(Act::Squash, cs[0].id), Step::Exec("make test".into())]);
        assert!(parse("pick 9999 x\n", &cs).is_err());
        assert!(parse("break\n", &cs).is_err());
        assert!(parse("fixup -C 0101\n", &cs).is_err());
        assert!(parse("frob 0101\n", &cs).is_err());
    }
}
