//! `vcs bisect start|good|bad|skip|reset|run`: binary search over a zone's first-parent history.
//!
//! State lives in `.vcs/bisect` (a small text file replaced atomically after every step, so a
//! crash loses at most the step in flight; `bisect good/bad` again resumes). Candidates are
//! checked out on the branch `bisect/work` of the zone with the same safety as `switch`
//! (refuses with uncommitted changes); `bisect reset` returns to the original branch.

use super::{open, zone_of_cwd};
use clap::Subcommand;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use vcs_objects::ObjectId;
use vcs_repo::{Error, Repo};

const BRANCH: &str = "bisect/work";

#[derive(Subcommand, Clone)]
pub enum BisectCmd {
    /// Start bisecting; optionally give the bad commit and good commits right away
    Start {
        /// Bad revision, then any number of good revisions
        revs: Vec<String>,
        #[arg(long)]
        zone: Option<String>,
    },
    /// Mark revisions (default: the checked-out commit) as good
    Good { revs: Vec<String> },
    /// Mark revisions (default: the checked-out commit) as bad
    Bad { revs: Vec<String> },
    /// Mark revisions (default: the checked-out commit) as untestable
    Skip { revs: Vec<String> },
    /// End the bisection and return to the original branch
    Reset,
    /// Run a command at each step: exit 0 = good, 125 = skip, 1-127 = bad, above 127 aborts
    Run {
        #[arg(trailing_var_arg = true, required = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },
}

#[derive(Default, Debug, PartialEq)]
struct State {
    zone: String,
    orig: String,
    good: Vec<ObjectId>,
    bad: Vec<ObjectId>,
    skip: Vec<ObjectId>,
}

fn path(repo: &Repo) -> PathBuf {
    repo.worktree().join(".vcs").join("bisect")
}

fn err<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Msg(m.into()))
}

impl State {
    fn encode(&self) -> String {
        let mut s = format!("zone {}\norig {}\n", self.zone, self.orig);
        for (k, v) in [("good", &self.good), ("bad", &self.bad), ("skip", &self.skip)] {
            for id in v {
                s.push_str(&format!("{k} {}\n", id.to_hex()));
            }
        }
        s
    }
    fn decode(text: &str) -> Result<State, Error> {
        let mut st = State::default();
        for l in text.lines() {
            let Some((k, v)) = l.split_once(' ') else { continue };
            let id = || ObjectId::from_hex(v).map_err(|e| Error::Msg(format!(".vcs/bisect is corrupt: {e}")));
            match k {
                "zone" => st.zone = v.into(),
                "orig" => st.orig = v.into(),
                "good" => st.good.push(id()?),
                "bad" => st.bad.push(id()?),
                "skip" => st.skip.push(id()?),
                _ => {}
            }
        }
        if st.zone.is_empty() || st.orig.is_empty() {
            return err(".vcs/bisect is corrupt (run `vcs bisect reset`)");
        }
        Ok(st)
    }
    fn save(&self, repo: &Repo) -> Result<(), Error> {
        let p = path(repo);
        let tmp = p.with_extension("tmp");
        let io = |source| Error::Io { path: p.clone(), source };
        let mut f = fs::File::create(&tmp).map_err(io)?;
        f.write_all(self.encode().as_bytes()).map_err(io)?;
        f.sync_all().map_err(io)?;
        fs::rename(&tmp, &p).map_err(io)
    }
}

