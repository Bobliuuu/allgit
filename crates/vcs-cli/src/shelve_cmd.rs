//! `vcs shelve`, `vcs shelves`, `vcs unshelve`: private work-in-progress parked on the remote
//! (docs/shelves.md). The manifest is never touched.

use crate::remote;
use clap::Args;
use vcs_objects::Signature;
use vcs_remote::shelf;
use vcs_remote::Backend;
use vcs_repo::{Error, Repo, StashPopOutcome};

#[derive(Args, Clone)]
pub struct ShelveArgs {
    /// Shelf name (default: shelf-<unix time>); with --delete, `name` or `principal/name`
    pub name: Option<String>,
    /// Description of the shelf
    #[arg(short, long)]
    pub message: Option<String>,
    /// Shelve only this zone (default: every subscribed zone with changes)
    #[arg(long)]
    pub zone: Option<String>,
    /// Let `vcs gc --remote` delete the shelf after this long (e.g. 90m, 12h, 30d, 2w)
    #[arg(long, value_name = "DURATION", conflicts_with = "delete")]
    pub expires: Option<String>,
    /// Delete the shelf instead of creating it (owner or admin)
    #[arg(long)]
    pub delete: bool,
}

#[derive(Args, Clone)]
pub struct ShelvesArgs {
    /// Also list shelves of other principals that you are allowed to read
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Clone)]
pub struct UnshelveArgs {
    /// `principal/name` as printed by `vcs shelves --all` (a bare `name` is one of yours)
    pub shelf: String,
    /// Delete the shelf from the remote after it applied cleanly (like `stash pop`); a conflicted
    /// apply keeps it
    #[arg(long)]
    pub drop: bool,
}

/// Parse `90s`, `15m`, `12h`, `30d`, `2w` into milliseconds.
pub fn parse_duration(s: &str) -> Result<u64, Error> {
    let bad = || Error::Msg(format!("bad duration {s:?}: use a number and a unit s, m, h, d or w (e.g. 30d)"));
    let unit = s.chars().last().ok_or_else(bad)?;
    let n: u64 = s[..s.len() - unit.len_utf8()].parse().map_err(|_| bad())?;
    let mult = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86400,
        'w' => 7 * 86400,
        _ => return Err(bad()),
    };
    if n == 0 {
        return Err(bad());
    }
    n.checked_mul(mult).and_then(|x| x.checked_mul(1000)).ok_or_else(bad)
}

/// A principal usable as a key component: other characters become `_`.
fn sanitize(p: &str) -> String {
    let s: String = p
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._@+=-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(128)
        .collect();
    if s.is_empty() || s.starts_with('.') {
        format!("_{s}")
    } else {
        s
    }
}

/// The backend plus the name this user's shelves live under. For a vcs-server the name is what
/// the server calls the token (`/v1/whoami`), since the server only lets that principal write there.
fn connect(repo: &Repo, local_principal: &str) -> Result<(Box<dyn Backend>, String), Error> {
    let url = repo
        .remote_url()?
        .ok_or_else(|| Error::Msg("no remote configured (use `vcs remote set <url>`)".into()))?;
    if url.starts_with("http://") || url.starts_with("https://") {
        let tok = crate::auth::token_for(&url)?
            .map(|t| t.expose().to_owned())
            .filter(|t| !t.is_empty());
        let tok = tok.ok_or_else(|| Error::Msg(format!("not logged in to {url}; run `vcs login {url}` (or set VCS_TOKEN)")))?;
        let b = remote::http_backend(&url, &tok)?;
        let who = b.whoami().map_err(|e| Error::Msg(e.to_string().replace(&tok, "<redacted>")))?;
        let name = who.split_whitespace().next().unwrap_or("").to_owned();
        if name.is_empty() {
            return Err(Error::Msg("the server did not say who you are".into()));
        }
        return Ok((Box::new(b), name));
    }
    Ok((remote::open_backend(&url)?, sanitize(local_principal)))
}

fn cerr(e: shelf::ShelfError) -> Error {
    Error::Msg(e.to_string())
}

