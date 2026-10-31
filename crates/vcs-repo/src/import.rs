//! Import a `git fast-export` stream into the root zone.
//!
//! The importer never runs `git`; it parses the stream (see `git-fast-import(1)` for the
//! format) and writes blobs, trees and commits through the normal object store.
//!
//! * Each `refs/heads/<name>` becomes branch `<name>` of the root zone. Other refs (tags,
//!   remotes, notes) are counted and skipped.
//! * Id mapping: commits and blobs are keyed by Git's `original-oid` (stream produced with
//!   `--show-original-ids`) or, lacking one, by `mark:<n>`. The table lives in `.vcs/git-import`.
//!   Mark keys are only meaningful when the same producer keeps its marks stable.
//! * Re-import is incremental: mapped commits are skipped, and since every id is derived
//!   from content (the change id is derived from the Git key), re-running writes nothing new.
//! * Symlinks, submodules and paths vcs cannot represent are skipped and reported.

use crate::error::{msg, Error, Result};
use crate::meta::{write_atomic, ROOT_ZONE};
use crate::repo::Repo;
use crate::treeops::{self, Flat};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{BufRead, Read};
use vcs_objects::names::{validate_branch_name, validate_path};
use vcs_objects::{ChangeId, Commit, EntryMode, Object, ObjectId, RefUpdate, Signature, Transaction};
use vcs_store::ObjectStore;

const MAP_FILE: &str = "git-import";
/// Longest header/command line accepted (paths are <= 4096 bytes, quoted at most 4x).
const MAX_LINE: u64 = 64 * 1024;
/// Largest single `data` block (one blob or one message). The block is held in memory.
const MAX_DATA: u64 = 1 << 30;

#[derive(Debug, Default)]
pub struct ImportReport {
    pub commits_imported: usize,
    pub commits_skipped: usize,
    pub blobs: usize,
    /// Objects that were not already in the store.
    pub objects_written: usize,
    /// (branch, new head)
    pub branches_updated: Vec<(String, ObjectId)>,
    pub refs_ignored: Vec<String>,
    pub paths_skipped: Vec<String>,
}

enum Op {
    Modify { mode: u32, dataref: DataRef, path: Vec<u8> },
    Delete(Vec<u8>),
    Copy(Vec<u8>, Vec<u8>),
    Rename(Vec<u8>, Vec<u8>),
    DeleteAll,
}

enum DataRef {
    Mark(String),
    Sha(String),
    Inline(Vec<u8>),
}

struct GitCommit {
    refname: String,
    mark: Option<String>,
    oid: Option<String>,
    signature: Option<Signature>,
    message: Vec<u8>,
    from: Option<String>,
    merges: Vec<String>,
    ops: Vec<Op>,
}

struct Parser<R: BufRead> {
    r: R,
    peeked: Option<Vec<u8>>,
}

impl<R: BufRead> Parser<R> {
    fn raw_line(&mut self) -> Result<Option<Vec<u8>>> {
        let mut buf = Vec::new();
        // bounded: a stream without newlines must not grow one line without limit
        let n = (&mut self.r)
            .take(MAX_LINE + 1)
            .read_until(b'\n', &mut buf)
            .map_err(|source| Error::Io {
                path: "<stream>".into(),
                source,
            })?;
        if n == 0 {
            return Ok(None);
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
        } else if n as u64 > MAX_LINE {
            return msg("stream line too long");
        }
        Ok(Some(buf))
    }
    fn next(&mut self) -> Result<Option<Vec<u8>>> {
        match self.peeked.take() {
            Some(l) => Ok(Some(l)),
            None => self.raw_line(),
        }
    }
    fn peek(&mut self) -> Result<Option<&[u8]>> {
        if self.peeked.is_none() {
            self.peeked = self.raw_line()?;
        }
        Ok(self.peeked.as_deref())
    }
    fn peek_starts(&mut self, p: &str) -> Result<bool> {
        Ok(self.peek()?.is_some_and(|l| l.starts_with(p.as_bytes())))
    }
    /// Parse a `data` command whose header line is `line`.
    fn data(&mut self, line: &[u8]) -> Result<Vec<u8>> {
        let spec = line.strip_prefix(b"data ").ok_or_else(|| Error::Msg("expected `data`".into()))?;
        if let Some(delim) = spec.strip_prefix(b"<<") {
            let mut out = Vec::new();
            loop {
                let Some(l) = self.raw_line()? else {
                    return msg("unterminated delimited data");
                };
                if l == delim {
                    return Ok(out);
                }
                if out.len() as u64 + l.len() as u64 + 1 > MAX_DATA {
                    return msg("data block too large");
                }
                out.extend_from_slice(&l);
                out.push(b'\n');
            }
        }
        let n: usize = std::str::from_utf8(spec)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| Error::Msg("bad data length".into()))?;
        if n as u64 > MAX_DATA {
            return msg("data block too large");
        }
        // never trust the length for preallocation
        let mut out = Vec::new();
        let got = (&mut self.r).take(n as u64).read_to_end(&mut out).map_err(|source| Error::Io {
            path: "<stream>".into(),
            source,
        })?;
        if got != n {
            return msg("stream ended inside a data block");
        }
        // the LF after a data block is optional
        if self.peek()? == Some(&b""[..]) {
            self.peeked = None;
        }
        Ok(out)
    }
}

