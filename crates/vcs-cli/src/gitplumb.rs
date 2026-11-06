//! The git plumbing commands that have a faithful meaning on the vcs model, run by `vcs git`:
//! `for-each-ref`, `show-ref`, `ls-remote`, `name-rev`, `update-ref`, `check-ignore`,
//! `count-objects`, `hash-object`, `write-tree` and `commit-tree`.
//!
//! Two facts shape every one of them. Object ids are blake3 hashes (64 hex digits), so an id never
//! equals the sha1 git would print for the same content. And refs live per zone, so each command
//! acts on ONE zone (the one containing the current directory) and names its refs the way git does
//! (`refs/heads/<branch>`, `refs/tags/<tag>`, `refs/remotes/origin/<branch>`); `ls-remote` reads the
//! whole remote and names the refs of a zone other than `root` `refs/zones/<zone>/heads/<branch>`.
//! docs/git-to-vcs.md lists what each command prints and what it leaves out.

use super::{open, zone_of_cwd};
use std::collections::HashMap;
use std::io::{Read, Write};
use vcs_objects::ObjectId;
use vcs_repo::{Error, Repo};

fn bad<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Msg(m.into()))
}

/// Leave with status 1 and nothing more to say (git's "no match" exit for show-ref, check-ignore).
fn exit_no_match() -> ! {
    let _ = std::io::stdout().flush();
    std::process::exit(1)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plumb {
    ForEachRef {
        patterns: Vec<String>,
        format: Option<String>,
        sort: Vec<String>,
        count: Option<usize>,
    },
    ShowRef {
        heads: bool,
        tags: bool,
        hash: Option<usize>,
        verify: bool,
        quiet: bool,
        patterns: Vec<String>,
    },
    LsRemote {
        remote: Option<String>,
        heads: bool,
        tags: bool,
        patterns: Vec<String>,
    },
    NameRev {
        revs: Vec<String>,
        name_only: bool,
    },
    UpdateRef {
        name: String,
        new: Option<String>,
        old: Option<String>,
        delete: bool,
    },
    CheckIgnore {
        paths: Vec<String>,
        quiet: bool,
    },
    CountObjects {
        verbose: bool,
    },
    HashObject {
        write: bool,
        stdin: bool,
        files: Vec<String>,
    },
    WriteTree,
    CommitTree {
        tree: String,
        parents: Vec<String>,
        messages: Vec<String>,
        file: Option<String>,
    },
}

impl Plumb {
    /// The canonical text shown in docs/git-to-vcs.md.
    #[cfg(test)]
    pub fn display(&self) -> String {
        match self {
            Plumb::ForEachRef { .. } => "vcs branch + vcs tag (rendered as refs)".into(),
            Plumb::ShowRef { .. } => "vcs branch + vcs tag (rendered as `<id> <refname>`)".into(),
            Plumb::LsRemote { .. } => "vcs remote show (read live from the remote)".into(),
            Plumb::NameRev { .. } => "vcs describe-like search over branches and tags".into(),
            Plumb::UpdateRef { delete: true, .. } => "vcs branch -d / vcs tag -d".into(),
            Plumb::UpdateRef { .. } => "vcs branch <name> <rev> (moves the ref only)".into(),
            Plumb::CheckIgnore { .. } => "vcs .vcsignore match".into(),
            Plumb::CountObjects { .. } => "vcs object store count".into(),
            Plumb::HashObject { write: true, .. } => "vcs object id of file content, stored".into(),
            Plumb::HashObject { .. } => "vcs object id of file content".into(),
            Plumb::WriteTree => "vcs tree id of the index".into(),
            Plumb::CommitTree { .. } => "vcs commit object from a tree".into(),
        }
    }

    pub fn run(&self) -> Result<(), Error> {
        match self {
            Plumb::ForEachRef {
                patterns,
                format,
                sort,
                count,
            } => for_each_ref(patterns, format.as_deref(), sort, *count),
            Plumb::ShowRef {
                heads,
                tags,
                hash,
                verify,
                quiet,
                patterns,
            } => show_ref(*heads, *tags, *hash, *verify, *quiet, patterns),
            Plumb::LsRemote {
                remote,
                heads,
                tags,
                patterns,
            } => ls_remote(remote.as_deref(), *heads, *tags, patterns),
            Plumb::NameRev { revs, name_only } => name_rev(revs, *name_only),
            Plumb::UpdateRef { name, new, old, delete } => update_ref(name, new.as_deref(), old.as_deref(), *delete),
            Plumb::CheckIgnore { paths, quiet } => check_ignore(paths, *quiet),
            Plumb::CountObjects { verbose } => count_objects(*verbose),
            Plumb::HashObject { write, stdin, files } => hash_object(*write, *stdin, files),
            Plumb::WriteTree => write_tree(),
            Plumb::CommitTree {
                tree,
                parents,
                messages,
                file,
            } => commit_tree(tree, parents, messages, file.as_deref()),
        }
    }
}

// ---------------------------------------------------------------- refs

struct RefRow {
    name: String,
    id: ObjectId,
}

/// Branches, tags and remote-tracking branches of `zone`, sorted by full ref name.
fn ref_rows(repo: &Repo, zone: &str) -> Result<Vec<RefRow>, Error> {
    let mut v = Vec::new();
    for (b, id, _) in repo.branches(zone)? {
        if let Some(id) = id {
            v.push(RefRow {
                name: format!("refs/heads/{b}"),
                id,
            });
        }
    }
    for t in repo.tag_list(zone)? {
        v.push(RefRow {
            name: format!("refs/tags/{}", t.name),
            id: t.commit,
        });
    }
    for ((z, b), id) in &repo.remote_refs()?.branches {
        if z == zone {
            v.push(RefRow {
                name: format!("refs/remotes/origin/{b}"),
                id: *id,
            });
        }
    }
    v.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(v)
}

fn wild(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| wild(rest, &s[i..])),
        Some((c, rest)) => s.first() == Some(c) && wild(rest, &s[1..]),
    }
}

