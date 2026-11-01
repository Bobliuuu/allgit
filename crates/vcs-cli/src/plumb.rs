//! Plumbing basics for scripts: `rev-parse`, `ls-files`, `cat-file`, `shortlog`. All accept the
//! global `--json` (one document with `version` and `command`, see docs/cli.md).

use super::{open, zone_of_cwd};
use crate::json::{doc, J};
use clap::Args;
use std::io::Write;
use vcs_objects::{Object, ObjectId};
use vcs_repo::{Error, Repo};

fn bad<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Msg(m.into()))
}

fn out(json: bool, text: String, j: impl FnOnce() -> J) -> Result<(), Error> {
    if json {
        println!("{}", j().render());
    } else {
        let mut o = std::io::stdout().lock();
        o.write_all(text.as_bytes()).map_err(|source| Error::Io {
            path: "stdout".into(),
            source,
        })?;
    }
    Ok(())
}

// ---------- rev-parse ----------

#[derive(Args, Clone)]
pub struct RevParseArgs {
    /// Revisions to resolve (HEAD, a branch, a tag, a commit id prefix, with ~N or ^)
    pub revs: Vec<String>,
    /// Abbreviate the ids
    #[arg(long)]
    pub short: bool,
    /// Print the absolute path of the working tree root instead
    #[arg(long)]
    pub show_toplevel: bool,
    /// Zone (default: the zone containing the current directory)
    #[arg(long)]
    pub zone: Option<String>,
}

pub fn rev_parse(a: &RevParseArgs, json: bool) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    if a.show_toplevel {
        let p = repo.worktree().display().to_string();
        return out(json, format!("{p}\n"), || doc("rev-parse", vec![("toplevel", J::Str(p.clone()))]));
    }
    if a.revs.is_empty() {
        return bad("usage: vcs rev-parse [--short] <rev>... | --show-toplevel");
    }
    let zone = repo.resolve_zone(&zone_of_cwd(&repo, &cwd, a.zone.clone())?)?;
    let mut ids = Vec::new();
    for r in &a.revs {
        let id = repo.resolve_rev(&zone, r)?;
        ids.push(if a.short { id.short() } else { id.to_hex() });
    }
    out(json, ids.iter().map(|i| format!("{i}\n")).collect(), || {
        doc(
            "rev-parse",
            vec![
                ("zone", J::Str(zone.clone())),
                ("ids", J::Arr(ids.iter().cloned().map(J::Str).collect())),
            ],
        )
    })
}

// ---------- ls-files ----------

#[derive(Args, Clone)]
pub struct LsFilesArgs {
    /// Show mode, object id and path
    #[arg(short = 's', long)]
    pub stage: bool,
    /// List the files of this revision instead of the working tree's tracked files
    #[arg(long)]
    pub rev: Option<String>,
    /// Only these paths (files or directories)
    pub paths: Vec<String>,
}

fn mode_str(m: vcs_objects::EntryMode) -> &'static str {
    match m {
        vcs_objects::EntryMode::Executable => "100755",
        vcs_objects::EntryMode::Symlink => "120000",
        _ => "100644",
    }
}

pub fn ls_files(a: &LsFilesArgs, json: bool) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let filters: Vec<String> = a.paths.iter().map(|p| repo.rel_path(&cwd, p)).collect::<Result<_, _>>()?;
    let keep = |p: &str| {
        filters.is_empty()
            || filters
                .iter()
                .any(|f| f.is_empty() || p == f || p.strip_prefix(f.as_str()).is_some_and(|r| r.starts_with('/')))
    };
    let files: Vec<_> = match &a.rev {
        Some(r) => repo.rev_files(r)?.into_iter().map(|(p, (m, i))| (p, m, i)).collect(),
        None => repo.tracked_files()?,
    };
    let files: Vec<_> = files.into_iter().filter(|(p, _, _)| keep(p)).collect();
    let text: String = files
        .iter()
        .map(|(p, m, i)| {
            if a.stage {
                format!("{} {} {p}\n", mode_str(*m), i.to_hex())
            } else {
                format!("{p}\n")
            }
        })
        .collect();
    out(json, text, || {
        let list = files
            .iter()
            .map(|(p, m, i)| {
                J::Obj(vec![
                    ("path", J::Str(p.clone())),
                    ("mode", J::Str(mode_str(*m).into())),
                    ("id", J::Str(i.to_hex())),
                ])
            })
            .collect();
        doc("ls-files", vec![("files", J::Arr(list))])
    })
}

// ---------- cat-file ----------

#[derive(Args, Clone)]
pub struct CatFileArgs {
    /// An object id (64 hex digits), a revision, or `<rev>:<path>` for a file
    pub object: String,
    /// Print the object's type
    #[arg(short = 't', conflicts_with_all = ["size", "print"])]
    pub type_: bool,
    /// Print the object's size in bytes
    #[arg(short = 's', conflicts_with = "print")]
    pub size: bool,
    /// Print the content (file bytes; for a commit its header and message)
    #[arg(short = 'p')]
    pub print: bool,
    #[arg(long)]
    pub zone: Option<String>,
}

