//! Export one zone branch as a Git fast-import stream. See `docs/interop.md`.
//!
//! Incremental mode (`ExportOptions`) skips commits recorded in `.vcs/git-export` and references them by
//! mark, so a target `git fast-import` that keeps its marks (`--export-marks` / `--import-marks`) reproduces
//! the same SHAs for old commits. Tags of the zone are exported as `tag` / `reset refs/tags/..` commands.
//!
//! Pure stream writer: the core never runs `git` or links libgit2. Output is deterministic (marks follow a
//! parents-first walk; blobs are emitted once, at first use).
use crate::error::{msg, Error, Result};
use crate::meta::write_atomic;
use crate::repo::Repo;
use crate::treeops::{self, Flat};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use vcs_objects::{Commit, EntryMode, Object, ObjectId};

const EXPORT_MAP: &str = "git-export";
const IMPORT_MAP: &str = "git-import";

/// How `export_git_with` treats earlier exports.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExportOptions {
    /// Skip commits already recorded in `.vcs/git-export` (referenced as `:<mark>`). The target Git
    /// must have loaded the marks of the earlier run (`git fast-import --import-marks=FILE`).
    pub incremental: bool,
    /// Also treat commits that came from `import-git` (key = a Git commit id) as present in the
    /// target, referencing them by their original SHA. Use when exporting back into the origin repo.
    pub onto_origin: bool,
    /// Write the commits of this run to `.vcs/git-export` (replacing the table unless `incremental`).
    pub record: bool,
}

/// Extra knobs for callers that drive several exports into one stream (the Git remote helper).
#[derive(Debug, Default, Clone)]
pub struct ExportExt {
    /// Commits the target already has, and how the stream refers to them (`:<mark>` or a Git sha).
    pub known: BTreeMap<ObjectId, String>,
    /// Marks continue after this number.
    pub mark_base: usize,
    /// Write to this ref instead of `refs/heads/<branch>`.
    pub git_ref: Option<String>,
    /// Do not write tags or the closing `done` (more exports follow in the same stream).
    pub partial: bool,
    /// Write every tag as a lightweight ref `<prefix>/<name>` (annotations are dropped) instead of
    /// `refs/tags/<name>` / `tag` commands. For importers that map tags into a private namespace.
    pub tag_ref_prefix: Option<String>,
}

/// Trailer added to exported messages so a future import can restore the stable change id.
pub const CHANGE_ID_TRAILER: &str = "Vcs-Change-Id";

#[derive(Debug, Default)]
pub struct ExportReport {
    pub git_ref: String,
    pub commits: usize,
    pub blobs: usize,
    /// `(our commit id, mark)` in stream order.
    pub marks: Vec<(ObjectId, usize)>,
    /// Commits skipped because an earlier export or the import table already has them.
    pub commits_known: usize,
    pub tags: usize,
    /// Tags not exported, with the reason.
    pub tags_skipped: Vec<String>,
    /// Symlink entries written (per modification).
    pub symlinks: usize,
    /// Highest mark used by this run (or `mark_base` when nothing was written).
    pub last_mark: usize,
    /// `(content id, mark)` of the blobs written by this run.
    pub blob_marks: Vec<(ObjectId, usize)>,
}

fn load_table(text: &str) -> BTreeMap<ObjectId, String> {
    text.lines()
        .filter_map(|l| {
            let (h, k) = l.split_once(' ')?;
            Some((ObjectId::from_hex(h).ok()?, k.to_owned()))
        })
        .collect()
}