fn s(b: &[u8]) -> Result<&str> {
    std::str::from_utf8(b).map_err(|_| Error::Msg("non-UTF-8 text in stream header".into()))
}

/// Parse `Name <email> 123 +0000` (name may be empty).
fn parse_sig(rest: &[u8]) -> Result<Signature> {
    let rest = String::from_utf8_lossy(rest).into_owned();
    let (Some(lt), Some(gt)) = (rest.find('<'), rest.rfind('>')) else {
        return msg(format!("bad identity line {rest:?}"));
    };
    if gt < lt {
        return msg(format!("bad identity line {rest:?}"));
    }
    let name = rest[..lt].trim().to_owned();
    let email = rest[lt + 1..gt].to_owned();
    let timestamp = rest[gt + 1..]
        .split_whitespace()
        .next()
        .and_then(|t| t.parse().ok())
        .ok_or_else(|| Error::Msg(format!("bad timestamp in {rest:?}")))?;
    Ok(Signature { name, email, timestamp })
}

/// Parse a path operand; supports C-style quoting. Returns (path, rest-after-it).
fn parse_path(b: &[u8], last: bool) -> Result<(Vec<u8>, &[u8])> {
    if b.first() == Some(&b'"') {
        let mut out = Vec::new();
        let mut i = 1;
        while i < b.len() {
            match b[i] {
                b'"' => return Ok((out, b[i + 1..].strip_prefix(b" ").unwrap_or(&b[i + 1..]))),
                b'\\' => {
                    i += 1;
                    let c = *b.get(i).ok_or_else(|| Error::Msg("bad escape".into()))?;
                    match c {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'a' => out.push(7),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'v' => out.push(11),
                        b'0'..=b'7' => {
                            let mut v = 0u32;
                            let mut k = 0;
                            while k < 3 && i < b.len() && (b'0'..=b'7').contains(&b[i]) {
                                v = v * 8 + (b[i] - b'0') as u32;
                                i += 1;
                                k += 1;
                            }
                            out.push(v as u8);
                            continue;
                        }
                        other => out.push(other),
                    }
                }
                c => out.push(c),
            }
            i += 1;
        }
        return msg("unterminated quoted path");
    }
    if last {
        return Ok((b.to_vec(), &[]));
    }
    match b.iter().position(|&c| c == b' ') {
        Some(p) => Ok((b[..p].to_vec(), &b[p + 1..])),
        None => msg("missing second path"),
    }
}

