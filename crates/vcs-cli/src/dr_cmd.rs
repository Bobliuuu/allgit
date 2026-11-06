//! `vcs remote verify | backup | restore | safe-point` (issue #570, docs/backup-restore.md).
use crate::remote;
use std::path::{Path, PathBuf};
use vcs_remote::dr::{self, CopyOptions, VerifyOptions};
use vcs_remote::Backend;
use vcs_repo::Error;

fn cerr(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Msg(format!("{what}: {e}"))
}

/// The URL to act on: the argument, else the repository's configured remote.
fn target(arg: Option<String>, cwd: &Path) -> Result<String, Error> {
    match arg {
        Some(u) => Ok(remote::normalize_url(&u, cwd)),
        None => {
            let (repo, _) = crate::open()?;
            repo.remote_url()?
                .ok_or_else(|| Error::Msg("no remote configured (use `vcs remote set <url>` or pass a URL)".into()))
        }
    }
}

fn print_problems(label: &str, total: usize, list: &[dr::Problem]) {
    if total == 0 {
        return;
    }
    println!("{total} {label}:");
    for p in list {
        println!("  {} {} ({})", p.id.map_or(String::from("-"), |i| i.short()), p.what, p.context);
    }
    if total > list.len() {
        println!("  ... and {} more", total - list.len());
    }
}

pub fn print_report(url: &str, r: &dr::VerifyReport) {
    if !r.manifest_present {
        println!("{url}: NO MANIFEST (an empty remote, or a backup that never finished: re-run `vcs remote backup`)");
        return;
    }
    println!(
        "{url}: {} branch(es), {} snapshot(s), {} tag(s); {} root(s)",
        r.branches, r.snapshots, r.tags, r.roots
    );
    println!("walked {} reachable object(s), every hash checked", r.objects_walked);
    if r.deep {
        println!(
            "deep: {} pack(s) read end to end, {} loose object(s) checked ({} object(s), {} byte(s))",
            r.packs, r.loose, r.deep_objects, r.bytes_walked
        );
    }
    if r.packs_without_index > 0 {
        println!(
            "note: {} .pack file(s) have no index (an interrupted publish; harmless, gc removes them once old)",
            r.packs_without_index
        );
    }
    print_problems("MISSING object(s)", r.missing_total, &r.missing);
    print_problems("CORRUPT object(s)", r.corrupt_total, &r.corrupt);
    if r.is_ok() {
        println!("verify: OK");
    } else {
        println!("verify: FAILED");
    }
}

pub fn verify(url: Option<String>, manifest: Option<PathBuf>, deep: bool, cwd: &Path) -> Result<(), Error> {
    let url = target(url, cwd)?;
    let backend = remote::open_backend(&url)?;
    let m = match manifest {
        None => None,
        Some(p) => {
            let bytes = std::fs::read(&p).map_err(|e| cerr(&format!("cannot read {}", p.display()), e))?;
            Some(vcs_remote::Manifest::decode(&bytes).map_err(|e| cerr(&format!("{} is not a manifest", p.display()), e))?)
        }
    };
    let r = dr::verify_remote(backend.as_ref(), &VerifyOptions { manifest: m, deep }).map_err(|e| cerr("verify", e))?;
    print_report(&url, &r);
    if let Some(sp) = &r.safe_point {
        println!(
            "last gc safe point: backend time {} ms ({} object(s), {} pack(s) deleted); a manifest older than this may name deleted objects",
            sp.at_ms, sp.objects_deleted, sp.packs_deleted
        );
    }
    if r.is_ok() {
        Ok(())
    } else {
        Err(Error::Msg("remote verification failed".into()))
    }
}

pub struct CopyArgs {
    pub hold_secs: u64,
    pub no_hold: bool,
    pub no_deep: bool,
}

fn copy(src_url: &str, dst_url: &str, restore: bool, a: &CopyArgs) -> Result<(), Error> {
    let src = remote::open_backend(src_url)?;
    let dst = remote::open_backend(dst_url)?;
    let opts = CopyOptions {
        hold: !a.no_hold,
        hold_ttl_ms: a.hold_secs.saturating_mul(1000),
        deep: !a.no_deep,
        require_empty_dest: restore,
        ..Default::default()
    };
    let verb = if restore { "restore" } else { "backup" };
    let r = dr::backup_copy(src.as_ref(), dst.as_ref(), &opts).map_err(|e| cerr(verb, e))?;
    println!(
        "{verb}: {} key(s) copied ({} byte(s)), {} already there, {} branch(es){}{}",
        r.copied,
        r.bytes_copied,
        r.already_there,
        r.branches,
        if r.held { "; GC safe point held on the source" } else { "" },
        if r.passes > 1 { format!("; {} pass(es) ({} key(s) vanished)", r.passes, r.vanished) } else { String::new() },
    );
    print_report(dst_url, &r.verify);
    println!("manifest written to {dst_url} (last)");
    if restore {
        println!("use it with: vcs clone {dst_url} <dir>   (or vcs remote set {dst_url})");
    }
    Ok(())
}

pub fn backup(dest: &str, from: Option<String>, a: &CopyArgs, cwd: &Path) -> Result<(), Error> {
    let src = target(from, cwd)?;
    copy(&src, &remote::normalize_url(dest, cwd), false, a)
}

pub fn restore(from: &str, to: &str, a: &CopyArgs, cwd: &Path) -> Result<(), Error> {
    copy(&remote::normalize_url(from, cwd), &remote::normalize_url(to, cwd), true, a)
}

pub fn safe_point(url: Option<String>, cwd: &Path) -> Result<(), Error> {
    let url = target(url, cwd)?;
    let b = remote::open_backend(&url)?;
    let b: &dyn Backend = b.as_ref();
    match dr::read_safe_point(b).map_err(|e| cerr("safe point", e))? {
        Some(sp) => println!(
            "last destructive gc: backend time {} ms, {} object(s) and {} pack(s) deleted.\nA manifest or backup captured before that time without a hold may name deleted objects.",
            sp.at_ms, sp.objects_deleted, sp.packs_deleted
        ),
        None => println!("no destructive gc has been recorded on {url}"),
    }
    if b.supports_gc() {
        let now = b.now_ms().map_err(|e| cerr("clock", e))?;
        let holds = dr::list_holds(b, now).map_err(|e| cerr("holds", e))?;
        if holds.is_empty() {
            println!("no backup holds");
        }
        for h in holds {
            if h.expires_ms > now {
                println!("HOLD (gc and compaction refuse to run): {} for another {} s", h.owner, (h.expires_ms - now) / 1000);
            } else {
                println!("expired hold left by {} (a killed backup; the next gc clears it)", h.owner);
            }
        }
    } else {
        println!("this backend cannot be collected, so holds do not apply");
    }
    Ok(())
}