fn load(repo: &Repo) -> Result<State, Error> {
    match fs::read_to_string(path(repo)) {
        Ok(t) => State::decode(&t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => err("no bisection in progress; start with `vcs bisect start <bad> <good>`"),
        Err(e) => Err(Error::Io {
            path: ".vcs/bisect".into(),
            source: e,
        }),
    }
}

/// What the marks say to do next.
#[derive(Debug, PartialEq)]
enum Next {
    NeedBad,
    NeedGood,
    Test { commit: ObjectId, left: usize },
    Found(ObjectId),
    OnlySkipped(Vec<ObjectId>),
}

fn chain(repo: &Repo, from: ObjectId) -> Result<Vec<ObjectId>, Error> {
    repo.parent_index(&[from]).first_parent_chain(from)
}

fn plan(repo: &Repo, st: &State) -> Result<Next, Error> {
    let Some(top) = st.bad.first() else { return Ok(Next::NeedBad) };
    if st.good.is_empty() {
        return Ok(Next::NeedGood);
    }
    let ch = chain(repo, *top)?;
    let pos = |id: &ObjectId| ch.iter().position(|c| c == id);
    // Oldest bad on the first-parent chain, nearest good below it.
    let ib = st.bad.iter().filter_map(pos).max().unwrap_or(0);
    let Some(ig) = st.good.iter().filter_map(pos).filter(|i| *i > ib).min() else {
        return err("no good commit is on the first-parent history below the bad commit; mark one that is");
    };
    if ig == ib + 1 {
        return Ok(Next::Found(ch[ib]));
    }
    let untested: Vec<usize> = (ib + 1..ig).filter(|i| !st.skip.contains(&ch[*i])).collect();
    if untested.is_empty() {
        return Ok(Next::OnlySkipped((ib..ig).map(|i| ch[i]).collect()));
    }
    let mid = (ib + ig) / 2;
    let pick = *untested.iter().min_by_key(|i| i.abs_diff(mid)).unwrap();
    Ok(Next::Test {
        commit: ch[pick],
        left: untested.len(),
    })
}

fn steps(n: usize) -> u32 {
    usize::BITS - n.max(1).leading_zeros() - 1
}

/// Print the plan; when a commit must be tested, check it out. Returns the plan.
fn advance(repo: &Repo, st: &State) -> Result<Next, Error> {
    let n = plan(repo, st)?;
    match &n {
        Next::NeedBad => println!("status: waiting for a bad commit (`vcs bisect bad [rev]`)"),
        Next::NeedGood => println!("status: waiting for a good commit (`vcs bisect good [rev]`)"),
        Next::Test { commit, left } => {
            repo.checkout_commit(&st.zone, BRANCH, commit)?;
            let c = repo.commit_of(commit)?;
            println!(
                "Bisecting: {} revision(s) left to test (roughly {} step(s)); now at {} {}",
                left,
                steps(*left),
                commit.short(),
                c.message.lines().next().unwrap_or("")
            );
        }
        Next::Found(id) => {
            let c = repo.commit_of(id)?;
            println!(
                "{} is the first bad commit\n    {}",
                id.to_hex(),
                c.message.lines().next().unwrap_or("")
            );
        }
        Next::OnlySkipped(v) => {
            println!("only skipped commits are left to test; the first bad commit is one of:");
            for id in v {
                println!("  {}", id.short());
            }
        }
    }
    Ok(n)
}

fn revs(repo: &Repo, st: &State, revs: &[String]) -> Result<Vec<ObjectId>, Error> {
    if revs.is_empty() {
        return match repo.head_commit(&st.zone)? {
            Some(h) => Ok(vec![h]),
            None => err("zone has no commits"),
        };
    }
    revs.iter().map(|r| repo.resolve_rev(&st.zone, r)).collect()
}

fn mark(repo: &Repo, st: &mut State, kind: &str, ids: Vec<ObjectId>) -> Result<(), Error> {
    for id in ids {
        for list in [&mut st.good, &mut st.bad, &mut st.skip] {
            list.retain(|x| *x != id);
        }
        match kind {
            "good" => st.good.push(id),
            "bad" => st.bad.push(id),
            _ => st.skip.push(id),
        }
    }
    st.save(repo)
}

pub fn run(cmd: &BisectCmd) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    match cmd {
        BisectCmd::Start { revs: r, zone } => {
            if path(&repo).exists() {
                return err("a bisection is already in progress; `vcs bisect reset` ends it");
            }
            let zone = repo.resolve_zone(&zone_of_cwd(&repo, &cwd, zone.clone())?)?;
            let Some(orig) = repo.current_branch(&zone)? else {
                return err(format!("zone {zone:?} is not subscribed"));
            };
            let mut st = State {
                zone,
                orig,
                ..State::default()
            };
            if let Some((bad, good)) = r.split_first() {
                st.bad.push(repo.resolve_rev(&st.zone, bad)?);
                for g in good {
                    st.good.push(repo.resolve_rev(&st.zone, g)?);
                }
            }
            st.save(&repo)?;
            if let Err(e) = advance(&repo, &st) {
                let _ = fs::remove_file(path(&repo));
                return Err(e);
            }
        }
        BisectCmd::Good { revs: r } | BisectCmd::Bad { revs: r } | BisectCmd::Skip { revs: r } => {
            let mut st = load(&repo)?;
            let kind = match cmd {
                BisectCmd::Good { .. } => "good",
                BisectCmd::Bad { .. } => "bad",
                _ => "skip",
            };
            let ids = revs(&repo, &st, r)?;
            mark(&repo, &mut st, kind, ids)?;
            advance(&repo, &st)?;
        }
        BisectCmd::Reset => {
            let st = load(&repo)?;
            if repo.current_branch(&st.zone)?.as_deref() == Some(BRANCH) {
                repo.checkout(&st.zone, &st.orig)?;
            }
            let _ = repo.branch_delete(&st.zone, BRANCH, true);
            fs::remove_file(path(&repo)).map_err(|source| Error::Io {
                path: ".vcs/bisect".into(),
                source,
            })?;
            println!("bisect ended; back on {}/{}", st.zone, st.orig);
        }
        BisectCmd::Run { cmd } => {
            let mut st = load(&repo)?;
            while let Next::Test { commit, .. } = advance(&repo, &st)? {
                let code = run_cmd(&repo, cmd)?;
                let kind = match code {
                    0 => "good",
                    125 => "skip",
                    1..=127 => "bad",
                    c => return err(format!("bisect run: command exited with status {c}; bisection left in place")),
                };
                println!("  -> {} is {kind}", commit.short());
                mark(&repo, &mut st, kind, vec![commit])?;
            }
        }
    }
    Ok(())
}

fn run_cmd(repo: &Repo, cmd: &[String]) -> Result<i32, Error> {
    let mut c = if cmd.len() == 1 {
        let mut c = Command::new("sh");
        c.arg("-c").arg(&cmd[0]);
        c
    } else {
        let mut c = Command::new(&cmd[0]);
        c.args(&cmd[1..]);
        c
    };
    let status = c.current_dir(repo.worktree()).status().map_err(|source| Error::Io {
        path: cmd[0].clone().into(),
        source,
    })?;
    Ok(status.code().unwrap_or(255))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_roundtrip() {
        let id = ObjectId::from_bytes([7; 32]);
        let st = State {
            zone: "root".into(),
            orig: "main".into(),
            good: vec![id],
            bad: vec![id],
            skip: vec![],
        };
        assert_eq!(State::decode(&st.encode()).unwrap(), st);
        assert!(State::decode("junk").is_err());
    }

    #[test]
    fn step_estimate() {
        assert_eq!(steps(1), 0);
        assert_eq!(steps(8), 3);
    }
}
