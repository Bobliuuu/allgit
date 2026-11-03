//! Server-side validation of a manifest write (docs/locks.md "Publish with locks",
//! docs/research/server.md 3.1). The client computes the new manifest as today; the server
//! re-checks it against the manifest it replaces, using the authenticated principal.

use crate::auth::{Principal, Role};
use crate::policy::{Policy, Verdict};
use std::collections::BTreeSet;
use vcs_objects::signing::{self, SigStatus, Trust};
use vcs_objects::{Object, ObjectId};
use vcs_remote::{changed_paths, fetch_object, Backend, Manifest, PublishError};

#[derive(Debug, PartialEq, Eq)]
pub enum Reject {
    /// 403: the principal's role/zone policy forbids this write.
    Forbidden(String),
    /// 409: a changed path is locked by someone else (or a live foreign lock is altered).
    LockedBy {
        zone: String,
        path: String,
        holder: String,
        expires_at: u64,
    },
    /// 422: the new manifest is malformed or inconsistent with the old one.
    Invalid(String),
    /// 428: policy `require-lock`: the path may only change under a lock held by the writer.
    LockRequired { zone: String, path: String },
    /// 503: backend trouble while validating.
    Unavailable(String),
}

/// Allowed forward distance of `time_hw` from the server clock (ms), to absorb replica skew.
pub const TIME_SKEW_MS: u64 = 60_000;

/// Most the fencing counter `next_token` may advance in one write (one token per lock granted).
/// It can never decrease, so an unbounded jump would let one writer exhaust it for good.
pub const MAX_TOKEN_JUMP: u64 = 1 << 16;

/// Default longest lease a client may ask for (7 days).
pub const DEFAULT_MAX_LEASE_MS: u64 = 7 * 24 * 3600 * 1000;

/// Validator settings beyond roles: the path policy, lease cap and clock-skew allowance.
#[derive(Clone, Debug)]
pub struct ValidatorConfig {
    /// Per-zone path-glob policy; empty = only the token-level zone lists apply.
    pub policy: Policy,
    /// Largest lease (ms) a lock acquire/renew may request; larger requests are refused.
    pub max_lease_ms: u64,
    /// Replica clock-skew allowance (ms): locks stay live this long past `expires_at`.
    /// Set to at least twice the largest clock offset between server replicas on one backend.
    pub skew_epsilon_ms: u64,
    /// Allowed and revoked signing keys, for `require-signed` rules (docs/signing.md).
    pub signers: Trust,
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            policy: Policy::default(),
            max_lease_ms: DEFAULT_MAX_LEASE_MS,
            skew_epsilon_ms: 0,
            signers: Trust::default(),
        }
    }
}

fn live(expires_at: u64, now: u64, eps: u64) -> bool {
    now < expires_at.saturating_add(eps)
}

/// A branch head a manifest write replaced with something that does not descend from it
/// (`new == Some(_)`, "force") or removed (`new == None`, "delete"). Only these are recorded in the
/// server's audit log as `FORCE` / `DELETE-BRANCH`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rewrite {
    pub zone: String,
    pub branch: String,
    pub old: ObjectId,
    pub new: Option<ObjectId>,
    /// The ancestry walk hit its bound, so descent could not be established either way.
    pub unverified: bool,
}

/// Validate with default settings, discarding the normalized manifest.
pub fn validate_manifest_write(
    backend: &dyn Backend,
    cur: &Manifest,
    new: &Manifest,
    who: &Principal,
    server_now: u64,
) -> Result<(), Reject> {
    validate_and_normalize(backend, cur, &mut new.clone(), who, server_now, &ValidatorConfig::default())
}

/// Validate `new` against `cur` and rewrite it where the server is the authority: lock
/// expiry is recomputed from the server clock (the client's `expires_at` only conveys the
/// requested lease, `expires_at - time_hw`), `acquired_at` of fresh locks is the server time,
/// and `time_hw` becomes the server high-water mark. The caller stores the rewritten `new`.
pub fn validate_and_normalize(
    backend: &dyn Backend,
    cur: &Manifest,
    new: &mut Manifest,
    who: &Principal,
    server_now: u64,
    cfg: &ValidatorConfig,
) -> Result<(), Reject> {
    validate_and_report(backend, cur, new, who, server_now, cfg, None)
}

