//! `git-remote-allgit`: a Git remote helper (see gitremote-helpers(7)) for allgit repositories.
//!
//! `git clone allgit::<path>` / `git fetch` use the `import` capability: this program writes a
//! fast-import stream (`Repo::export_git_ext`) that Git feeds to its own `git fast-import`.
//! `git push` uses `export`: Git's `fast-export` stream is read from stdin (`Repo::import_git_with`).
//! The helper never runs `git` or links libgit2; it is driven by Git. Only the root zone is mapped.
//! Limits are documented in `docs/interop.md`.
//!
//! State lives in `$GIT_DIR/allgit/<remote>/`:
//! * `marks`   shared fast-import / fast-export marks file (written by Git, `:<n> <sha>`)
//! * `pending` `<c|b> <allgit id> :<n>` objects whose Git sha is not yet known (the marks file is
//!   written after this process may have exited, so it is read on the next run)
//! * `map`     `<c|b> <allgit id> <git sha>` for commits and blobs known to both sides
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use vcs_objects::ObjectId;
use vcs_repo::{ExportExt, ExportOptions, ImportOptions, Repo, ROOT_ZONE};

type Res<T> = Result<T, String>;

fn e<E: std::fmt::Display>(x: E) -> String {
    x.to_string()
}

struct State {
    dir: PathBuf,
    /// commit id <-> git sha
    commits: BTreeMap<ObjectId, String>,
    /// blob content id <-> git sha
    blobs: BTreeMap<ObjectId, String>,
    /// objects waiting for their sha: (kind, id, mark)
    pending: Vec<(char, ObjectId, usize)>,
    /// `:n` -> sha as written by Git
    marks: BTreeMap<usize, String>,
}

fn read_or_empty(p: &Path) -> Res<String> {
    match fs::read_to_string(p) {
        Ok(t) => Ok(t),
        Err(x) if x.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(x) => Err(format!("{}: {x}", p.display())),
    }
}

impl State {
    fn load(dir: PathBuf) -> Res<State> {
        fs::create_dir_all(&dir).map_err(|x| format!("{}: {x}", dir.display()))?;
        let mut st = State {
            dir,
            commits: BTreeMap::new(),
            blobs: BTreeMap::new(),
            pending: Vec::new(),
            marks: BTreeMap::new(),
        };
        for l in read_or_empty(&st.dir.join("marks"))?.lines() {
            if let Some((m, sha)) = l.strip_prefix(':').and_then(|r| r.split_once(' ')) {
                if let Ok(n) = m.parse() {
                    st.marks.insert(n, sha.trim().to_owned());
                }
            }
        }
        for l in read_or_empty(&st.dir.join("map"))?.lines() {
            let f: Vec<&str> = l.split(' ').collect();
            if let [k, id, sha] = f[..] {
                if let Ok(id) = ObjectId::from_hex(id) {
                    match k {
                        "c" => st.commits.insert(id, sha.to_owned()),
                        _ => st.blobs.insert(id, sha.to_owned()),
                    };
                }
            }
        }
        let mut keep = Vec::new();
        let mut learned = String::new();
        for l in read_or_empty(&st.dir.join("pending"))?.lines() {
            let f: Vec<&str> = l.split(' ').collect();
            let [k, id, m] = f[..] else { continue };
            let (Ok(id), Some(n)) = (ObjectId::from_hex(id), m.strip_prefix(':').and_then(|n| n.parse::<usize>().ok())) else {
                continue;
            };
            let kind = k.chars().next().unwrap_or('c');
            match st.marks.get(&n) {
                Some(sha) => {
                    learned.push_str(&format!("{kind} {} {sha}\n", id.to_hex()));
                    if kind == 'c' {
                        st.commits.insert(id, sha.clone());
                    } else {
                        st.blobs.insert(id, sha.clone());
                    }
                }
                None => keep.push((kind, id, n)),
            }
        }
        if !learned.is_empty() {
            let mut f = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(st.dir.join("map"))
                .map_err(e)?;
            f.write_all(learned.as_bytes()).map_err(e)?;
            st.pending = keep;
            st.save_pending()?;
        } else {
            st.pending = keep;
        }
        Ok(st)
    }

    fn save_pending(&self) -> Res<()> {
        let mut t = String::new();
        for (k, id, n) in &self.pending {
            t.push_str(&format!("{k} {} :{n}\n", id.to_hex()));
        }
        fs::write(self.dir.join("pending"), t).map_err(e)
    }

    fn add_pending(&mut self, items: impl IntoIterator<Item = (char, ObjectId, usize)>) -> Res<()> {
        self.pending.extend(items);
        self.save_pending()
    }

    fn max_mark(&self) -> usize {
        let a = self.marks.keys().next_back().copied().unwrap_or(0);
        let b = self.pending.iter().map(|p| p.2).max().unwrap_or(0);
        a.max(b)
    }

