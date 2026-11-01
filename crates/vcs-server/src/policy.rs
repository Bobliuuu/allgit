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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub effect: Effect,
    pub principal: String,
    pub zone: String,
    pub glob: String,
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
            if f.len() != 4 {
                return Err(err(
                    "expected `<allow|deny|require-lock|allow-read|deny-read> <principal|*> <zone|*> <glob>`",
                ));
            }
            let effect = match f[0] {
                "allow" => Effect::Allow,
                "deny" => Effect::Deny,
                "require-lock" => Effect::RequireLock,
                "allow-read" => Effect::AllowRead,
                "deny-read" => Effect::DenyRead,
                _ => return Err(err("effect must be allow, deny, require-lock, allow-read or deny-read")),
            };
            if matches!(effect, Effect::AllowRead | Effect::DenyRead) && f[3] != "**" {
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

    /// True when there is no write-side rule (`allow`, `deny`, `require-lock`). Read rules alone do
    /// not switch writes to deny-by-default.
    pub fn is_empty(&self) -> bool {
        self.rules.iter().all(|r| matches!(r.effect, Effect::AllowRead | Effect::DenyRead))
    }

    /// Does any read rule name this principal (or `*`)? Only then are its reads checked at all.
    pub fn read_restricted(&self, principal: &str) -> bool {
        self.rules
            .iter()
            .any(|r| matches!(r.effect, Effect::AllowRead | Effect::DenyRead) && (r.principal == "*" || r.principal == principal))
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
            if let Some(r) = hit(Effect::Deny) {
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
        if self
            .rules
            .iter()
            .any(|r| r.effect == Effect::RequireLock && r.applies(principal, zone) && glob_match(&r.glob, path))
        {
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
    fn parse_errors() {
        assert!(Policy::parse("allow alice art").is_err());
        assert!(Policy::parse("permit alice art x").is_err());
        assert!(Policy::parse("allow alice art /abs").is_err());
        assert!(Policy::parse("# only a comment\n\n").unwrap().is_empty());
    }
}