/// Like [`validate_and_normalize`]; with `report`, also lists every forced move and deletion (this
/// walks history from each changed head to its old head, so only the server asks for it).
pub fn validate_and_report(
    backend: &dyn Backend,
    cur: &Manifest,
    new: &mut Manifest,
    who: &Principal,
    server_now: u64,
    cfg: &ValidatorConfig,
    mut report: Option<&mut Vec<Rewrite>>,
) -> Result<(), Reject> {
    let eps = cfg.skew_epsilon_ms;
    let live = |e: u64, n: u64| live(e, n, eps);
    if who.role < Role::Write {
        return Err(Reject::Forbidden("read-only principal cannot write the manifest".into()));
    }
    let now = server_now.max(cur.time_hw);

    // Monotonic counters; clients cannot move time or fencing tokens backwards or far ahead.
    if new.next_token < cur.next_token {
        return Err(Reject::Invalid("next_token decreased".into()));
    }
    if new.next_token - cur.next_token > MAX_TOKEN_JUMP {
        return Err(Reject::Invalid(format!(
            "next_token jumped by more than {MAX_TOKEN_JUMP} in one write"
        )));
    }
    if new.time_hw < cur.time_hw {
        return Err(Reject::Invalid("time_hw decreased".into()));
    }
    if new.time_hw > now.saturating_add(TIME_SKEW_MS) {
        return Err(Reject::Invalid("time_hw is ahead of the server clock".into()));
    }

    // Server-computed lease expiry. A lock record that differs from the stored one asks for
    // a lease of `expires_at - time_hw` (the client set both from one clock reading); the
    // server restarts that lease at its own `now`, so client clocks never decide expiry.
    let client_hw = new.time_hw;
    for (k, l) in new.locks.iter_mut() {
        let prev = cur.locks.get(k);
        if prev == Some(&*l) {
            continue;
        }
        let lease = l.expires_at.saturating_sub(client_hw);
        if lease > cfg.max_lease_ms {
            return Err(Reject::Invalid(format!(
                "lease of {lease} ms on {} in zone {} exceeds the server maximum of {} ms",
                k.1, k.0, cfg.max_lease_ms
            )));
        }
        l.acquired_at = match prev {
            Some(p) if p.token == l.token => p.acquired_at,
            _ => now,
        };
        l.expires_at = now.saturating_add(lease);
    }
    if new.has_lock_state() {
        new.time_hw = now;
    }
    let new: &Manifest = new;

    // Per-zone write policy: every branch head and zone definition that changes.
    let mut changed_heads = vec![];
    let keys: BTreeSet<&(String, String)> = cur.branches.keys().chain(new.branches.keys()).collect();
    for k in keys {
        let (o, n) = (cur.branches.get(k).copied(), new.branches.get(k).copied());
        if o != n {
            if !who.may_write_zone(&k.0) {
                return Err(Reject::Forbidden(format!("principal {} may not write zone {}", who.name, k.0)));
            }
            policy_zone(cfg, who, &k.0)?;
            match (o, n) {
                (_, Some(n)) => changed_heads.push((k.0.clone(), k.1.clone(), o, n)),
                (Some(old), None) => {
                    if cfg.policy.protects_branch(&k.0, &k.1) {
                        return Err(Reject::Forbidden(format!(
                            "policy protect-branch: {}/{} cannot be deleted",
                            k.0, k.1
                        )));
                    }
                    if let Some(r) = report.as_deref_mut() {
                        r.push(Rewrite {
                            zone: k.0.clone(),
                            branch: k.1.clone(),
                            old,
                            new: None,
                            unverified: false,
                        });
                    }
                }
                (None, None) => {}
            }
        }
    }
    for z in cur.zones.keys().chain(new.zones.keys()) {
        if cur.zones.get(z) != new.zones.get(z) && !who.may_write_zone(z) {
            return Err(Reject::Forbidden(format!("principal {} may not define zone {z}", who.name)));
        }
        if cur.zones.get(z) != new.zones.get(z) {
            policy_zone(cfg, who, z)?;
        }
    }

    // Lock record changes.
    let lock_keys: BTreeSet<&(String, String)> = cur.locks.keys().chain(new.locks.keys()).collect();
    for k in lock_keys {
        let (o, n) = (cur.locks.get(k), new.locks.get(k));
        if o == n {
            continue;
        }
        if !who.may_write_zone(&k.0) {
            return Err(Reject::Forbidden(format!(
                "principal {} may not change locks in zone {}",
                who.name, k.0
            )));
        }
        let foreign_live = o.filter(|l| live(l.expires_at, now) && l.holder != who.name);
        if let Some(n) = n {
            // A changed lock record always names the writer: nobody may renew or re-stamp a lock in
            // another principal's name, live or expired (an unchanged record is skipped above).
            if n.holder != who.name {
                return Err(Reject::Forbidden(format!(
                    "lock holder {:?} is not the authenticated principal {:?}",
                    n.holder, who.name
                )));
            }
            if let Some(l) = foreign_live {
                // Only an admin may touch a live lock of someone else (break/replace).
                if who.role < Role::Admin || n.holder == l.holder {
                    return Err(Reject::LockedBy {
                        zone: k.0.clone(),
                        path: k.1.clone(),
                        holder: l.holder.clone(),
                        expires_at: l.expires_at,
                    });
                }
            }
        } else if let Some(l) = foreign_live.filter(|_| who.role < Role::Admin) {
            return Err(Reject::Forbidden(format!(
                "only an admin may break {}'s lock on {} in zone {}",
                l.holder, k.1, k.0
            )));
        }
    }

    // Net changed paths vs. the locks that will be in force after this write.
    for (zone, branch, old, newh) in changed_heads {
        let protected = cfg.policy.protects_branch(&zone, &branch);
        if let (Some(o), true) = (old, protected || report.is_some()) {
            match descends(backend, o, newh)? {
                Some(true) => {}
                d => {
                    if protected && d == Some(false) {
                        return Err(Reject::Forbidden(format!(
                            "policy protect-branch: {zone}/{branch} only accepts fast-forward updates (this would replace {} with {})",
                            o.short(),
                            newh.short()
                        )));
                    }
                    if protected {
                        return Err(Reject::Invalid(format!(
                            "protect-branch: history between {} and {} too large to check",
                            o.short(),
                            newh.short()
                        )));
                    }
                    if let Some(r) = report.as_deref_mut() {
                        r.push(Rewrite {
                            zone: zone.clone(),
                            branch: branch.clone(),
                            old: o,
                            new: Some(newh),
                            unverified: d.is_none(),
                        });
                    }
                }
            }
        }
        if cfg.policy.requires_signed(&zone, &branch) {
            check_signed(backend, &cfg.signers, &zone, &branch, old, newh)?;
        }
        let zone_locked = new.locks.iter().any(|((z, _), l)| *z == zone && live(l.expires_at, now));
        if !zone_locked && cfg.policy.is_empty() {
            continue;
        }
        let paths = changed_paths(backend, old, newh).map_err(|e| match e {
            PublishError::Backend(b) => Reject::Unavailable(b.to_string()),
            e => Reject::Invalid(format!("cannot diff {zone}: {e}")),
        })?;
        for p in paths {
            let verdict = if cfg.policy.is_empty() {
                Verdict::Ok
            } else {
                cfg.policy.check_path(&who.name, who.role >= Role::Admin, &zone, &p)
            };
            match verdict {
                Verdict::Ok => {}
                Verdict::Denied(m) => return Err(Reject::Forbidden(m)),
                Verdict::NeedsLock(_) => {
                    let held = new
                        .locks
                        .get(&(zone.clone(), p.clone()))
                        .is_some_and(|l| live(l.expires_at, now) && l.holder == who.name);
                    if !held {
                        // a foreign live lock is reported as such below
                        if !new
                            .locks
                            .get(&(zone.clone(), p.clone()))
                            .is_some_and(|l| live(l.expires_at, now) && l.holder != who.name)
                        {
                            return Err(Reject::LockRequired { zone, path: p });
                        }
                    }
                }
            }
            if let Some(l) = new.locks.get(&(zone.clone(), p.clone())) {
                if live(l.expires_at, now) && l.holder != who.name {
                    return Err(Reject::LockedBy {
                        zone,
                        path: p,
                        holder: l.holder.clone(),
                        expires_at: l.expires_at,
                    });
                }
            }
        }
    }
    Ok(())
}

