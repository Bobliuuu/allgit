//! Local ledger behind `vcs p4`'s numbered changelists (`.vcs/p4-changes`, plain text).
//!
//! A pending changelist is a number, a description and a set of opened files (path -> action).
//! Nothing is staged until `p4 submit -c N`. A submitted changelist is a number -> commit id
//! entry, which is what lets `@N` name a commit. Numbers come from one local counter, so they
//! are only meaningful in this repository: another clone has its own ledger.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use vcs_repo::Error;

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Pending {
    pub desc: String,
    /// repo-relative path -> "add" | "edit" | "delete"
    pub files: BTreeMap<String, String>,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Ledger {
    pub next: u64,
    pub pending: BTreeMap<u64, Pending>,
    /// changelist number -> commit id (hex)
    pub submitted: BTreeMap<u64, String>,
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\t', "\\t")
}
fn unesc(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

impl Ledger {
    pub fn path(worktree: &Path) -> PathBuf {
        worktree.join(".vcs").join("p4-changes")
    }

    pub fn parse(text: &str) -> Ledger {
        let mut l = Ledger {
            next: 1,
            ..Default::default()
        };
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            match f.as_slice() {
                ["next", n] => l.next = n.parse().unwrap_or(1).max(1),
                ["pending", n, d] => {
                    if let Ok(n) = n.parse() {
                        l.pending.entry(n).or_default().desc = unesc(d);
                    }
                }
                ["file", n, act, p] => {
                    if let Ok(n) = n.parse() {
                        l.pending.entry(n).or_default().files.insert(unesc(p), (*act).to_owned());
                    }
                }
                ["submitted", n, id] => {
                    if let Ok(n) = n.parse() {
                        l.submitted.insert(n, (*id).to_owned());
                    }
                }
                _ => {}
            }
        }
        let top = l.pending.keys().chain(l.submitted.keys()).max().copied().unwrap_or(0);
        l.next = l.next.max(top + 1);
        l
    }

    pub fn render(&self) -> String {
        let mut o = format!("next\t{}\n", self.next);
        for (n, p) in &self.pending {
            o.push_str(&format!("pending\t{n}\t{}\n", esc(&p.desc)));
            for (path, act) in &p.files {
                o.push_str(&format!("file\t{n}\t{act}\t{}\n", esc(path)));
            }
        }
        for (n, id) in &self.submitted {
            o.push_str(&format!("submitted\t{n}\t{id}\n"));
        }
        o
    }

    pub fn load(worktree: &Path) -> Ledger {
        std::fs::read_to_string(Self::path(worktree))
            .map(|t| Ledger::parse(&t))
            .unwrap_or_else(|_| Ledger::parse(""))
    }

    pub fn save(&self, worktree: &Path) -> Result<(), Error> {
        let p = Self::path(worktree);
        let tmp = p.with_extension("tmp");
        std::fs::write(&tmp, self.render())
            .and_then(|_| std::fs::rename(&tmp, &p))
            .map_err(|e| Error::Msg(format!("cannot write {}: {e}", p.display())))
    }

    pub fn alloc(&mut self) -> u64 {
        let n = self.next;
        self.next += 1;
        n
    }

    /// The pending changelist that has `path` open, if any.
    pub fn holder(&self, path: &str) -> Option<u64> {
        self.pending.iter().find(|(_, p)| p.files.contains_key(path)).map(|(n, _)| *n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_counter() {
        let mut l = Ledger::parse("");
        let a = l.alloc();
        let b = l.alloc();
        assert_eq!((a, b), (1, 2));
        let p = l.pending.entry(a).or_default();
        p.desc = "two\nlines\twith tab \\ slash".into();
        p.files.insert("src/a b.txt".into(), "edit".into());
        l.submitted.insert(7, "abcdef".into());
        let back = Ledger::parse(&l.render());
        assert_eq!(back.pending, l.pending);
        assert_eq!(back.submitted, l.submitted);
        // the counter never goes below the highest number in use
        assert_eq!(back.next, 8);
        assert_eq!(back.holder("src/a b.txt"), Some(1));
        assert_eq!(back.holder("nope"), None);
    }
}
