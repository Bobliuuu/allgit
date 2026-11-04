//! Signing and verification helpers for `vcs commit -S`, `vcs tag -s`, `vcs verify-commit`,
//! `vcs verify-tag` and `vcs log --show-signature`. Format and threat model: docs/signing.md.

use crate::config::Config;
use std::path::PathBuf;
use vcs_objects::signing::{self, SigStatus, SigningKey, Trust};
use vcs_repo::{Error, Repo};

fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(rest))
            .unwrap_or_else(|| p.into()),
        None => p.into(),
    }
}

/// The signing key: `keyid` (a path to an OpenSSH private key) or config `user.signingkey`.
pub fn load_key(cfg: &Config, keyid: Option<&str>) -> Result<SigningKey, Error> {
    let path = match keyid.map(str::to_owned).or_else(|| cfg.get("user.signingkey")) {
        Some(p) => p,
        None => {
            return Err(Error::Msg(
                "no signing key: set `user.signingkey` to the path of an unencrypted OpenSSH ed25519 private key (or pass -S<path>)".into(),
            ))
        }
    };
    let p = expand(&path);
    let text = std::fs::read_to_string(&p).map_err(|e| Error::Msg(format!("cannot read signing key {}: {e}", p.display())))?;
    SigningKey::from_openssh(&text).map_err(|e| Error::Msg(format!("signing key {}: {e}", p.display())))
}

/// Allowed signers (`signing.allowedSigners`) and revoked keys (`signing.revoked`). Missing
/// settings give an empty list: every valid signature is then reported as an unknown key.
pub fn load_trust(cfg: &Config) -> Result<Trust, Error> {
    let mut t = Trust::default();
    if let Some(p) = cfg.get("signing.allowedSigners") {
        let p = expand(&p);
        let text = std::fs::read_to_string(&p).map_err(|e| Error::Msg(format!("cannot read allowed signers {}: {e}", p.display())))?;
        t.parse_allowed(&text).map_err(Error::Msg)?;
    }
    if let Some(p) = cfg.get("signing.revoked") {
        let p = expand(&p);
        let text = std::fs::read_to_string(&p).map_err(|e| Error::Msg(format!("cannot read revoked keys {}: {e}", p.display())))?;
        t.parse_revoked(&text).map_err(Error::Msg)?;
    }
    Ok(t)
}

pub fn describe(s: &SigStatus) -> String {
    match s {
        SigStatus::Unsigned => "no signature".into(),
        SigStatus::Good { principal, fingerprint } => format!("good signature from {principal} (key {fingerprint})"),
        SigStatus::Bad(why) => format!("BAD signature: {why}"),
        SigStatus::UnknownKey { fingerprint } => format!("signature by unknown key {fingerprint} (not in signing.allowedSigners)"),
        SigStatus::Revoked { fingerprint } => format!("signature by REVOKED key {fingerprint}"),
    }
}

/// `vcs verify-commit`: print the verdict; fail unless the signature is good.
pub fn verify_commit(repo: &Repo, zone: &str, rev: &str) -> Result<(), Error> {
    let cfg = Config::load()?;
    let trust = load_trust(&cfg)?;
    let zone = repo.resolve_zone(zone)?;
    let id = repo.resolve_rev(&zone, rev)?;
    let c = repo.commit_of(&id)?;
    let st = signing::verify_commit(&c, &trust);
    println!("commit {}: {}", id.short(), describe(&st));
    if st.is_good() {
        Ok(())
    } else {
        Err(Error::Msg(format!(
            "commit {} is not validly signed by an allowed signer",
            id.short()
        )))
    }
}

/// `vcs verify-tag`.
pub fn verify_tag(repo: &Repo, zone: &str, name: &str) -> Result<(), Error> {
    let cfg = Config::load()?;
    let trust = load_trust(&cfg)?;
    let tags = repo.tag_list(zone)?;
    let Some(t) = tags.into_iter().find(|t| t.name == name) else {
        return Err(Error::Msg(format!("no such tag {name}")));
    };
    let Some(a) = t.annotation else {
        return Err(Error::Msg(format!("tag {name} is lightweight and cannot be signed")));
    };
    let st = signing::verify_tag(&a, &trust);
    println!("tag {name}: {}", describe(&st));
    if st.is_good() {
        Ok(())
    } else {
        Err(Error::Msg(format!("tag {name} is not validly signed by an allowed signer")))
    }
}