/// Policy gate for zone-level changes (branch heads, zone definitions): deny by default.
fn policy_zone(cfg: &ValidatorConfig, who: &Principal, zone: &str) -> Result<(), Reject> {
    if cfg.policy.is_empty() || who.role >= Role::Admin || cfg.policy.zone_access(&who.name, zone) {
        return Ok(());
    }
    Err(Reject::Forbidden(format!(
        "policy: principal {} has no allow rule for zone {zone} (deny-by-default)",
        who.name
    )))
}

/// Most commits walked on either side while checking signatures; beyond it the publish is refused
/// rather than letting one request make the server read an unbounded history.
const MAX_SIGNED_WALK: usize = 100_000;

/// Is `old` an ancestor of (or equal to) `new`? `None` when the walk bound is hit first.
fn descends(backend: &dyn Backend, old: ObjectId, new: ObjectId) -> Result<Option<bool>, Reject> {
    let mut seen: BTreeSet<ObjectId> = BTreeSet::new();
    let mut stack = vec![new];
    while let Some(id) = stack.pop() {
        if id == old {
            return Ok(Some(true));
        }
        if !seen.insert(id) {
            continue;
        }
        if seen.len() > MAX_SIGNED_WALK {
            return Ok(None);
        }
        stack.extend(commit_of(backend, &id)?.parents);
    }
    Ok(Some(false))
}