fn parse_commit<R: BufRead>(p: &mut Parser<R>, refname: String) -> Result<GitCommit> {
    let mut c = GitCommit {
        refname,
        mark: None,
        oid: None,
        signature: None,
        message: Vec::new(),
        from: None,
        merges: Vec::new(),
        ops: Vec::new(),
    };
    let mut committer = None;
    let mut have_msg = false;
    while let Some(line) = p.next()? {
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix(b"mark ") {
            c.mark = Some(s(v)?.trim().to_owned());
        } else if let Some(v) = line.strip_prefix(b"original-oid ") {
            c.oid = Some(s(v)?.trim().to_owned());
        } else if let Some(v) = line.strip_prefix(b"author ") {
            c.signature = Some(parse_sig(v)?);
        } else if let Some(v) = line.strip_prefix(b"committer ") {
            committer = Some(parse_sig(v)?);
        } else if line.starts_with(b"encoding ") {
        } else if line.starts_with(b"data ") {
            c.message = p.data(&line)?;
            have_msg = true;
        } else if let Some(v) = line.strip_prefix(b"from ") {
            c.from = Some(s(v)?.trim().to_owned());
        } else if let Some(v) = line.strip_prefix(b"merge ") {
            c.merges.push(s(v)?.trim().to_owned());
        } else if line == b"deleteall" {
            c.ops.push(Op::DeleteAll);
        } else if let Some(v) = line.strip_prefix(b"M ") {
            let sp = v.iter().position(|&x| x == b' ').ok_or_else(|| Error::Msg("bad M line".into()))?;
            let mode = u32::from_str_radix(s(&v[..sp])?, 8).map_err(|_| Error::Msg("bad file mode".into()))?;
            let rest = &v[sp + 1..];
            let sp2 = rest
                .iter()
                .position(|&x| x == b' ')
                .ok_or_else(|| Error::Msg("bad M line".into()))?;
            let dref = s(&rest[..sp2])?;
            let (path, _) = parse_path(&rest[sp2 + 1..], true)?;
            let dataref = if dref == "inline" {
                let Some(dl) = p.next()? else {
                    return msg("missing inline data");
                };
                DataRef::Inline(p.data(&dl)?)
            } else if dref.starts_with(':') {
                DataRef::Mark(dref.to_owned())
            } else {
                DataRef::Sha(dref.to_owned())
            };
            c.ops.push(Op::Modify { mode, dataref, path });
        } else if let Some(v) = line.strip_prefix(b"D ") {
            c.ops.push(Op::Delete(parse_path(v, true)?.0));
        } else if let Some(v) = line.strip_prefix(b"C ") {
            let (a, rest) = parse_path(v, false)?;
            c.ops.push(Op::Copy(a, parse_path(rest, true)?.0));
        } else if let Some(v) = line.strip_prefix(b"R ") {
            let (a, rest) = parse_path(v, false)?;
            c.ops.push(Op::Rename(a, parse_path(rest, true)?.0));
        } else if line.starts_with(b"N ") {
            // notes are not imported; consume inline data if present
            if line.windows(7).any(|w| w == b" inline") {
                let Some(dl) = p.next()? else {
                    return msg("missing inline data");
                };
                p.data(&dl)?;
            }
        } else {
            // start of the next top-level command
            p.peeked = Some(line);
            break;
        }
    }
    if !have_msg {
        return msg("commit without message data");
    }
    if c.signature.is_none() {
        c.signature = committer;
    }
    if c.signature.is_none() {
        return msg("commit without author or committer");
    }
    Ok(c)
}

fn git_change_id(key: &str) -> ChangeId {
    let mut h = blake3::Hasher::new_derive_key("all-git 2026 git import change id v1");
    h.update(key.as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&h.finalize().as_bytes()[..16]);
    ChangeId::from_bytes(b)
}

struct Importer<'a> {
    repo: &'a Repo,
    map: BTreeMap<String, ObjectId>,
    marks: HashMap<String, ObjectId>, // ":n" -> blob or commit id
    tips: HashMap<String, ObjectId>,  // refname -> commit
    flat_cache: Option<(ObjectId, Flat)>,
    report: ImportReport,
    new_keys: usize,
}

