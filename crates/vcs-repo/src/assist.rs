//! Assisted merge: ask an external command for a resolution of each text conflict, keep the
//! answers as *proposals* under `.vcs/proposals/`, and apply one only on explicit acceptance.
//! Nothing here touches the working tree, the index or the conflict state until
//! [`Repo::accept_proposal`] is called. See docs/merge.md ("Assisted merge").
//!
//! Proposal file (one per path, named by the blake3 of the path): lines `path`, `ours-hex`,
//! `theirs-hex`, `<explanation byte length>`, then the explanation bytes followed by the
//! resolution bytes. A malformed file is ignored. A proposal made for another merge (different
//! heads) is stale and ignored.

use crate::error::{io, msg, Error, Result};
use crate::merge::{ConflictRec, MergeState, Resolution};
use crate::meta::write_atomic;
use crate::repo::Repo;
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use vcs_merge::assist::{self, Reject, Request, MAX_RESPONSE_BYTES};
use vcs_merge::ConflictKind;
use vcs_objects::ObjectId;
use vcs_store::ObjectStore;

const DIR: &str = "proposals";

/// What happened for one conflicted path when the assistant was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssistResult {
    /// A valid proposal was recorded.
    Proposed,
    /// The conflict is not eligible (binary, not UTF-8, delete/modify, ...); the assistant was not run.
    Skipped(String),
    /// The assistant ran but its answer was ignored.
    Rejected(String),
    TimedOut,
    /// The command could not be started or exited unsuccessfully.
    Failed(String),
}

/// A recorded proposal, for display.
#[derive(Clone, Debug)]
pub struct ProposalInfo {
    pub path: String,
    pub explanation: String,
    /// Diff from our side to the proposal.
    pub diff: String,
}

struct Stored {
    path: String,
    ours: ObjectId,
    theirs: ObjectId,
    explanation: String,
    resolution: String,
}

fn encode(s: &Stored) -> Vec<u8> {
    let mut v = format!("{}\n{}\n{}\n{}\n", s.path, s.ours.to_hex(), s.theirs.to_hex(), s.explanation.len()).into_bytes();
    v.extend_from_slice(s.explanation.as_bytes());
    v.extend_from_slice(s.resolution.as_bytes());
    v
}

fn decode(b: &[u8]) -> Option<Stored> {
    let mut rest = b;
    let mut line = || -> Option<&str> {
        let n = rest.iter().position(|&c| c == b'\n')?;
        let (l, r) = rest.split_at(n);
        rest = &r[1..];
        std::str::from_utf8(l).ok()
    };
    let path = line()?.to_owned();
    let ours = ObjectId::from_hex(line()?).ok()?;
    let theirs = ObjectId::from_hex(line()?).ok()?;
    let n: usize = line()?.parse().ok()?;
    if n > rest.len() {
        return None;
    }
    let (e, r) = rest.split_at(n);
    Some(Stored {
        path,
        ours,
        theirs,
        explanation: String::from_utf8(e.to_vec()).ok()?,
        resolution: String::from_utf8(r.to_vec()).ok()?,
    })
}

/// Run `cmd` through the shell, feed it `input` on stdin, and return its stdout.
/// Kills it at `timeout`. Stdout is read up to the protocol limit plus one byte.
pub fn run_command(cmd: &str, input: &[u8], timeout: Duration, cwd: &std::path::Path) -> std::result::Result<Vec<u8>, AssistResult> {
    #[cfg(unix)]
    let mut c = {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    #[cfg(not(unix))]
    let mut c = {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    };
    c.current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = c.spawn().map_err(|e| AssistResult::Failed(format!("cannot start {cmd:?}: {e}")))?;
    let deadline = Instant::now() + timeout;
    let mut stdin = child.stdin.take().expect("piped");
    let data = input.to_vec();
    // Threads are detached: if a grandchild keeps a pipe open after the kill they end with it.
    std::thread::spawn(move || {
        let _ = stdin.write_all(&data); // the assistant may exit without reading
    });
    let mut stdout = child.stdout.take().expect("piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let r = (&mut stdout).take(MAX_RESPONSE_BYTES as u64 + 1).read_to_end(&mut buf);
        let _ = tx.send(r.map(|_| buf));
    });
    let remaining = deadline.saturating_duration_since(Instant::now());
    let out = match rx.recv_timeout(remaining) {
        Ok(Ok(buf)) => buf,
        Ok(Err(e)) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AssistResult::Failed(format!("reading output: {e}")));
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AssistResult::TimedOut);
        }
    };
    if out.len() > MAX_RESPONSE_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return Err(AssistResult::Rejected(Reject::TooLarge(out.len()).to_string()));
    }
    loop {
        match child.try_wait() {
            Ok(Some(st)) if st.success() => return Ok(out),
            Ok(Some(st)) => return Err(AssistResult::Failed(format!("exited with {st}"))),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AssistResult::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(AssistResult::Failed(e.to_string())),
        }
    }
}

impl Repo {
    fn proposals_dir(&self) -> PathBuf {
        self.vcs.join(DIR)
    }

    fn proposal_file(&self, path: &str) -> PathBuf {
        self.proposals_dir().join(blake3::hash(path.as_bytes()).to_hex().to_string())
    }

