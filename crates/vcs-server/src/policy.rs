//! Per-zone path-glob permission policy (#123) and `require-lock` rules.
//!
//! File format, one rule per line, `#` starts a comment:
//!
//! ```text
//! <allow|deny|require-lock> <principal|*> <zone|*> <glob>
//! ```
//!
//! **Deny by default.** When a policy is configured, a Write principal may change a path only
//! if some `allow` rule matches (principal, zone, path) and no `deny` rule does. `deny` always
//! wins over `allow`. A principal also needs at least one `allow` rule for a zone to move a
//! branch head or define that zone at all. Admins bypass `allow`/`deny` (they can already break
//! locks and write every zone) but not `require-lock`. Read access is by role only: objects are
//! content-addressed and shared between zones, so reads cannot be filtered by path (see below).
//!
//! **Read rules** (`allow-read` / `deny-read`, glob must be `**`: whole zones only) restrict what a
//! non-admin principal may read of the objects reachable only from other zones. A principal with
//! at least one `allow-read` rule may read exactly the zones some `allow-read` matches; without
//! any it may read every zone except those a `deny-read` names. `deny-read` always wins. The
//! server enforces this on object GET/HEAD/range and list (docs/server.md "Read policy" states
//! what this cannot hide: the manifest and object ids stay visible).
//!
//! **Signed branches**: `require-signed <zone|*> <branch-glob>` (three fields). A publish that moves
//! a matching branch head is refused unless every commit new to the remote (not an ancestor of the
//! branch's current head) carries a valid Ed25519 signature by a key in the server's allowed-signers
//! file and not in its revoked list (docs/signing.md). It applies to everyone, admins included, and
//! does not switch writes to deny-by-default.
//!
//! **Protected branches**: `protect-branch <zone|*> <branch-glob>` (three fields). A publish that
//! would move a matching branch head to a commit that is not a descendant of its current head
//! (a force push), or delete the branch, is refused. It applies to everyone, admins included, and
//! does not switch writes to deny-by-default. Without this rule a forced update is allowed (it is
//! always a compare-and-swap) but is recorded in the audit log as `FORCE`.
//!
//! `require-lock`: a change to a path matching the glob is refused unless the writer holds a
//! live lock on exactly that path in the resulting manifest, so binary assets cannot be
//! overwritten blind. Locks are validated by the existing lock logic; this rule only demands
//! that one exists.

use crate::glob::glob_match;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Allow,
    Deny,
    RequireLock,
    AllowRead,
    DenyRead,
    /// `shelve-read <reader|*> <owner|*> **`: the reader may read the owner's shelves.
    ShelveRead,
    /// `require-signed <zone> <branch-glob>`: stored with principal `*`, glob = branch glob.
    RequireSigned,
    /// `protect-branch <zone> <branch-glob>`: no non-fast-forward move, no deletion.
    ProtectBranch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub effect: Effect,
    pub principal: String,
    pub zone: String,
    pub glob: String,
}

/// The spelling under which a case-folding, Unicode-normalizing or Windows filesystem would resolve a
/// path: per component, drop an NTFS `:stream` suffix and trailing dots/spaces, apply NFKC, remove
/// zero-width code points, lowercase and fold `ß`/`ſ`/`ς`. `deny` and `require-lock` rules also match
/// this alias, so `Secrets/k` or `secrets./k` cannot slip past `deny ... secrets/**` (the objects are
/// accepted as written; only the checked-out spelling collides).
pub fn alias_key(path: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let mut out = String::with_capacity(path.len());
    for (i, c) in path.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        let base = c.split(':').next().unwrap_or(c).trim_end_matches(['.', ' ']);
        if base.is_ascii() {
            out.extend(base.chars().map(|ch| ch.to_ascii_lowercase()));
            continue;
        }
        for ch in base
            .nfkc()
            .filter(|c| !matches!(*c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}'))
            .flat_map(char::to_lowercase)
        {
            match ch {
                'ß' => out.push_str("ss"),
                'ſ' => out.push('s'),
                'ς' => out.push('σ'),
                ch => out.push(ch),
            }
        }
    }
    out
}

impl Rule {
    fn applies(&self, principal: &str, zone: &str) -> bool {
        (self.principal == "*" || self.principal == principal) && (self.zone == "*" || self.zone == zone)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    rules: Vec<Rule>,
}

/// Why a path change was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    /// No allow rule (deny-by-default) or an explicit deny matched.
    Denied(String),
    /// A `require-lock` rule matched and the writer holds no live lock.
    NeedsLock(String),
}