fn locate(repo: &Repo, cwd: &std::path::Path, a: &CatFileArgs) -> Result<ObjectId, Error> {
    if let Some((rev, path)) = a.object.split_once(':') {
        let rel = repo.rel_path(cwd, path)?;
        let files = repo.rev_files(if rev.is_empty() { "HEAD" } else { rev })?;
        return match files.get(&rel) {
            Some((_, id)) => Ok(*id),
            None => bad(format!("{path:?} is not a file in {}", if rev.is_empty() { "HEAD" } else { rev })),
        };
    }
    if a.object.len() == 64 && a.object.bytes().all(|b| b.is_ascii_hexdigit()) {
        return ObjectId::from_hex(&a.object).map_err(|e| Error::Msg(e.to_string()));
    }
    let zone = repo.resolve_zone(&zone_of_cwd(repo, cwd, a.zone.clone())?)?;
    repo.resolve_rev(&zone, &a.object)
}

pub fn cat_file(a: &CatFileArgs, json: bool) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    if !(a.type_ || a.size || a.print) {
        return bad("usage: vcs cat-file (-t | -s | -p) <object>");
    }
    let id = locate(&repo, &cwd, a)?;
    let obj = repo.store().get(&id)?;
    let (kind, bytes): (&str, Vec<u8>) = match &obj {
        Object::Commit(_) => ("commit", commit_text(&obj).into_bytes()),
        Object::Blob(_) | Object::Chunked(_) => ("blob", repo.content(&id)?),
        Object::Tag(_) => ("tag", obj.encode()),
        other => (kind_name(other), obj.encode()),
    };
    if json {
        let mut f = vec![
            ("id", J::Str(id.to_hex())),
            ("type", J::Str(kind.into())),
            ("size", J::Num(bytes.len() as u64)),
        ];
        if a.print {
            match std::str::from_utf8(&bytes) {
                Ok(s) => f.push(("content", J::Str(s.into()))),
                Err(_) => return bad("--json -p needs text content; this object is binary"),
            }
        }
        println!("{}", doc("cat-file", f).render());
        return Ok(());
    }
    let mut o = std::io::stdout().lock();
    let w = |e: std::io::Error| Error::Io {
        path: "stdout".into(),
        source: e,
    };
    if a.type_ {
        writeln!(o, "{kind}").map_err(w)?;
    } else if a.size {
        writeln!(o, "{}", bytes.len()).map_err(w)?;
    } else {
        o.write_all(&bytes).map_err(w)?;
    }
    Ok(())
}

fn kind_name(o: &Object) -> &'static str {
    match o {
        Object::Tree(_) => "tree",
        Object::Snapshot(_) => "snapshot",
        Object::Transaction(_) => "transaction",
        Object::Operation(_) => "operation",
        _ => "object",
    }
}

fn commit_text(o: &Object) -> String {
    let Object::Commit(c) = o else { return String::new() };
    let mut s = format!("zone {}\ntree {}\n", c.zone, c.root.to_hex());
    for p in &c.parents {
        s.push_str(&format!("parent {}\n", p.to_hex()));
    }
    s.push_str(&format!(
        "author {} <{}> {}\n\n{}",
        c.author.name, c.author.email, c.author.timestamp, c.message
    ));
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

// ---------- shortlog ----------

#[derive(Args, Clone)]
pub struct ShortlogArgs {
    /// Branch (default: the checked-out one)
    pub branch: Option<String>,
    /// Only the number of commits per author
    #[arg(short = 's', long)]
    pub summary: bool,
    /// Sort by number of commits instead of by name
    #[arg(short = 'n', long)]
    pub numbered: bool,
    /// Show the author's email
    #[arg(short = 'e', long)]
    pub email: bool,
    #[arg(long)]
    pub zone: Option<String>,
}

pub fn shortlog(a: &ShortlogArgs, json: bool) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = repo.resolve_zone(&zone_of_cwd(&repo, &cwd, a.zone.clone())?)?;
    let log = repo.log(&zone, a.branch.as_deref(), usize::MAX)?;
    let mut groups: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for (_, c) in &log {
        let who = if a.email {
            format!("{} <{}>", c.author.name, c.author.email)
        } else {
            c.author.name.clone()
        };
        groups
            .entry(who)
            .or_default()
            .push(c.message.lines().next().unwrap_or("").to_owned());
    }
    let mut v: Vec<_> = groups.into_iter().collect();
    if a.numbered {
        v.sort_by(|x, y| y.1.len().cmp(&x.1.len()).then_with(|| x.0.cmp(&y.0)));
    }
    let mut text = String::new();
    for (who, subs) in &v {
        if a.summary {
            text.push_str(&format!("{:>6}\t{who}\n", subs.len()));
        } else {
            text.push_str(&format!("{who} ({}):\n", subs.len()));
            for s in subs {
                text.push_str(&format!("      {s}\n"));
            }
            text.push('\n');
        }
    }
    out(json, text, || {
        let authors = v
            .iter()
            .map(|(who, subs)| {
                J::Obj(vec![
                    ("author", J::Str(who.clone())),
                    ("count", J::Num(subs.len() as u64)),
                    ("subjects", J::Arr(subs.iter().cloned().map(J::Str).collect())),
                ])
            })
            .collect();
        doc("shortlog", vec![("zone", J::Str(zone.clone())), ("authors", J::Arr(authors))])
    })
}