/// for-each-ref patterns: a glob, or a name prefix ending at a `/` boundary.
fn prefix_match(name: &str, pat: &str) -> bool {
    if pat.contains('*') {
        wild(pat.as_bytes(), name.as_bytes())
    } else {
        let p = pat.trim_end_matches('/');
        name == p || name.starts_with(&format!("{p}/"))
    }
}

/// show-ref / ls-remote patterns: the pattern matches the end of the ref name, component-wise.
fn tail_match(name: &str, pat: &str) -> bool {
    name == pat || name.ends_with(&format!("/{pat}"))
}

fn short_ref(name: &str) -> &str {
    for p in ["refs/heads/", "refs/tags/", "refs/remotes/"] {
        if let Some(r) = name.strip_prefix(p) {
            return r;
        }
    }
    name
}

fn for_each_ref(patterns: &[String], format: Option<&str>, sort: &[String], count: Option<usize>) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = zone_of_cwd(&repo, &cwd, None)?;
    let head = repo.current_branch(&zone)?;
    let mut rows: Vec<RefRow> = ref_rows(&repo, &zone)?
        .into_iter()
        .filter(|r| patterns.is_empty() || patterns.iter().any(|p| prefix_match(&r.name, p)))
        .collect();
    // the last --sort is the primary key, so apply them first to last (the sort is stable)
    for key in sort {
        let (desc, k) = match key.strip_prefix('-') {
            Some(k) => (true, k),
            None => (false, key.as_str()),
        };
        match k {
            "refname" | "objectname" => {}
            "authordate" | "committerdate" | "creatordate" | "subject" => {}
            other => return bad(format!("for-each-ref --sort={other}: only refname, objectname, authordate, committerdate, creatordate and subject can be sorted on")),
        }
        let mut keyed: Vec<(String, u64, RefRow)> = Vec::new();
        for r in rows {
            let (s, n) = match k {
                "refname" => (r.name.clone(), 0),
                "objectname" => (r.id.to_hex(), 0),
                "subject" => (repo.commit_of(&r.id)?.message.lines().next().unwrap_or("").to_owned(), 0),
                _ => (String::new(), repo.commit_of(&r.id)?.author.timestamp),
            };
            keyed.push((s, n, r));
        }
        keyed.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        if desc {
            keyed.reverse();
        }
        rows = keyed.into_iter().map(|(_, _, r)| r).collect();
    }
    if let Some(n) = count {
        rows.truncate(n);
    }
    let fmt = format.unwrap_or("%(objectname) %(objecttype)\t%(refname)");
    let mut out = String::new();
    for r in &rows {
        let is_head = r.name.strip_prefix("refs/heads/") == head.as_deref();
        out.push_str(&expand(&repo, fmt, r, is_head)?);
        out.push('\n');
    }
    std::io::stdout().lock().write_all(out.as_bytes()).map_err(|source| Error::Io {
        path: "stdout".into(),
        source,
    })
}

