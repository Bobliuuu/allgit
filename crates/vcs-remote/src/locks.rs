//! Lock validation: a pure function over (manifest, transaction, principal, time) evaluated
//! at the `Load` step against the very manifest version that the CAS will replace
//! (docs/locks.md). Because locks live in the manifest, the lock check and the branch-head
//! update are committed by one CAS: no check-then-act gap.

use crate::manifest::{Lock, Manifest};
use crate::protocol::PublishError;
use std::collections::{BTreeMap, BTreeSet};
use vcs_objects::{LockOp, Transaction};

/// Deliberately broken validators, used only by the model checker's mutation checks to prove
/// the invariants can fail. Never set in production code.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockFault {
    /// Do not compare the presented token with the current one.
    SkipTokenCheck,
    /// `Release` accepts a non-holder.
    ReleaseAnyone,
    /// Use raw clock input, ignoring the high-water mark.
    NoHighWater,
    /// Hand out the previous token again instead of a fresh one.
    ReuseTokens,
}

fn live(l: &Lock, now: u64) -> bool {
    now < l.expires_at
}

/// Apply the lock ops of `tx` to `new` (a clone of `cur`) and validate every net-changed path.
/// `changed(i)` returns the net changed paths of `tx.updates[i]`; it is only called for
/// zones that have at least one live lock.
pub fn apply(
    cur: &Manifest,
    new: &mut Manifest,
    tx: &Transaction,
    principal: &str,
    raw_now: u64,
    fault: Option<LockFault>,
    changed: &mut dyn FnMut(usize) -> Result<BTreeSet<String>, PublishError>,
) -> Result<(), PublishError> {
    let now = if fault == Some(LockFault::NoHighWater) {
        raw_now
    } else {
        raw_now.max(cur.time_hw)
    };
    let mut locks = cur.locks.clone();
    let mut next = cur.next_token;
    let mut acquired: BTreeSet<(String, String)> = BTreeSet::new();
    let mut presented: BTreeMap<(String, String), u64> = BTreeMap::new();
    let broken = |z: &str, p: &str| PublishError::LockBroken {
        zone: z.into(),
        path: p.into(),
    };
    for op in &tx.lock_ops {
        let key = (op.zone().to_owned(), op.path().to_owned());
        match op {
            LockOp::Acquire { lease_ms, reason, .. } => match locks.get_mut(&key) {
                Some(l) if live(l, now) && l.holder != principal => {
                    return Err(PublishError::LockedBy {
                        zone: key.0,
                        path: key.1,
                        holder: l.holder.clone(),
                        expires_at: l.expires_at,
                    })
                }
                Some(l) if live(l, now) => {
                    l.expires_at = now.saturating_add(*lease_ms);
                    acquired.insert(key);
                }
                _ => {
                    let token = if fault == Some(LockFault::ReuseTokens) && next > 0 {
                        next - 1
                    } else {
                        next += 1;
                        next - 1
                    };
                    locks.insert(
                        key.clone(),
                        Lock {
                            holder: principal.into(),
                            token,
                            acquired_at: now,
                            expires_at: now.saturating_add(*lease_ms),
                            reason: reason.clone(),
                        },
                    );
                    acquired.insert(key);
                }
            },
            LockOp::Renew { token, lease_ms, .. } => match locks.get_mut(&key) {
                Some(l) if live(l, now) && l.holder == principal && l.token == *token => {
                    l.expires_at = now.saturating_add(*lease_ms);
                }
                _ => return Err(broken(&key.0, &key.1)),
            },
            LockOp::Release { token, .. } => match locks.get(&key) {
                None => return Err(broken(&key.0, &key.1)),
                Some(l) if !live(l, now) => {
                    locks.remove(&key);
                }
                Some(l) => {
                    if l.holder != principal && fault != Some(LockFault::ReleaseAnyone) {
                        return Err(PublishError::NotHolder {
                            zone: key.0,
                            path: key.1,
                            holder: l.holder.clone(),
                        });
                    }
                    if l.token != *token && fault != Some(LockFault::ReleaseAnyone) {
                        return Err(broken(&key.0, &key.1));
                    }
                    locks.remove(&key);
                }
            },
            LockOp::Break { .. } => {
                locks.remove(&key);
            }
            LockOp::Present { token, .. } => {
                presented.insert(key, *token);
            }
        }
    }
    if fault != Some(LockFault::SkipTokenCheck) {
        for (key, token) in &presented {
            match locks.get(key) {
                Some(l) if live(l, now) && l.holder == principal && l.token == *token => {}
                _ => return Err(broken(&key.0, &key.1)),
            }
        }
    }
    for (i, u) in tx.updates.iter().enumerate() {
        let zone_locked = locks.iter().any(|((z, _), l)| *z == u.zone && live(l, now));
        if !zone_locked {
            continue;
        }
        for path in changed(i)? {
            let key = (u.zone.clone(), path.clone());
            let Some(l) = locks.get(&key).filter(|l| live(l, now)) else {
                continue;
            };
            if l.holder != principal {
                return Err(PublishError::LockedBy {
                    zone: u.zone.clone(),
                    path,
                    holder: l.holder.clone(),
                    expires_at: l.expires_at,
                });
            }
            let named = acquired.contains(&key) || presented.contains_key(&key);
            if !named && fault != Some(LockFault::SkipTokenCheck) {
                return Err(broken(&u.zone, &path));
            }
        }
    }
    new.locks = locks;
    new.next_token = next;
    new.time_hw = now;
    Ok(())
}
