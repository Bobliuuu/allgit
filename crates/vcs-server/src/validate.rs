//! Server-side validation of a manifest write (docs/locks.md "Publish with locks",
//! docs/research/server.md 3.1). The client computes the new manifest as today; the server
//! re-checks it against the manifest it replaces, using the authenticated principal.

use crate::auth::{Principal, Role};
use std::collections::BTreeSet;
use vcs_remote::{changed_paths, Backend, Manifest, PublishError};

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
    /// 503: backend trouble while validating.
    Unavailable(String),
}

/// Allowed forward distance of `time_hw` from the server clock (ms), to absorb replica skew.
pub const TIME_SKEW_MS: u64 = 60_000;

fn live(expires_at: u64, now: u64) -> bool {
    now < expires_at
}

pub fn validate_manifest_write(
    backend: &dyn Backend,
    cur: &Manifest,
    new: &Manifest,
    who: &Principal,
    server_now: u64,
) -> Result<(), Reject> {
    if who.role < Role::Write {
        return Err(Reject::Forbidden("read-only principal cannot write the manifest".into()));
    }
    let now = server_now.max(cur.time_hw);

    // Monotonic counters; clients cannot move time or fencing tokens backwards or far ahead.
    if new.next_token < cur.next_token {
        return Err(Reject::Invalid("next_token decreased".into()));
    }
    if new.time_hw < cur.time_hw {
        return Err(Reject::Invalid("time_hw decreased".into()));
    }
    if new.time_hw > now.saturating_add(TIME_SKEW_MS) {
        return Err(Reject::Invalid("time_hw is ahead of the server clock".into()));
    }

    // Per-zone write policy: every branch head and zone definition that changes.
    let mut changed_heads = vec![];
    let keys: BTreeSet<&(String, String)> = cur.branches.keys().chain(new.branches.keys()).collect();
    for k in keys {
        let (o, n) = (cur.branches.get(k).copied(), new.branches.get(k).copied());
        if o != n {
            if !who.may_write_zone(&k.0) {
                return Err(Reject::Forbidden(format!("principal {} may not write zone {}", who.name, k.0)));
            }
            if let Some(n) = n {
                changed_heads.push((k.0.clone(), o, n));
            }
        }
    }
    for z in cur.zones.keys().chain(new.zones.keys()) {
        if cur.zones.get(z) != new.zones.get(z) && !who.may_write_zone(z) {
            return Err(Reject::Forbidden(format!("principal {} may not define zone {z}", who.name)));
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
            if n.holder != who.name && n.holder != o.map(|l| l.holder.clone()).unwrap_or_default() {
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
    for (zone, old, newh) in changed_heads {
        if !new.locks.iter().any(|((z, _), l)| *z == zone && live(l.expires_at, now)) {
            continue;
        }
        let paths = changed_paths(backend, old, newh).map_err(|e| match e {
            PublishError::Backend(b) => Reject::Unavailable(b.to_string()),
            e => Reject::Invalid(format!("cannot diff {zone}: {e}")),
        })?;
        for p in paths {
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