fn expand(repo: &Repo, fmt: &str, r: &RefRow, is_head: bool) -> Result<String, Error> {
    let mut out = String::new();
    let mut rest = fmt;
    let mut commit = None;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        if let Some(r2) = rest.strip_prefix('%') {
            out.push('%');
            rest = r2;
        } else if let Some(r2) = rest.strip_prefix("0a") {
            out.push('\n');
            rest = r2;
        } else if let Some(r2) = rest.strip_prefix("00") {
            out.push('\0');
            rest = r2;
        } else if let Some(r2) = rest.strip_prefix('(') {
            let Some(end) = r2.find(')') else {
                return bad("for-each-ref --format: unterminated %(");
            };
            let atom = &r2[..end];
            rest = &r2[end + 1..];
            if commit.is_none()
                && !matches!(
                    atom,
                    "refname" | "refname:short" | "objectname" | "objecttype" | "HEAD" | "upstream"
                )
            {
                commit = Some(repo.commit_of(&r.id)?);
            }
            let field = |f: fn(&vcs_objects::Commit) -> String| commit.as_ref().map(f).unwrap_or_default();
            match atom {
                "refname" => out.push_str(&r.name),
                "refname:short" => out.push_str(short_ref(&r.name)),
                "objectname" => out.push_str(&r.id.to_hex()),
                "objectname:short" => out.push_str(&r.id.short()),
                "objecttype" => out.push_str("commit"),
                "HEAD" => out.push(if is_head { '*' } else { ' ' }),
                "upstream" => {}
                "subject" => out.push_str(&field(|c| c.message.lines().next().unwrap_or("").to_owned())),
                "body" => out.push_str(&field(|c| c.message.lines().skip(1).collect::<Vec<_>>().join("\n").trim().to_owned())),
                "authorname" | "committername" | "creatorname" => out.push_str(&field(|c| c.author.name.clone())),
                "authoremail" | "committeremail" | "creatoremail" => out.push_str(&field(|c| format!("<{}>", c.author.email))),
                "authordate:unix" | "committerdate:unix" | "creatordate:unix" => out.push_str(&field(|c| c.author.timestamp.to_string())),
                other => return bad(format!("for-each-ref --format: unknown field %({other}); known: refname[:short], objectname[:short], objecttype, subject, body, authorname, authoremail, authordate:unix (committer* and creator* answer the same), HEAD")),
            }
        } else {
            out.push('%');
        }
    }
    out.push_str(rest);
    Ok(out)
}

fn show_ref(heads: bool, tags: bool, hash: Option<usize>, verify: bool, quiet: bool, patterns: &[String]) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = zone_of_cwd(&repo, &cwd, None)?;
    let all = ref_rows(&repo, &zone)?;
    let mut out = String::new();
    let mut hits = 0usize;
    if verify {
        if patterns.is_empty() {
            return bad("show-ref --verify needs at least one full ref name");
        }
        for p in patterns {
            match all.iter().find(|r| r.name == *p) {
                Some(r) => {
                    hits += 1;
                    out.push_str(&line(r, hash));
                }
                None => {
                    if !quiet {
                        eprintln!("fatal: '{p}' - not a valid ref");
                    }
                    exit_no_match();
                }
            }
        }
    } else {
        for r in &all {
            let kind_ok = (!heads && !tags) || (heads && r.name.starts_with("refs/heads/")) || (tags && r.name.starts_with("refs/tags/"));
            if kind_ok && (patterns.is_empty() || patterns.iter().any(|p| tail_match(&r.name, p))) {
                hits += 1;
                out.push_str(&line(r, hash));
            }
        }
    }
    if !quiet {
        std::io::stdout().lock().write_all(out.as_bytes()).map_err(|source| Error::Io {
            path: "stdout".into(),
            source,
        })?;
    }
    if hits == 0 {
        exit_no_match();
    }
    Ok(())
}