impl Importer<'_> {
    fn put(&mut self, o: &Object) -> Result<ObjectId> {
        let id = o.id();
        if !self.repo.store.contains(&id)? {
            self.report.objects_written += 1;
        }
        self.repo.store.put(o)?;
        Ok(id)
    }

    fn put_content(&mut self, data: &[u8]) -> Result<ObjectId> {
        let id = vcs_objects::content_id(data);
        if !self.repo.store.contains(&id)? {
            self.report.objects_written += 1;
        }
        self.repo.store.put_content(data)?;
        Ok(id)
    }

    fn resolve(&self, r: &str) -> Result<ObjectId> {
        if let Some(id) = self.marks.get(r) {
            return Ok(*id);
        }
        if let Some(id) = self.map.get(r) {
            return Ok(*id);
        }
        if r.starts_with(':') {
            if let Some(id) = self.map.get(&format!("mark{r}")) {
                return Ok(*id);
            }
        }
        msg(format!("stream references unknown object {r:?}"))
    }

    fn flat_of(&mut self, commit: &ObjectId) -> Result<Flat> {
        if let Some((id, f)) = &self.flat_cache {
            if id == commit {
                return Ok(f.clone());
            }
        }
        let root = self.repo.commit_obj(commit)?.root;
        treeops::flatten(&self.repo.store, Some(root), "")
    }

    fn blob(&mut self, p: &mut Parser<impl BufRead>, header: &[u8]) -> Result<()> {
        let _ = header;
        let mut mark = None;
        let mut oid = None;
        let data = loop {
            let Some(l) = p.next()? else {
                return msg("blob without data");
            };
            if let Some(v) = l.strip_prefix(b"mark ") {
                mark = Some(s(v)?.trim().to_owned());
            } else if let Some(v) = l.strip_prefix(b"original-oid ") {
                oid = Some(s(v)?.trim().to_owned());
            } else if l.starts_with(b"data ") {
                break p.data(&l)?;
            } else {
                return msg("unexpected line in blob");
            }
        };
        let id = self.put_content(&data)?;
        self.report.blobs += 1;
        if let Some(m) = mark {
            self.marks.insert(format!(":{}", m.trim_start_matches(':')), id);
        }
        if let Some(o) = oid {
            self.map.insert(o, id);
        }
        Ok(())
    }

    fn commit(&mut self, gc: GitCommit) -> Result<()> {
        let mark_key = gc.mark.as_ref().map(|m| format!(":{}", m.trim_start_matches(':')));
        let key = gc.oid.clone().or_else(|| mark_key.as_ref().map(|m| format!("mark{m}")));
        if let Some(existing) = key.as_ref().and_then(|k| self.map.get(k)).copied() {
            if self.repo.store.contains(&existing)? {
                if let Some(m) = &mark_key {
                    self.marks.insert(m.clone(), existing);
                }
                self.tips.insert(gc.refname, existing);
                self.report.commits_skipped += 1;
                return Ok(());
            }
        }
        let mut parents: Vec<ObjectId> = Vec::new();
        match &gc.from {
            Some(f) => parents.push(self.resolve(f)?),
            None => {
                if let Some(t) = self.tips.get(&gc.refname) {
                    parents.push(*t);
                }
            }
        }
        for m in &gc.merges {
            let id = self.resolve(m)?;
            if !parents.contains(&id) {
                parents.push(id);
            }
        }
        let mut flat = match parents.first() {
            Some(p) => self.flat_of(p)?,
            None => Flat::new(),
        };
        for op in &gc.ops {
            self.apply(&mut flat, op)?;
        }
        let root = treeops::build(&self.repo.store, flat.iter().map(|(p, (m, id))| (p.as_str(), *m, *id)))?;
        let author = gc.signature.clone().expect("checked in parse");
        let message = String::from_utf8_lossy(&gc.message).into_owned();
        let change_id = match &key {
            Some(k) => git_change_id(k),
            None => {
                let mut h = blake3::Hasher::new();
                h.update(root.as_bytes());
                for p in &parents {
                    h.update(p.as_bytes());
                }
                h.update(author.name.as_bytes());
                h.update(&author.timestamp.to_le_bytes());
                h.update(message.as_bytes());
                git_change_id(&h.finalize().to_hex())
            }
        };
        let commit = Object::Commit(Commit {
            zone: ROOT_ZONE.into(),
            parents,
            root,
            change_id,
            author,
            message,
        });
        // The stream is untrusted: refuse a commit (oversized message or identity, too many
        // parents) that the strict decoder would later reject, instead of storing it.
        if Object::decode(&commit.encode()).is_err() {
            return msg("commit exceeds object limits (message, identity or parent count)");
        }
        let id = self.put(&commit)?;
        if let Some(m) = mark_key {
            self.marks.insert(m, id);
        }
        if let Some(k) = key {
            self.map.insert(k, id);
            self.new_keys += 1;
        }
        self.flat_cache = Some((id, flat));
        self.tips.insert(gc.refname, id);
        self.report.commits_imported += 1;
        Ok(())
    }

    fn path(&mut self, raw: &[u8]) -> Option<String> {
        let p = match std::str::from_utf8(raw) {
            Ok(p) if validate_path(p).is_ok() => p.to_owned(),
            _ => {
                self.report.paths_skipped.push(String::from_utf8_lossy(raw).into_owned());
                return None;
            }
        };
        Some(p)
    }

    fn apply(&mut self, flat: &mut Flat, op: &Op) -> Result<()> {
        match op {
            Op::DeleteAll => flat.clear(),
            Op::Delete(p) => {
                if let Some(p) = std::str::from_utf8(p).ok().map(str::to_owned) {
                    remove_prefix(flat, &p);
                }
            }
            Op::Copy(a, b) | Op::Rename(a, b) => {
                let (Ok(a), Some(b)) = (std::str::from_utf8(a), self.path(b)) else {
                    return Ok(());
                };
                let moved: Vec<(String, (EntryMode, ObjectId))> = flat
                    .iter()
                    .filter(|(k, _)| k.as_str() == a || k.strip_prefix(a).is_some_and(|r| r.starts_with('/')))
                    .map(|(k, v)| (format!("{b}{}", &k[a.len()..]), *v))
                    .collect();
                if matches!(op, Op::Rename(..)) {
                    remove_prefix(flat, a);
                }
                flat.extend(moved);
            }
            Op::Modify { mode, dataref, path } => {
                let Some(p) = self.path(path) else { return Ok(()) };
                let m = match mode {
                    0o100644 | 0o644 => EntryMode::File,
                    0o100755 | 0o755 => EntryMode::Executable,
                    _ => {
                        // symlink (120000), gitlink (160000), tree (040000)
                        self.report.paths_skipped.push(format!("{p} (mode {mode:o})"));
                        remove_prefix(flat, &p);
                        return Ok(());
                    }
                };
                let id = match dataref {
                    DataRef::Inline(d) => self.put_content(d)?,
                    DataRef::Mark(r) | DataRef::Sha(r) => self.resolve(r)?,
                };
                remove_prefix(flat, &p); // file replaces a directory of the same name
                flat.insert(p, (m, id));
            }
        }
        Ok(())
    }
}