fn commit_of(backend: &dyn Backend, id: &ObjectId) -> Result<vcs_objects::Commit, Reject> {
    match fetch_object(backend, id) {
        Ok(Object::Commit(c)) => Ok(c),
        Ok(_) => Err(Reject::Invalid(format!("{id} is not a commit"))),
        Err(PublishError::Backend(b)) => Err(Reject::Unavailable(b.to_string())),
        Err(e) => Err(Reject::Invalid(format!("cannot read commit {id}: {e}"))),
    }
}

/// `require-signed`: every commit reachable from `new` but not from `old` must be validly signed by
/// an allowed, unrevoked key. Runs inside the manifest CAS validator, so no unsigned history can
/// become a branch head through this server.
fn check_signed(
    backend: &dyn Backend,
    trust: &Trust,
    zone: &str,
    branch: &str,
    old: Option<ObjectId>,
    new: ObjectId,
) -> Result<(), Reject> {
    let mut have: BTreeSet<ObjectId> = BTreeSet::new();
    let mut stack: Vec<ObjectId> = old.into_iter().collect();
    while let Some(id) = stack.pop() {
        if !have.insert(id) {
            continue;
        }
        if have.len() > MAX_SIGNED_WALK {
            return Err(Reject::Invalid("require-signed: existing history too large to check".into()));
        }
        stack.extend(commit_of(backend, &id)?.parents);
    }
    let mut seen: BTreeSet<ObjectId> = BTreeSet::new();
    let mut stack = vec![new];
    while let Some(id) = stack.pop() {
        if have.contains(&id) || !seen.insert(id) {
            continue;
        }
        if seen.len() > MAX_SIGNED_WALK {
            return Err(Reject::Invalid("require-signed: too many new commits to check".into()));
        }
        let c = commit_of(backend, &id)?;
        match signing::verify_commit(&c, trust) {
            SigStatus::Good { .. } => {}
            other => {
                let why = match other {
                    SigStatus::Unsigned => "is not signed".to_owned(),
                    SigStatus::Bad(w) => format!("has an invalid signature ({w})"),
                    SigStatus::UnknownKey { fingerprint } => format!("is signed by a key that is not allowed ({fingerprint})"),
                    SigStatus::Revoked { fingerprint } => format!("is signed by a revoked key ({fingerprint})"),
                    SigStatus::Good { .. } => unreachable!(),
                };
                return Err(Reject::Forbidden(format!(
                    "policy require-signed: commit {} {why}; {zone}/{branch} only accepts commits signed by an allowed signer",
                    id.short()
                )));
            }
        }
        stack.extend(c.parents);
    }
    Ok(())
}