fn line(r: &RefRow, hash: Option<usize>) -> String {
    let hex = r.id.to_hex();
    match hash {
        Some(0) => format!("{hex}\n"),
        Some(n) => format!("{}\n", &hex[..n.clamp(4, 64)]),
        None => format!("{hex} {}\n", r.name),
    }
}

fn ls_remote(remote: Option<&str>, heads: bool, tags: bool, patterns: &[String]) -> Result<(), Error> {
    let (repo, _) = open()?;
    let url = match remote {
        None | Some("origin") => crate::need_remote(&repo)?,
        Some(u) => u.to_owned(),
    };
    let backend = crate::remote::open_backend(&url)?;
    let (m, _) = vcs_remote::refresh_manifest(backend.as_ref(), None).map_err(|e| Error::Msg(format!("ls-remote {url}: {e}")))?;
    let prefix = |zone: &str| {
        if zone == vcs_repo::ROOT_ZONE {
            "refs".to_owned()
        } else {
            format!("refs/zones/{zone}")
        }
    };
    let mut rows: Vec<RefRow> = Vec::new();
    for ((z, b), id) in &m.branches {
        rows.push(RefRow {
            name: format!("{}/heads/{b}", prefix(z)),
            id: *id,
        });
    }
    for ((z, t), id) in &m.tags {
        rows.push(RefRow {
            name: format!("{}/tags/{t}", prefix(z)),
            id: *id,
        });
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = String::new();
    for r in &rows {
        let is_head = r.name.contains("/heads/");
        let is_tag = r.name.contains("/tags/");
        if ((heads || tags) && !((heads && is_head) || (tags && is_tag)))
            || !(patterns.is_empty() || patterns.iter().any(|p| tail_match(&r.name, p)))
        {
            continue;
        }
        out.push_str(&format!("{}\t{}\n", r.id.to_hex(), r.name));
    }
    std::io::stdout().lock().write_all(out.as_bytes()).map_err(|source| Error::Io {
        path: "stdout".into(),
        source,
    })
}

// ---------------------------------------------------------------- name-rev

#[derive(Clone)]
struct Nm {
    base: String,
    gen: u64,
    dist: u64,
    tag: bool,
    /// The base is an annotated tag's own name and nothing was walked yet: git prints `^0`.
    peeled: bool,
}

/// git's weight of leaving the first-parent line (builtin/name-rev.c MERGE_TRAVERSAL_WEIGHT).
const MERGE_WEIGHT: u64 = 65535;

fn name_rev(revs: &[String], name_only: bool) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = zone_of_cwd(&repo, &cwd, None)?;
    if revs.is_empty() {
        return bad("usage: git name-rev <commit-ish>...");
    }
    let mut map: HashMap<ObjectId, Nm> = HashMap::new();
    let mut stack: Vec<ObjectId> = Vec::new();
    let better = |a: &Nm, b: &Nm| (a.dist, !a.tag) < (b.dist, !b.tag);
    let mut tips: Vec<(ObjectId, Nm)> = Vec::new();
    for (b, id, _) in repo.branches(&zone)? {
        if let Some(id) = id {
            tips.push((
                id,
                Nm {
                    base: b,
                    gen: 0,
                    dist: 0,
                    tag: false,
                    peeled: false,
                },
            ));
        }
    }
    for t in repo.tag_list(&zone)? {
        tips.push((
            t.commit,
            Nm {
                base: format!("tags/{}", t.name),
                gen: 0,
                dist: 0,
                tag: true,
                peeled: t.annotation.is_some(),
            },
        ));
    }
    for (id, nm) in tips {
        if map.get(&id).is_none_or(|o| better(&nm, o)) {
            map.insert(id, nm);
            stack.push(id);
        }
    }
    while let Some(c) = stack.pop() {
        let Some(nm) = map.get(&c).cloned() else { continue };
        for (i, p) in repo.commit_of(&c)?.parents.iter().enumerate() {
            let cand = if i == 0 {
                Nm {
                    base: nm.base.clone(),
                    gen: nm.gen + 1,
                    dist: nm.dist + 1,
                    tag: nm.tag,
                    peeled: false,
                }
            } else {
                let base = if nm.gen == 0 {
                    format!("{}^{}", nm.base, i + 1)
                } else {
                    format!("{}~{}^{}", nm.base, nm.gen, i + 1)
                };
                Nm {
                    base,
                    gen: 0,
                    dist: nm.dist + MERGE_WEIGHT + 1,
                    tag: nm.tag,
                    peeled: false,
                }
            };
            if map.get(p).is_none_or(|o| better(&cand, o)) {
                map.insert(*p, cand);
                stack.push(*p);
            }
        }
    }
    for r in revs {
        let id = rev_or_id(&repo, &zone, r)?;
        let name = match map.get(&id) {
            Some(n) if n.gen == 0 && n.peeled => format!("{}^0", n.base),
            Some(n) if n.gen == 0 => n.base.clone(),
            Some(n) => format!("{}~{}", n.base, n.gen),
            None => "undefined".to_owned(),
        };
        if name_only {
            println!("{name}");
        } else {
            println!("{r} {name}");
        }
    }
    Ok(())
}