fn remove_prefix(flat: &mut Flat, p: &str) {
    let dir = format!("{p}/");
    flat.retain(|k, _| k != p && !k.starts_with(&dir));
}

fn load_map(text: &str) -> Result<BTreeMap<String, ObjectId>> {
    let mut m = BTreeMap::new();
    for l in text.lines() {
        let (hex, key) = l.split_once(' ').ok_or_else(|| Error::Corrupt("git-import: bad line".into()))?;
        let id = ObjectId::from_hex(hex).map_err(|_| Error::Corrupt("git-import: bad id".into()))?;
        m.insert(key.to_owned(), id);
    }
    Ok(m)
}

impl Repo {
    /// Import a fast-export stream. See the module docs.
    pub fn import_git(&self, input: impl BufRead) -> Result<ImportReport> {
        let _l = self.lock()?;
        let mp = self.vcs.join(MAP_FILE);
        let map = match fs::read_to_string(&mp) {
            Ok(t) => load_map(&t)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(source) => return Err(Error::Io { path: mp, source }),
        };
        let mut imp = Importer {
            repo: self,
            map,
            marks: HashMap::new(),
            tips: HashMap::new(),
            flat_cache: None,
            report: ImportReport::default(),
            new_keys: 0,
        };
        let mut p = Parser { r: input, peeked: None };
        let mut ignored = BTreeSet::new();
        while let Some(line) = p.next()? {
            if line.is_empty() || line.starts_with(b"#") {
                continue;
            }
            if line == b"done" {
                break;
            }
            if line.starts_with(b"blob") && line.len() == 4 {
                imp.blob(&mut p, &line)?;
            } else if let Some(r) = line.strip_prefix(b"commit ") {
                let gc = parse_commit(&mut p, s(r)?.trim().to_owned())?;
                imp.commit(gc)?;
            } else if let Some(r) = line.strip_prefix(b"reset ") {
                let r = s(r)?.trim().to_owned();
                if p.peek_starts("from ")? {
                    let l = p.next()?.expect("peeked");
                    let id = imp.resolve(s(&l[5..])?.trim())?;
                    imp.tips.insert(r, id);
                } else {
                    imp.tips.remove(&r);
                }
            } else if line.starts_with(b"tag ") {
                ignored.insert(String::from_utf8_lossy(&line[4..]).into_owned());
                while let Some(l) = p.next()? {
                    if l.starts_with(b"data ") {
                        p.data(&l)?;
                        break;
                    }
                }
            } else if line.starts_with(b"progress ")
                || line.starts_with(b"feature ")
                || line.starts_with(b"option ")
                || line == b"checkpoint"
            {
            } else {
                return msg(format!("unsupported stream command {:?}", String::from_utf8_lossy(&line)));
            }
        }

        // Decide ref updates.
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut refs = self.refs()?;
        let checked_out = ws.subs.get(ROOT_ZONE).cloned();
        let mut updates = Vec::new();
        let mut tips: Vec<(&String, &ObjectId)> = imp.tips.iter().collect();
        tips.sort();
        for (r, id) in tips {
            let Some(branch) = r.strip_prefix("refs/heads/") else {
                ignored.insert(r.clone());
                continue;
            };
            validate_branch_name(branch)?;
            let old = refs.head(ROOT_ZONE, branch);
            if old == Some(*id) {
                continue;
            }
            if let Some(o) = old {
                if checked_out.as_deref() == Some(branch) {
                    return msg(format!(
                        "branch {branch:?} is checked out and already has history; check out another branch first"
                    ));
                }
                if !is_ancestor(self, &o, id)? {
                    return msg(format!("branch {branch:?} has local history that is not part of the import"));
                }
            }
            updates.push(RefUpdate {
                zone: ROOT_ZONE.into(),
                branch: branch.to_owned(),
                old,
                new: *id,
            });
        }
        updates.sort();

        // Materialize an unborn checked-out branch so status stays clean.
        let mut ix = None;
        if let Some(u) = updates
            .iter()
            .find(|u| u.old.is_none() && checked_out.as_deref() == Some(u.branch.as_str()))
        {
            let st = self.status()?;
            self.require_clean(ROOT_ZONE, &st)?;
            let new = treeops::flatten(&self.store, Some(self.commit_obj(&u.new)?.root), "")?;
            let mut i = self.index()?;
            let old = self.head_flat(ROOT_ZONE, &zones, &ws, &refs)?;
            self.transition(ROOT_ZONE, &zones, &old, &new, &mut i)?;
            ix = Some(i);
        }

        self.store.sync()?;
        if imp.new_keys > 0 {
            let mut t = String::new();
            for (k, id) in &imp.map {
                t.push_str(&format!("{} {k}\n", id.to_hex()));
            }
            write_atomic(&mp, t.as_bytes())?;
        }
        let mut report = imp.report;
        if !updates.is_empty() {
            let author = Signature {
                name: "vcs import-git".into(),
                email: "import@localhost".into(),
                timestamp: updates
                    .iter()
                    .map(|u| self.commit_obj(&u.new).map(|c| c.author.timestamp))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .max()
                    .unwrap_or(0),
            };
            let message = format!("import git history ({} branch(es))", updates.len());
            self.store.put(&Object::Transaction(Transaction {
                updates: updates.clone(),
                author: author.clone(),
                message: message.clone(),
                lock_ops: vec![],
            }))?;
            refs.apply(&updates)?;
            self.store.sync()?;
            if let Some(mut i) = ix {
                self.save_index(&mut i)?;
            }
            write_atomic(&self.vcs.join("refs"), &refs.encode())?;
            self.record_op(&message, Some(author))?;
            report.branches_updated = updates.iter().map(|u| (u.branch.clone(), u.new)).collect();
        }
        report.refs_ignored = ignored.into_iter().collect();
        Ok(report)
    }
}

pub(crate) fn is_ancestor(repo: &Repo, anc: &ObjectId, of: &ObjectId) -> Result<bool> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![*of];
    while let Some(c) = stack.pop() {
        if &c == anc {
            return Ok(true);
        }
        if seen.insert(c) {
            stack.extend(repo.commit_obj(&c)?.parents);
        }
    }
    Ok(false)
}