fn is_sha(k: &str) -> bool {
    matches!(k.len(), 40 | 64) && k.bytes().all(|b| b.is_ascii_hexdigit())
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
        self.export_git_with(zone, branch, out, ExportOptions::default())
    }

    /// Like `export_git`, with incremental export and table recording (see `ExportOptions`).
    pub fn export_git_with(&self, zone: &str, branch: &str, out: &mut impl Write, opts: ExportOptions) -> Result<ExportReport> {
        self.export_git_ext(zone, branch, out, opts, &ExportExt::default())
    }

    /// `export_git_with` plus `ExportExt`.
    pub fn export_git_ext(
        &self,
        zone: &str,
        branch: &str,
        out: &mut impl Write,
        opts: ExportOptions,
        ext: &ExportExt,
    ) -> Result<ExportReport> {
        if !ref_ok(branch) {
            return msg(format!("branch name {branch:?} is not a valid Git ref name"));
        }
        let refs = self.refs()?;
        let head = refs
            .head(zone, branch)
            .ok_or_else(|| Error::Msg(format!("no such branch {zone}/{branch}")))?;
        let read = |name: &str| -> Result<String> {
            let p = self.vcs.join(name);
            match std::fs::read_to_string(&p) {
                Ok(t) => Ok(t),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
                Err(source) => Err(Error::Io { path: p, source }),
            }
        };
        let mut table = if opts.incremental {
            load_table(&read(EXPORT_MAP)?)
        } else {
            BTreeMap::new()
        };
        // known commit -> how a stream refers to it (`:<mark>` or a Git sha)
        let mut known: BTreeMap<ObjectId, String> = BTreeMap::new();
        let mut next = ext.mark_base;
        for (id, k) in &table {
            if let Some(n) = k.strip_prefix("mark:").and_then(|n| n.parse::<usize>().ok()) {
                known.insert(*id, format!(":{n}"));
                next = next.max(n);
            }
        }
        for (id, k) in &ext.known {
            known.insert(*id, k.clone());
        }
        if opts.onto_origin {
            for (id, k) in load_table(&read(IMPORT_MAP)?) {
                if is_sha(&k) {
                    known.entry(id).or_insert(k);
                }
            }
        }
        // parents-first order, deterministic (iterative DFS, first parent first)
        let mut order: Vec<ObjectId> = Vec::new();
        let mut commits: BTreeMap<ObjectId, Commit> = BTreeMap::new();
        let mut done = BTreeSet::new();
        let mut rep_known = 0usize;
        let mut stack = vec![(head, false)];
        while let Some((id, expanded)) = stack.pop() {
            if expanded {
                order.push(id);
                continue;
            }
            if !done.insert(id) {
                continue;
            }
            if known.contains_key(&id) {
                rep_known += 1;
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

        let git_ref = ext.git_ref.clone().unwrap_or_else(|| format!("refs/heads/{branch}"));
        let mut rep = ExportReport {
            git_ref: git_ref.clone(),
            commits_known: rep_known,
            ..Default::default()
        };
        let mut commit_mark: BTreeMap<ObjectId, String> = known.clone();
        let mut blob_mark: BTreeMap<ObjectId, usize> = BTreeMap::new();
        let mut flats: BTreeMap<ObjectId, Flat> = BTreeMap::new();
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
                        rep.blob_marks.push((*bid, next));
                        rep.blobs += 1;
                        next
                    }
                };
                let git_mode = match mode {
                    EntryMode::Executable => "100755",
                    EntryMode::Symlink => "120000",
                    _ => "100644",
                };
                if *mode == EntryMode::Symlink {
                    rep.symlinks += 1;
                }
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
                let pm: String = commit_mark
                    .get(p)
                    .cloned()
                    .ok_or_else(|| Error::Corrupt(format!("parent {p} not exported")))?;
                writeln!(out, "{} {pm}", if i == 0 { "from" } else { "merge" }).map_err(io_err)?;
            }
            out.write_all(&lines).map_err(io_err)?;
            out.write_all(b"\n").map_err(io_err)?;
            commit_mark.insert(id, format!(":{next}"));
            table.insert(id, format!("mark:{next}"));
            rep.marks.push((id, next));
            rep.commits += 1;
            flats.insert(id, flat);
        }
        rep.last_mark = next;
        if !ext.partial {
            self.export_tags(zone, out, &commit_mark, &mut rep, ext.tag_ref_prefix.as_deref())?;
            out.write_all(b"done\n").map_err(io_err)?;
        }
        out.flush().map_err(io_err)?;
        if opts.record {
            let mut t = String::new();
            for (id, k) in &table {
                t.push_str(&format!("{} {k}\n", id.to_hex()));
            }
            write_atomic(&self.vcs.join(EXPORT_MAP), t.as_bytes())?;
        }
        Ok(rep)
    }

    /// Emit the zone's tags whose commit is part of the exported history (or known to the target).
    fn export_tags(
        &self,
        zone: &str,
        out: &mut impl Write,
        refer: &BTreeMap<ObjectId, String>,
        rep: &mut ExportReport,
        prefix: Option<&str>,
    ) -> Result<()> {
        for ((z, name), val) in &self.refs()?.tags {
            if z != zone {
                continue;
            }
            if !ref_ok(name) {
                rep.tags_skipped.push(format!("{name} (not a valid Git ref name)"));
                continue;
            }
            let (commit, ann) = match self.store().get(val)? {
                Object::Tag(t) => (t.target, Some(t)),
                _ => (*val, None),
            };
            let Some(target) = refer.get(&commit) else {
                rep.tags_skipped.push(format!("{name} (commit is not in the exported branch)"));
                continue;
            };
            match ann {
                None => {
                    let r = prefix.map_or_else(|| format!("refs/tags/{name}"), |p| format!("{p}/{name}"));
                    write!(out, "reset {r}\nfrom {target}\n\n").map_err(io_err)?
                }
                Some(_) if prefix.is_some() => {
                    write!(out, "reset {}/{name}\nfrom {target}\n\n", prefix.unwrap_or_default()).map_err(io_err)?
                }
                Some(t) => {
                    let ident = format!(
                        "{} <{}> {} +0000",
                        clean(&t.tagger.name),
                        clean(&t.tagger.email),
                        t.tagger.timestamp
                    );
                    write!(
                        out,
                        "tag {name}\nfrom {target}\ntagger {ident}\ndata {}\n{}\n",
                        t.message.len(),
                        t.message
                    )
                    .map_err(io_err)?;
                }
            }
            rep.tags += 1;
        }
        Ok(())
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