/// A revision, or the full 64-digit id of any commit in the store (reachable or not, e.g. from commit-tree).
fn rev_or_id(repo: &Repo, zone: &str, s: &str) -> Result<ObjectId, Error> {
    if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        if let Ok(id) = ObjectId::from_hex(s) {
            if repo.commit_of(&id).is_ok() {
                return Ok(id);
            }
        }
    }
    repo.resolve_rev(zone, s)
}

// ---------------------------------------------------------------- update-ref

fn update_ref(name: &str, new: Option<&str>, old: Option<&str>, delete: bool) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = zone_of_cwd(&repo, &cwd, None)?;
    let zero = |s: &str| !s.is_empty() && s.bytes().all(|b| b == b'0');
    let want_old = |s: &str| -> Result<Option<ObjectId>, Error> {
        if zero(s) {
            Ok(None)
        } else {
            rev_or_id(&repo, &zone, s).map(Some)
        }
    };
    let (kind, short) = if name == "HEAD" {
        ("head", String::new())
    } else if let Some(b) = name.strip_prefix("refs/heads/") {
        ("branch", b.to_owned())
    } else if let Some(t) = name.strip_prefix("refs/tags/") {
        ("tag", t.to_owned())
    } else {
        return bad(format!(
            "update-ref {name}: only HEAD, refs/heads/<branch> and refs/tags/<tag> exist in vcs"
        ));
    };
    if delete {
        return match kind {
            "branch" => {
                if let Some(o) = old {
                    let cur = repo.branches(&zone)?.into_iter().find(|(b, _, _)| *b == short).and_then(|x| x.1);
                    if cur != want_old(o)? {
                        return bad(format!("cannot lock ref {name}: it is not at the expected value"));
                    }
                }
                repo.branch_delete(&zone, &short, true).map(|_| ())
            }
            "tag" => repo.tag_delete(&zone, &short).map(|_| ()),
            _ => bad("update-ref -d HEAD: HEAD cannot be deleted"),
        };
    }
    let Some(new) = new else {
        return bad("usage: git update-ref [-d] <ref> [<new-value> [<old-value>]]");
    };
    let to = rev_or_id(&repo, &zone, new)?;
    match kind {
        "head" => {
            if repo.current_branch(&zone)?.is_none() {
                return bad("update-ref HEAD: HEAD is detached; use `git switch --detach` or `git reset`");
            }
            if let Some(o) = old {
                if repo.head_commit(&zone)? != want_old(o)? {
                    return bad("cannot lock ref HEAD: it is not at the expected value");
                }
            }
            // moves the checked-out branch and leaves the index and files alone, as git does
            repo.reset(&zone, &to, vcs_repo::ResetMode::Soft).map(|_| ())
        }
        "branch" => {
            let expect = match old {
                Some(o) => Some(want_old(o)?),
                None => None,
            };
            repo.branch_set(&zone, &short, &to, expect).map(|_| ())
        }
        _ => {
            let cur = repo.tag_list(&zone)?.into_iter().find(|t| t.name == short).map(|t| t.commit);
            if let Some(o) = old {
                if cur != want_old(o)? {
                    return bad(format!("cannot lock ref {name}: it is not at the expected value"));
                }
            }
            if cur == Some(to) {
                return Ok(());
            }
            if cur.is_some() {
                repo.tag_delete(&zone, &short)?;
            }
            repo.tag_create(&zone, &short, Some(&to.to_hex()), None).map(|_| ())
        }
    }
}