    /// Keys a pushed stream may use for objects Git already has: `:<mark>` and the sha itself.
    fn known_for_import(&self) -> BTreeMap<String, ObjectId> {
        let mut by_sha: BTreeMap<&str, ObjectId> = BTreeMap::new();
        for (id, sha) in self.commits.iter().chain(self.blobs.iter()) {
            by_sha.insert(sha, *id);
        }
        let mut k = BTreeMap::new();
        for (sha, id) in &by_sha {
            k.insert((*sha).to_owned(), *id);
        }
        for (n, sha) in &self.marks {
            if let Some(id) = by_sha.get(sha.as_str()) {
                k.insert(format!(":{n}"), *id);
            }
        }
        k
    }
}

struct Helper {
    id: String,
    repo: Repo,
    st: Option<State>,
    out: io::Stdout,
}

fn clean_id(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn git_dir() -> Res<PathBuf> {
    let cwd = std::env::current_dir().map_err(e)?;
    match std::env::var_os("GIT_DIR") {
        Some(d) => Ok(cwd.join(d)),
        None if cwd.join(".git").is_dir() => Ok(cwd.join(".git")),
        None => Err("GIT_DIR is not set; this program is run by git (use `git clone allgit::<path>`)".into()),
    }
}

impl Helper {
    fn say(&self, s: &str) -> Res<()> {
        let mut o = self.out.lock();
        o.write_all(s.as_bytes()).map_err(e)
    }

    fn capabilities(&self) -> Res<()> {
        let mut s = format!(
            "import\nexport\nrefspec refs/heads/*:refs/allgit/{id}/heads/*\nrefspec refs/tags/*:refs/allgit/{id}/tags/*\n",
            id = self.id
        );
        if let Some(st) = &self.st {
            let m = st.dir.join("marks");
            s.push_str(&format!("*import-marks {m}\n*export-marks {m}\n", m = m.display()));
        }
        s.push('\n');
        self.say(&s)
    }

    fn head_branch(&self, branches: &[(String, ObjectId)]) -> Option<String> {
        let ws = self.repo.workspace().ok()?;
        if let Some(b) = ws.subs.get(ROOT_ZONE) {
            if branches.iter().any(|(n, _)| n == b) {
                return Some(b.clone());
            }
        }
        branches
            .iter()
            .find(|(n, _)| n == "main")
            .or(branches.first())
            .map(|(n, _)| n.clone())
    }

    fn list(&self) -> Res<()> {
        let refs = self.repo.refs().map_err(e)?;
        let branches = refs.branches_of(ROOT_ZONE);
        let mut s = String::new();
        for (b, _) in &branches {
            s.push_str(&format!("? refs/heads/{b}\n"));
        }
        for (z, n) in refs.tags.keys() {
            if z == ROOT_ZONE {
                s.push_str(&format!("? refs/tags/{n}\n"));
            }
        }
        if let Some(h) = self.head_branch(&branches) {
            s.push_str(&format!("@refs/heads/{h} HEAD\n"));
        }
        s.push('\n');
        self.say(&s)
    }

    /// Answer a batch of `import <ref>` lines with one fast-import stream.
    fn import(&mut self, wanted: &[String]) -> Res<()> {
        let st = self
            .st
            .as_mut()
            .ok_or("GIT_DIR is not set; run this through `git fetch`/`git clone`")?;
        let refs = self.repo.refs().map_err(e)?;
        let branches = refs.branches_of(ROOT_ZONE);
        let want_tags = wanted.iter().any(|r| r.starts_with("refs/tags/"));
        let marks = st.dir.join("marks");
        let mut buf: Vec<u8> = Vec::new();
        writeln!(buf, "feature done").map_err(e)?;
        writeln!(buf, "feature import-marks-if-exists={}", marks.display()).map_err(e)?;
        writeln!(buf, "feature export-marks={}", marks.display()).map_err(e)?;
        let mut known: BTreeMap<ObjectId, String> = st.commits.clone();
        let mut mark_base = st.max_mark();
        let mut pend: Vec<(char, ObjectId, usize)> = Vec::new();
        for (i, (b, _)) in branches.iter().enumerate() {
            let last = i + 1 == branches.len();
            let ext = ExportExt {
                known: known.clone(),
                mark_base,
                git_ref: Some(format!("refs/allgit/{}/heads/{b}", self.id)),
                partial: !last || !want_tags,
                tag_ref_prefix: Some(format!("refs/allgit/{}/tags", self.id)),
            };
            let rep = self
                .repo
                .export_git_ext(ROOT_ZONE, b, &mut buf, ExportOptions::default(), &ext)
                .map_err(|x| format!("cannot export branch {b}: {x}"))?;
            mark_base = rep.last_mark;
            for (id, m) in &rep.marks {
                known.insert(*id, format!(":{m}"));
                pend.push(('c', *id, *m));
            }
            for (id, m) in &rep.blob_marks {
                pend.push(('b', *id, *m));
            }
            for s in &rep.tags_skipped {
                eprintln!("git-remote-allgit: tag not exported: {s}");
            }
        }
        if branches.is_empty() || !want_tags {
            // the last export was partial (or there was none); close the stream ourselves
            writeln!(buf, "done").map_err(e)?;
        }
        st.add_pending(pend)?;
        let mut o = self.out.lock();
        o.write_all(&buf).map_err(e)?;
        o.flush().map_err(e)
    }

    /// `export`: read Git's fast-export stream from stdin into the root zone.
    fn export(&mut self, input: &mut impl BufRead) -> Res<()> {
        let st = self.st.as_mut().ok_or("GIT_DIR is not set; run this through `git push`")?;
        let opts = ImportOptions {
            isolate_marks: true,
            known: st.known_for_import(),
            per_ref_errors: true,
            update_checked_out: true,
        };
        let rep = self.repo.import_git_with(input, opts).map_err(|x| format!("push failed: {x}"))?;
        let mut pend = Vec::new();
        for (m, id) in &rep.commit_marks {
            if let Ok(n) = m.trim_start_matches(':').parse::<usize>() {
                pend.push(('c', *id, n));
            }
        }
        for (m, id) in &rep.blob_marks {
            if let Ok(n) = m.trim_start_matches(':').parse::<usize>() {
                pend.push(('b', *id, n));
            }
        }
        st.add_pending(pend)?;
        let rejected: BTreeMap<&str, &str> = rep.refs_rejected.iter().map(|(r, w)| (r.as_str(), w.as_str())).collect();
        let ignored: BTreeSet<&str> = rep.refs_ignored.iter().map(String::as_str).collect();
        let deleted: BTreeSet<&str> = rep.refs_deleted.iter().map(String::as_str).collect();
        let tag_skipped = |r: &str| -> Option<&String> {
            let n = r.strip_prefix("refs/tags/")?;
            rep.tags_skipped.iter().find(|t| t.starts_with(n))
        };
        let mut s = String::new();
        for r in &rep.refs_seen {
            let r = r.as_str();
            if let Some(w) = rejected.get(r) {
                s.push_str(&format!("error {r} {}\n", w.replace('\n', " ")));
            } else if deleted.contains(r) {
                s.push_str(&format!("error {r} deleting refs is not supported by the allgit helper\n"));
            } else if ignored.contains(r) {
                s.push_str(&format!("error {r} only refs/heads/* and refs/tags/* can be pushed\n"));
            } else if let Some(t) = tag_skipped(r) {
                s.push_str(&format!("error {r} {}\n", t.replace('\n', " ")));
            } else {
                s.push_str(&format!("ok {r}\n"));
            }
        }
        s.push('\n');
        self.say(&s)
    }
}

fn run() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (name, url) = match args.as_slice() {
        [n, u] => (n.clone(), u.clone()),
        [u] => (u.clone(), u.clone()),
        _ => return Err("usage: git-remote-allgit <remote> <url>".into()),
    };
    let path = url.strip_prefix("allgit::").unwrap_or(&url);
    if path.contains("://") && !path.starts_with("file://") {
        return Err(format!("only local allgit repositories are supported, not {path:?}"));
    }
    let path = path.strip_prefix("file://").unwrap_or(path);
    let repo = Repo::discover(Path::new(path)).map_err(|x| format!("{path}: not an allgit repository ({x})"))?;
    let id = clean_id(&name);
    let st = match git_dir() {
        Ok(d) => Some(State::load(d.join("allgit").join(&id))?),
        Err(_) => None,
    };
    let mut h = Helper {
        id,
        repo,
        st,
        out: io::stdout(),
    };
    let stdin = io::stdin();
    let mut input = io::BufReader::new(stdin.lock());
    let mut batch: Vec<String> = Vec::new();
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).map_err(e)? == 0 {
            break;
        }
        let line = line.trim_end_matches('\n');
        if let Some(r) = line.strip_prefix("import ") {
            batch.push(r.to_owned());
            continue;
        }
        if !batch.is_empty() {
            // a non-import line (blank) ends the batch
            let b = std::mem::take(&mut batch);
            h.import(&b)?;
            if line.is_empty() {
                h.out.lock().flush().map_err(e)?;
                continue;
            }
        }
        match line {
            "" => break,
            "capabilities" => h.capabilities()?,
            "list" | "list for-push" => h.list()?,
            "export" => h.export(&mut input)?,
            l if l.starts_with("option ") => h.say("unsupported\n")?,
            l => return Err(format!("unknown command {l:?}")),
        }
        h.out.lock().flush().map_err(e)?;
    }
    if !batch.is_empty() {
        h.import(&batch)?;
    }
    h.out.lock().flush().map_err(e)
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(m) => {
            eprintln!("fatal: git-remote-allgit: {m}");
            std::process::ExitCode::from(1)
        }
    }
}
