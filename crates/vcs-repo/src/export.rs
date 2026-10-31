//! Export one zone branch as a Git fast-import stream. See `docs/interop.md`.
//!
//! Pure stream writer: the core never runs `git` or links libgit2. Output is deterministic (marks follow a
//! parents-first walk; blobs are emitted once, at first use).
use crate::error::{msg, Error, Result};
use crate::repo::Repo;
use crate::treeops::{self, Flat};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use vcs_objects::{Commit, EntryMode, ObjectId};

/// Trailer added to exported messages so a future import can restore the stable change id.
pub const CHANGE_ID_TRAILER: &str = "Vcs-Change-Id";

#[derive(Debug, Default)]
pub struct ExportReport {
    pub git_ref: String,
    pub commits: usize,
    pub blobs: usize,
    /// `(our commit id, mark)` in stream order.
    pub marks: Vec<(ObjectId, usize)>,
}

fn io_err(e: std::io::Error) -> Error {
    Error::Msg(format!("write error: {e}"))
}

/// Conservative subset of `git check-ref-format`.
fn ref_ok(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.ends_with(['/', '.'])
        && !name.contains("..")
        && !name.contains("@{")
        && !name.contains("//")
        && name != "@"
        && !name.split('/').any(|c| c.starts_with('.') || c.ends_with(".lock"))
        && !name.chars().any(|c| c.is_control() || c == ' ' || "~^:?*[\\".contains(c))
}

fn quote_path(p: &str) -> String {
    if !p.starts_with('"') && !p.contains(['"', '\\', '\n']) {
        return p.to_string();
    }
    let mut o = String::from("\"");
    for c in p.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Ident field: Git forbids `<`, `>` and newlines in names/emails.
fn clean(s: &str) -> String {
    s.chars().map(|c| if matches!(c, '<' | '>' | '\n') { ' ' } else { c }).collect()
}

fn message_with_trailer(c: &Commit) -> String {
    let key = format!("{CHANGE_ID_TRAILER}: ");
    if c.message.lines().any(|l| l.starts_with(&key)) {
        return c.message.clone();
    }
    let mut m = c.message.trim_end_matches('\n').to_string();
    m.push_str(if m.contains("\n\n") || m.lines().count() > 1 {
        "\n"
    } else {
        "\n\n"
    });
    m.push_str(&format!("{key}{}\n", c.change_id));
    m
}

impl Repo {
    /// Write `zone`'s `branch` to `out` as a fast-import stream for `refs/heads/<branch>`.
    /// Paths are zone-relative. Zone membership, transactions and snapshots are not represented (see docs).
    pub fn export_git(&self, zone: &str, branch: &str, out: &mut impl Write) -> Result<ExportReport> {
        if !ref_ok(branch) {
            return msg(format!("branch name {branch:?} is not a valid Git ref name"));
        }
        let refs = self.refs()?;
        let head = refs
            .head(zone, branch)
            .ok_or_else(|| Error::Msg(format!("no such branch {zone}/{branch}")))?;
        // parents-first order, deterministic (iterative DFS, first parent first)
        let mut order: Vec<ObjectId> = Vec::new();
        let mut commits: BTreeMap<ObjectId, Commit> = BTreeMap::new();
        let mut done = BTreeSet::new();
        let mut stack = vec![(head, false)];
        while let Some((id, expanded)) = stack.pop() {
            if expanded {
                order.push(id);
                continue;
            }
            if !done.insert(id) {
                continue;
            }
            let c = self.commit_obj(&id)?;
            if c.zone != zone {
                return Err(Error::Corrupt(format!("commit {id} has parent from different zone")));
            }
            stack.push((id, true));
            for p in c.parents.iter().rev() {
                if !done.contains(p) {
                    stack.push((*p, false));
                }
            }
            commits.insert(id, c);
        }

        let git_ref = format!("refs/heads/{branch}");
        let mut rep = ExportReport {
            git_ref: git_ref.clone(),
            ..Default::default()
        };
        let mut commit_mark: BTreeMap<ObjectId, usize> = BTreeMap::new();
        let mut blob_mark: BTreeMap<ObjectId, usize> = BTreeMap::new();
        let mut flats: BTreeMap<ObjectId, Flat> = BTreeMap::new();
        let mut next = 0usize;
        for id in order {
            let c = &commits[&id];
            let flat = treeops::flatten(self.store(), Some(c.root), "")?;
            let base = match c.parents.first() {
                Some(p) => flats.get(p).cloned().map(Ok).unwrap_or_else(|| {
                    let pc = self.commit_obj(p)?;
                    treeops::flatten(self.store(), Some(pc.root), "")
                })?,
                None => Flat::new(),
            };
            let mut lines: Vec<u8> = Vec::new();
            for (path, (mode, bid)) in &flat {
                if base.get(path) == Some(&(*mode, *bid)) {
                    continue;
                }
                let m = match blob_mark.get(bid) {
                    Some(m) => *m,
                    None => {
                        let data = self.store().get_content(bid)?;
                        next += 1;
                        write!(out, "blob\nmark :{next}\ndata {}\n", data.len()).map_err(io_err)?;
                        out.write_all(&data).map_err(io_err)?;
                        out.write_all(b"\n").map_err(io_err)?;
                        blob_mark.insert(*bid, next);
                        rep.blobs += 1;
                        next
                    }
                };
                let git_mode = match mode {
                    EntryMode::Executable => "100755",
                    EntryMode::Symlink => "120000",
                    _ => "100644",
                };
                writeln!(lines, "M {git_mode} :{m} {}", quote_path(path)).map_err(io_err)?;
            }
            for path in base.keys().filter(|p| !flat.contains_key(*p)) {
                writeln!(lines, "D {}", quote_path(path)).map_err(io_err)?;
            }
            next += 1;
            let ident = format!(
                "{} <{}> {} +0000",
                clean(&c.author.name),
                clean(&c.author.email),
                c.author.timestamp
            );
            let msg_text = message_with_trailer(c);
            write!(
                out,
                "commit {git_ref}\nmark :{next}\nauthor {ident}\ncommitter {ident}\ndata {}\n{msg_text}\n",
                msg_text.len()
            )
            .map_err(io_err)?;
            for (i, p) in c.parents.iter().enumerate() {
                let pm = commit_mark
                    .get(p)
                    .ok_or_else(|| Error::Corrupt(format!("parent {p} not exported")))?;
                writeln!(out, "{} :{pm}", if i == 0 { "from" } else { "merge" }).map_err(io_err)?;
            }
            out.write_all(&lines).map_err(io_err)?;
            out.write_all(b"\n").map_err(io_err)?;
            commit_mark.insert(id, next);
            rep.marks.push((id, next));
            rep.commits += 1;
            flats.insert(id, flat);
        }
        out.write_all(b"done\n").map_err(io_err)?;
        Ok(rep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ref_names() {
        assert!(ref_ok("main") && ref_ok("feature/x-1"));
        for bad in ["", "a..b", "a b", "x.lock", "a/.b", "a@{1}", "a~", "a/", "@"] {
            assert!(!ref_ok(bad), "{bad}");
        }
    }
    #[test]
    fn quoting() {
        assert_eq!(quote_path("a b/c"), "a b/c");
        assert_eq!(quote_path("a\"b"), "\"a\\\"b\"");
    }
}