fn split_id<'a>(id: &'a str, me: &'a str) -> (&'a str, &'a str) {
    match id.split_once('/') {
        Some((p, n)) => (p, n),
        None => (me, id),
    }
}

pub fn shelve(repo: &Repo, a: ShelveArgs, local_principal: &str, author: Signature) -> Result<(), Error> {
    let (b, me) = connect(repo, local_principal)?;
    if a.delete {
        let id = a.name.ok_or_else(|| Error::Msg("--delete needs a shelf name".into()))?;
        let (p, n) = split_id(&id, &me);
        shelf::load_meta(b.as_ref(), p, n).map_err(cerr)?;
        shelf::delete(b.as_ref(), p, n).map_err(cerr)?;
        println!("deleted shelf {p}/{n}");
        return Ok(());
    }
    let name = a.name.unwrap_or_else(|| format!("shelf-{}", author.timestamp / 1_000_000_000));
    let expires = a.expires.as_deref().map(parse_duration).transpose()?;
    let m = repo.shelve(
        b.as_ref(),
        &me,
        &name,
        &vcs_repo::shelve::ShelveOpts {
            message: a.message.as_deref(),
            zone: a.zone.as_deref(),
            expires_in_ms: expires,
        },
        author,
    )?;
    println!(
        "shelved {} zone(s) as {} ({} object(s), {} byte(s)){}",
        m.zones.len(),
        m.id(),
        m.objects,
        m.pack_bytes,
        a.expires.map(|e| format!(", expires in {e}")).unwrap_or_default()
    );
    println!("pick it up with `vcs unshelve {}`", m.id());
    Ok(())
}

pub fn shelves(repo: &Repo, a: ShelvesArgs, local_principal: &str) -> Result<(), Error> {
    let (b, me) = connect(repo, local_principal)?;
    let list = shelf::list(b.as_ref(), if a.all { None } else { Some(&me) }).map_err(cerr)?;
    if list.is_empty() {
        println!("no shelves");
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    for l in list {
        match l.meta {
            Ok(m) => {
                let age = now.saturating_sub(m.created_ms) / 1000;
                let exp = match m.expires_ms {
                    Some(e) if e <= now => " (expired)".to_owned(),
                    Some(e) => format!(" (expires in {}s)", (e - now) / 1000),
                    None => String::new(),
                };
                let zs: Vec<&str> = m.zones.iter().map(|z| z.zone.as_str()).collect();
                println!(
                    "{}/{}  {}s ago  zones: {}{}  {}",
                    l.principal,
                    l.name,
                    age,
                    zs.join(","),
                    exp,
                    m.message
                );
            }
            Err(e) => println!("{}/{}  unreadable: {e}", l.principal, l.name),
        }
    }
    Ok(())
}

pub fn unshelve(repo: &Repo, a: UnshelveArgs, local_principal: &str) -> Result<(), Error> {
    let (b, me) = connect(repo, local_principal)?;
    let (p, n) = split_id(&a.shelf, &me);
    let (m, out) = repo.unshelve(b.as_ref(), p, n)?;
    match out {
        StashPopOutcome::Applied => {
            println!("applied shelf {} ({} zone(s)); changes are staged", m.id(), m.zones.len());
            if a.drop {
                shelf::delete(b.as_ref(), p, n).map_err(cerr)?;
                println!("dropped shelf {}", m.id());
            }
        }
        StashPopOutcome::Conflicted { paths } => {
            println!("applied shelf {} with conflicts in {} file(s):", m.id(), paths.len());
            for p in paths {
                println!("  {p}");
            }
            println!("fix the markers and `vcs add` the files; the shelf is still on the remote");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durations() {
        assert_eq!(parse_duration("30d").unwrap(), 30 * 86_400_000);
        assert_eq!(parse_duration("90m").unwrap(), 5_400_000);
        assert_eq!(parse_duration("2w").unwrap(), 14 * 86_400_000);
        for bad in ["", "d", "0d", "5", "5x", "-1d", "1.5h"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
        assert_eq!(sanitize("a b/c@x.io"), "a_b_c@x.io");
    }
}