// ---------------------------------------------------------------- the rest

fn check_ignore(paths: &[String], quiet: bool) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    if paths.is_empty() {
        return bad("usage: git check-ignore <pathname>...");
    }
    let mut any = false;
    for p in paths {
        let rel = repo.rel_path(&cwd, p)?;
        let is_dir = cwd.join(p).is_dir();
        if repo.is_ignored(&rel, is_dir)? {
            any = true;
            if !quiet {
                println!("{p}");
            }
        }
    }
    if !any {
        exit_no_match();
    }
    Ok(())
}

fn count_objects(verbose: bool) -> Result<(), Error> {
    let (repo, _) = open()?;
    let s = repo.object_stats()?;
    let kb = |b: u64| b.div_ceil(1024);
    if verbose {
        println!("count: {}", s.loose);
        println!("size: {}", kb(s.loose_bytes));
        println!("packs: {}", s.packs);
        println!("size-pack: {}", kb(s.pack_bytes));
    } else {
        println!("{} objects, {} kilobytes", s.loose, kb(s.loose_bytes));
    }
    Ok(())
}

fn hash_object(write: bool, stdin: bool, files: &[String]) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let mut inputs: Vec<Vec<u8>> = Vec::new();
    if stdin {
        let mut b = Vec::new();
        std::io::stdin().lock().read_to_end(&mut b).map_err(|source| Error::Io {
            path: "stdin".into(),
            source,
        })?;
        inputs.push(b);
    }
    for f in files {
        let p = cwd.join(f);
        inputs.push(std::fs::read(&p).map_err(|source| Error::Io { path: p, source })?);
    }
    if inputs.is_empty() {
        return bad("usage: git hash-object [-w] (--stdin | <file>...)");
    }
    for data in inputs {
        let id = if write {
            repo.store_content(&data)?
        } else {
            vcs_objects::content_id(&data)
        };
        println!("{}", id.to_hex());
    }
    Ok(())
}

fn write_tree() -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = zone_of_cwd(&repo, &cwd, None)?;
    match repo.write_tree(&zone)? {
        Some(id) => {
            println!("{}", id.to_hex());
            Ok(())
        }
        None => bad(format!(
            "write-tree: zone {zone:?} has nothing staged and no commits, so there is no tree"
        )),
    }
}

fn commit_tree(tree: &str, parents: &[String], messages: &[String], file: Option<&str>) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    let zone = zone_of_cwd(&repo, &cwd, None)?;
    let tree = ObjectId::from_hex(tree).map_err(|_| {
        Error::Msg(format!(
            "commit-tree: {tree:?} is not a full tree id (the output of `git write-tree`)"
        ))
    })?;
    let parents: Vec<ObjectId> = parents.iter().map(|p| rev_or_id(&repo, &zone, p)).collect::<Result<_, _>>()?;
    let message = if !messages.is_empty() {
        messages.join("\n\n")
    } else if let Some(f) = file {
        let p = cwd.join(f);
        std::fs::read_to_string(&p).map_err(|source| Error::Io { path: p, source })?
    } else {
        let mut s = String::new();
        std::io::stdin().lock().read_to_string(&mut s).map_err(|source| Error::Io {
            path: "stdin".into(),
            source,
        })?;
        s
    };
    let id = repo.commit_tree(&zone, &tree, &parents, message.trim_end(), crate::author()?)?;
    println!("{}", id.to_hex());
    Ok(())
}