impl Policy {
    pub fn parse(text: &str) -> Result<Policy, String> {
        let mut rules = vec![];
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            let err = |m: &str| format!("policy line {}: {m}", n + 1);
            if f.first() == Some(&"require-signed") {
                if f.len() != 3 || f[2].is_empty() {
                    return Err(err("expected `require-signed <zone|*> <branch-glob>`"));
                }
                rules.push(Rule {
                    effect: Effect::RequireSigned,
                    principal: "*".into(),
                    zone: f[1].to_owned(),
                    glob: f[2].to_owned(),
                });
                continue;
            }
            if f.first() == Some(&"protect-branch") {
                if f.len() != 3 || f[2].is_empty() {
                    return Err(err("expected `protect-branch <zone|*> <branch-glob>`"));
                }
                rules.push(Rule {
                    effect: Effect::ProtectBranch,
                    principal: "*".into(),
                    zone: f[1].to_owned(),
                    glob: f[2].to_owned(),
                });
                continue;
            }
            if f.len() != 4 {
                return Err(err(
                    "expected `<allow|deny|require-lock|allow-read|deny-read|shelve-read> <principal|*> <zone|*> <glob>`",
                ));
            }
            let effect = match f[0] {
                "allow" => Effect::Allow,
                "deny" => Effect::Deny,
                "require-lock" => Effect::RequireLock,
                "allow-read" => Effect::AllowRead,
                "deny-read" => Effect::DenyRead,
                "shelve-read" => Effect::ShelveRead,
                _ => {
                    return Err(err(
                        "effect must be allow, deny, require-lock, allow-read, deny-read or shelve-read",
                    ))
                }
            };
            if matches!(effect, Effect::AllowRead | Effect::DenyRead | Effect::ShelveRead) && f[3] != "**" {
                return Err(err(
                    "read rules apply to whole zones: the glob must be `**` (objects are content-addressed, so reads cannot be limited to paths)",
                ));
            }
            if f[3].starts_with('/') || f[3].is_empty() {
                return Err(err("glob must be a zone-relative path pattern (no leading /)"));
            }
            rules.push(Rule {
                effect,
                principal: f[1].to_owned(),
                zone: f[2].to_owned(),
                glob: f[3].to_owned(),
            });
        }
        Ok(Policy { rules })
    }

    /// Hex BLAKE3 of the canonical rule list (order and content), recorded in audit lines so a
    /// decision can be tied to the exact policy that made it.
    pub fn hash(&self) -> String {
        let mut h = blake3::Hasher::new();
        for r in &self.rules {
            h.update(format!("{:?}\t{}\t{}\t{}\n", r.effect, r.principal, r.zone, r.glob).as_bytes());
        }
        h.finalize().to_hex().to_string()
    }

    /// True when there is no write-side rule (`allow`, `deny`, `require-lock`). Read rules alone do
    /// not switch writes to deny-by-default.
    pub fn is_empty(&self) -> bool {
        self.rules.iter().all(|r| {
            matches!(
                r.effect,
                Effect::AllowRead | Effect::DenyRead | Effect::ShelveRead | Effect::RequireSigned | Effect::ProtectBranch
            )
        })
    }

    /// Is `zone`/`branch` protected against non-fast-forward moves and deletion?
    pub fn protects_branch(&self, zone: &str, branch: &str) -> bool {
        self.rules
            .iter()
            .any(|r| r.effect == Effect::ProtectBranch && (r.zone == "*" || r.zone == zone) && glob_match(&r.glob, branch))
    }

    /// Must new commits published to `zone`/`branch` be signed?
    pub fn requires_signed(&self, zone: &str, branch: &str) -> bool {
        self.rules
            .iter()
            .any(|r| r.effect == Effect::RequireSigned && (r.zone == "*" || r.zone == zone) && glob_match(&r.glob, branch))
    }

    /// Does any read rule name this principal (or `*`)? Only then are its reads checked at all.
    pub fn read_restricted(&self, principal: &str) -> bool {
        self.rules
            .iter()
            .any(|r| matches!(r.effect, Effect::AllowRead | Effect::DenyRead) && (r.principal == "*" || r.principal == principal))
    }

    /// May `reader` read the shelves of `owner`? (The owner always may; admins are never asked.)
    pub fn shelve_read_allowed(&self, reader: &str, owner: &str) -> bool {
        reader == owner
            || self
                .rules
                .iter()
                .any(|r| r.effect == Effect::ShelveRead && r.applies(reader, owner))
    }

    /// May a non-admin `principal` read objects of `zone`? Admins are never asked.
    pub fn read_zone_allowed(&self, principal: &str, zone: &str) -> bool {
        let hit = |e: Effect| self.rules.iter().any(|r| r.effect == e && r.applies(principal, zone));
        if hit(Effect::DenyRead) {
            return false;
        }
        let has_allow = self
            .rules
            .iter()
            .any(|r| r.effect == Effect::AllowRead && (r.principal == "*" || r.principal == principal));
        !has_allow || hit(Effect::AllowRead)
    }

    /// May `principal` touch zone-level state (branch heads, zone definition) in `zone`?
    pub fn zone_access(&self, principal: &str, zone: &str) -> bool {
        self.rules.iter().any(|r| r.effect == Effect::Allow && r.applies(principal, zone))
    }

    /// Deny-by-default check of one changed path. `admin` skips allow/deny.
    pub fn check_path(&self, principal: &str, admin: bool, zone: &str, path: &str) -> Verdict {
        if !admin {
            let hit = |e: Effect| {
                self.rules
                    .iter()
                    .find(|r| r.effect == e && r.applies(principal, zone) && glob_match(&r.glob, path))
            };
            // deny also matches the filesystem alias of the path (case, trailing dots, normalization)
            let alias = alias_key(path);
            let deny_hit = hit(Effect::Deny).or_else(|| {
                self.rules
                    .iter()
                    .find(|r| r.effect == Effect::Deny && r.applies(principal, zone) && glob_match(&alias_key(&r.glob), &alias))
            });
            if let Some(r) = deny_hit {
                return Verdict::Denied(format!(
                    "path {path} in zone {zone} is denied by policy rule `deny {} {} {}`",
                    r.principal, r.zone, r.glob
                ));
            }
            if hit(Effect::Allow).is_none() {
                return Verdict::Denied(format!(
                    "path {path} in zone {zone} is not allowed for {principal} (policy is deny-by-default)"
                ));
            }
        }
        let alias = alias_key(path);
        if self.rules.iter().any(|r| {
            r.effect == Effect::RequireLock
                && r.applies(principal, zone)
                && (glob_match(&r.glob, path) || glob_match(&alias_key(&r.glob), &alias))
        }) {
            return Verdict::NeedsLock(format!(
                "path {path} in zone {zone} requires a lock (`vcs lock`) before it can be changed"
            ));
        }
        Verdict::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pol() -> Policy {
        Policy::parse(
            "allow alice art art/**\nallow * code **\ndeny * code secrets/**\nrequire-lock * art **/*.psd\nallow bob art art/bob/**\n",
        )
        .unwrap()
    }

    #[test]
    fn deny_by_default_and_deny_wins() {
        let p = pol();
        assert_eq!(p.check_path("alice", false, "art", "art/x.png"), Verdict::Ok);
        assert!(matches!(p.check_path("alice", false, "art", "docs/x"), Verdict::Denied(_)));
        assert!(matches!(p.check_path("carol", false, "art", "art/x.png"), Verdict::Denied(_)));
        assert_eq!(p.check_path("carol", false, "code", "src/a.rs"), Verdict::Ok);
        assert!(matches!(p.check_path("carol", false, "code", "secrets/k"), Verdict::Denied(_)));
        assert!(matches!(p.check_path("alice", false, "unknown-zone", "x"), Verdict::Denied(_)));
        assert!(matches!(p.check_path("bob", false, "art", "art/alice/x"), Verdict::Denied(_)));
        assert_eq!(p.check_path("bob", false, "art", "art/bob/x"), Verdict::Ok);
    }

    #[test]
    fn admin_bypasses_allow_but_not_require_lock() {
        let p = pol();
        assert_eq!(p.check_path("root", true, "code", "secrets/k"), Verdict::Ok);
        assert!(matches!(p.check_path("root", true, "art", "art/h.psd"), Verdict::NeedsLock(_)));
        assert!(matches!(p.check_path("alice", false, "art", "art/h.psd"), Verdict::NeedsLock(_)));
    }

    #[test]
    fn zone_access_needs_an_allow_rule() {
        let p = pol();
        assert!(p.zone_access("alice", "art"));
        assert!(!p.zone_access("carol", "art"));
        assert!(p.zone_access("carol", "code"));
        assert!(!Policy::default().zone_access("alice", "art"));
    }

    #[test]
    fn read_rules() {
        let p = Policy::parse("allow-read rita art **\ndeny-read * secret **\ndeny-read bob code **\n").unwrap();
        // rita has an allow-read: only art
        assert!(p.read_restricted("rita"));
        assert!(p.read_zone_allowed("rita", "art"));
        assert!(!p.read_zone_allowed("rita", "code"));
        assert!(!p.read_zone_allowed("rita", "secret"));
        // bob: everything but code and secret
        assert!(p.read_zone_allowed("bob", "art"));
        assert!(!p.read_zone_allowed("bob", "code"));
        assert!(!p.read_zone_allowed("bob", "secret"));
        assert!(!Policy::parse("allow alice art **").unwrap().read_restricted("alice"));
        assert!(Policy::parse("allow-read rita art src/**").is_err());
        // read rules do not grant write access
        assert!(!p.zone_access("rita", "art"));
        // a policy with read rules only leaves write behaviour alone
        assert!(p.is_empty());
        assert!(!Policy::parse("allow-read a z **\nallow a z **").unwrap().is_empty());
    }

    #[test]
    fn shelve_read_rules() {
        let p = Policy::parse("shelve-read bob alice **\nshelve-read * carol **\n").unwrap();
        assert!(p.shelve_read_allowed("alice", "alice"));
        assert!(p.shelve_read_allowed("bob", "alice"));
        assert!(!p.shelve_read_allowed("bob", "dave"));
        assert!(!p.shelve_read_allowed("alice", "bob"));
        assert!(p.shelve_read_allowed("anyone", "carol"));
        // not a write rule, not a zone read restriction
        assert!(p.is_empty());
        assert!(!p.read_restricted("bob"));
        assert!(Policy::parse("shelve-read bob alice src/**").is_err());
    }

    #[test]
    fn require_signed_rules() {
        let p = Policy::parse("require-signed code main\nrequire-signed * release/*\n").unwrap();
        assert!(p.requires_signed("code", "main"));
        assert!(!p.requires_signed("art", "main"));
        assert!(p.requires_signed("art", "release/1"));
        assert!(!p.requires_signed("code", "dev"));
        assert!(p.is_empty(), "require-signed alone does not make writes deny-by-default");
        assert!(Policy::parse("require-signed code").is_err());
        assert!(Policy::parse("require-signed code main extra").is_err());
    }

    /// Path-normalization corpus: every spelling of a protected path that reaches the policy is
    /// treated like the protected path; spellings that can never be tree entries are refused by the
    /// object-name validator before any policy runs.
    #[test]
    fn normalization_corpus() {
        let p = Policy::parse("allow * z **\ndeny * z secrets/**\nrequire-lock * z art/*.psd\n").unwrap();
        let denied = [
            "secrets/k",
            "Secrets/k",
            "SECRETS/k",
            "secrets./k",
            "secrets /k",
            "secrets:stream/k",
            "\u{17f}ecrets/k",   // long s folds to s
            "s\u{200b}ecrets/k", // zero-width space
            "\u{ff53}ecrets/k",  // fullwidth s (NFKC)
            "secrets/deep/er/Name",
        ];
        for d in denied {
            assert!(
                matches!(p.check_path("bob", false, "z", d), Verdict::Denied(_)),
                "{d:?} must be denied"
            );
        }
        for ok in ["secret/k", "secretsx/k", "xsecrets/k", "docs/secrets.txt", "sec rets/k"] {
            assert_eq!(p.check_path("bob", false, "z", ok), Verdict::Ok, "{ok:?}");
        }
        for l in [
            "art/h.psd",
            "art/H.PSD",
            "art/h.psd.",
            "art/h.psd:stream",
            "ART/h.psd",
            "art/h.p\u{17f}d",
        ] {
            let want_lock = !l.ends_with("p\u{17f}d");
            let v = p.check_path("bob", false, "z", l);
            // `art/h.p\u{17f}d` folds to `art/h.psd` as well
            assert!(matches!(v, Verdict::NeedsLock(_)), "{l:?} (long-s expected={want_lock}) -> {v:?}");
        }
        assert_eq!(p.check_path("bob", false, "z", "art/h.txt"), Verdict::Ok);
        // spellings that are not valid tree entries never reach the policy
        use vcs_objects::names::validate_path;
        for bad in [
            "secrets/../x",
            "secrets//k",
            "./secrets/k",
            "/secrets/k",
            "secrets\\k",
            "secrets/\0k",
            "secrets/.",
        ] {
            assert!(validate_path(bad).is_err(), "{bad:?} must be refused by name validation");
        }
        // admin skips allow/deny but not require-lock, also through an alias
        assert!(matches!(p.check_path("root", true, "z", "ART/h.PSD"), Verdict::NeedsLock(_)));
        assert_eq!(p.check_path("root", true, "z", "Secrets/k"), Verdict::Ok);
    }

    #[test]
    fn policy_hash_tracks_content_and_order() {
        let a = Policy::parse("allow * z **\ndeny * z s/**\n").unwrap();
        assert_eq!(a.hash(), Policy::parse("# c\nallow * z **\n\ndeny * z s/**\n").unwrap().hash());
        assert_ne!(a.hash(), Policy::parse("deny * z s/**\nallow * z **\n").unwrap().hash());
        assert_ne!(a.hash(), Policy::parse("allow * z **\n").unwrap().hash());
        assert_eq!(a.hash().len(), 64);
    }

    #[test]
    fn parse_errors() {
        assert!(Policy::parse("allow alice art").is_err());
        assert!(Policy::parse("permit alice art x").is_err());
        assert!(Policy::parse("allow alice art /abs").is_err());
        assert!(Policy::parse("# only a comment\n\n").unwrap().is_empty());
    }
}