    pub(crate) fn clear_proposals(&self) {
        let _ = fs::remove_dir_all(self.proposals_dir());
    }

    fn text_of(&self, id: &Option<ObjectId>) -> Result<Option<Option<String>>> {
        // outer None: absent side; inner None: not text
        Ok(match id {
            None => None,
            Some(i) => {
                let b = self.store.get_blob(i)?;
                let bin = b[..b.len().min(8000)].contains(&0);
                Some(if bin { None } else { String::from_utf8(b).ok() })
            }
        })
    }

    fn stored_for(&self, st: &MergeState, path: &str) -> Option<Stored> {
        let s = decode(&fs::read(self.proposal_file(path)).ok()?)?;
        (s.path == path && s.ours == st.ours && s.theirs == st.theirs).then_some(s)
    }

    /// Ask `cmd` for a resolution of every unresolved text conflict of the in-progress merge or rebase.
    /// Valid answers are recorded as proposals; nothing else changes.
    pub fn assist_conflicts(&self, cmd: &str, timeout: Duration) -> Result<Vec<(String, AssistResult)>> {
        let st = self.merge_state()?.ok_or_else(|| Error::Msg("no merge in progress".into()))?;
        let dir = self.proposals_dir();
        io(&dir, fs::create_dir_all(&dir))?;
        let mut report = Vec::new();
        for c in st.conflicts.iter().filter(|c| !c.resolved) {
            let r = self.assist_one(&st, c, cmd, timeout)?;
            report.push((c.path.clone(), r));
        }
        Ok(report)
    }

    fn assist_one(&self, st: &MergeState, c: &ConflictRec, cmd: &str, timeout: Duration) -> Result<AssistResult> {
        let _ = fs::remove_file(self.proposal_file(&c.path)); // a re-run replaces any older proposal
        if !matches!(c.kind, ConflictKind::AddAdd | ConflictKind::ModifyModify) {
            return Ok(AssistResult::Skipped(format!("{} conflict", crate::merge::kind_name(c.kind))));
        }
        let (Some(Some(ours)), Some(Some(theirs))) = (self.text_of(&c.left)?, self.text_of(&c.right)?) else {
            return Ok(AssistResult::Skipped("binary or non-UTF-8 content".into()));
        };
        let base = match self.text_of(&c.base)? {
            None => None,
            Some(Some(b)) => Some(b),
            Some(None) => return Ok(AssistResult::Skipped("binary or non-UTF-8 content".into())),
        };
        let req = assist::encode_request(&Request {
            path: &c.path,
            language: assist::language_of(&c.path),
            base: base.as_deref(),
            ours: &ours,
            theirs: &theirs,
        });
        let out = match run_command(cmd, req.as_bytes(), timeout, &self.worktree) {
            Ok(o) => o,
            Err(r) => return Ok(r),
        };
        match assist::parse_response(&out) {
            Err(e) => Ok(AssistResult::Rejected(e.to_string())),
            Ok(p) => {
                let s = Stored {
                    path: c.path.clone(),
                    ours: st.ours,
                    theirs: st.theirs,
                    explanation: p.explanation,
                    resolution: p.resolution,
                };
                write_atomic(&self.proposal_file(&c.path), &encode(&s))?;
                Ok(AssistResult::Proposed)
            }
        }
    }

    /// Proposals for the in-progress merge or rebase (unresolved paths only), with a diff against our side.
    pub fn proposals(&self) -> Result<Vec<ProposalInfo>> {
        let st = self.merge_state()?.ok_or_else(|| Error::Msg("no merge in progress".into()))?;
        let mut out = Vec::new();
        for c in st.conflicts.iter().filter(|c| !c.resolved) {
            let Some(s) = self.stored_for(&st, &c.path) else { continue };
            let ours = match self.text_of(&c.left)? {
                Some(Some(t)) => t,
                _ => String::new(),
            };
            out.push(ProposalInfo {
                path: c.path.clone(),
                explanation: s.explanation,
                diff: assist::diff_lines(&ours, &s.resolution),
            });
        }
        Ok(out)
    }

    /// Apply the proposal for `path` through the ordinary `resolve` path (writes the file, stages
    /// it, marks the conflict resolved). Fails when there is no valid proposal for this merge.
    pub fn accept_proposal(&self, path: &str) -> Result<()> {
        let st = self
            .merge_state()?
            .ok_or_else(|| Error::Msg("no merge or rebase in progress".into()))?;
        if !st.conflicts.iter().any(|c| c.path == path && !c.resolved) {
            return msg(format!("{path:?} is not an unresolved conflict"));
        }
        let Some(s) = self.stored_for(&st, path) else {
            return msg(format!("no proposal for {path:?}"));
        };
        let tmp = self.vcs.join(format!("proposal-apply-{}", std::process::id()));
        io(&tmp, fs::write(&tmp, s.resolution.as_bytes()))?;
        let r = self.resolve(path, &Resolution::File(tmp.clone()));
        let _ = fs::remove_file(&tmp);
        r?;
        let _ = fs::remove_file(self.proposal_file(path));
        Ok(())
    }
}
