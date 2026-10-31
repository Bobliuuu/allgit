# Locks and leases for unmergeable files

Status: **[design]**. Nothing here is implemented. Tracks issue #7. Related: [architecture.md](architecture.md) (remote
design, publish step machine), [object-model.md](object-model.md), `crates/vcs-remote/tests/model.rs` (checker style).

## Problem

Large binaries (textures, scenes, models) cannot be merged. Two people editing the same file means one of them loses
work, found out only at publish. A lock says "I am editing this, do not publish changes to it".

Commits are local-first, so a client can always commit a locked file locally. The lock therefore cannot be enforced by
the client. Today the only linearization point is the CAS on the remote `manifest`, and the backend only compares
opaque version tags: it cannot look inside a transaction and see which paths it changes. **Enforcement needs a component
that sees the transaction diff before the CAS succeeds** (the "validating writer" already named in the roadmap, M3).

## Decisions

| # | Question | Choice | Cost |
|---|---|---|---|
| L1 | Where do lock records live? | In the `manifest`, next to branch heads | every lock op is a manifest CAS (contention); no TOCTOU gap, see below |
| L2 | What does a lock cover? | One `(zone, path)` file, on all branches of the zone | cannot lock the same file independently per branch; directory/glob locks are future work |
| L3 | Who enforces? | A validating writer holding the only write credential to the manifest | a new trusted component; without it everything is advisory |
| L4 | Whose clock decides expiry? | The validating writer's clock only, with a never-decreasing high-water mark | clients cannot judge expiry authoritatively |
| L5 | How do stale writers get fenced? | Monotonic `token` per lock, checked at publish | one more field per record |

### Why locks live in the manifest, not a separate object
If locks were a second object with its own CAS, "check lock, then CAS the manifest" is two operations. Another writer can
acquire the lock between them. Putting locks in the manifest means the lock check and the branch-head update are
validated against the same manifest version and committed by the same CAS: if the manifest changed in between (a lock
taken, a lock broken), the CAS fails and the validator re-checks against the new version. This is the same argument that
chose one manifest over per-zone manifests. The cost is that renewals and acquisitions contend with publishes on one key;
sharding the manifest is deferred (see architecture.md) and gets harder here, because a transaction touching zones A and B
must see locks in both.

## Lock record

Added to the manifest (format version bump; old readers must reject, not ignore, a manifest with locks):

```
Manifest {
  ...existing fields...
  locks:     map (zone, path) -> Lock
  next_token: u64        // fencing counter, never reused
  time_hw:   u64         // high-water mark of validator time, unix ms
}
Lock {
  holder:      principal   // authenticated identity, NOT Transaction.author (self-asserted)
  token:       u64         // from next_token at acquire; unchanged by renew
  acquired_at: u64         // validator time
  expires_at:  u64         // validator time
  reason:      string      // optional, shown to others
}
```

Lock operations are carried as **lock ops inside a Transaction** (`Acquire`, `Renew`, `Release`, `Break`), so they get the
existing machinery for free: dependency-first upload (nothing to upload), `tx/<txid>` record, `applied` idempotency
(a retried acquire returns `AlreadyApplied`, never a second lock), CAS linearization. A transaction may combine lock ops
with ref updates atomically (for example "acquire and publish", "publish and release").

An expired lock stays in the manifest until some later transaction removes it (lazy reclaim); the validator treats it as
absent for all decisions.

## State machine (per `(zone, path)`)

```
            Acquire(p)                      Renew(p, token)
  Free ───────────────► Held(p, token, exp) ◄─────────────────┐
   ▲   (validator: now ≥ exp+ε of any   │ │ └─────────────────┘
   │    old lock, or none)              │ │
   │   Release(p, token) by p           │ │ Break(admin) at any time
   ├────────────────────────────────────┘ │
   │   time ≥ exp + ε (implicit, no write) │
   └───────────────────────────────────────┘
```

Rules, all evaluated by the validator at the `Load` step against the manifest it will CAS:

* **Acquire(path)**: succeeds iff no lock exists, or the existing lock is expired (`now ≥ expires_at + ε`), or the holder is
  the same principal (idempotent re-acquire, keeps its token). New lock gets `token = next_token++`.
* **Renew(path, token)**: only the holder, only with the current token, only while not expired (`now < expires_at + ε` is
  rejected-after, so a renew after expiry is just a fresh Acquire). Sets `expires_at = now + lease`. Lease length and
  maximum total hold time are per-zone policy; **values are to be chosen with data, none are proposed here**.
* **Release(path, token)**: the holder, or lazily when expired.
* **Break(path)**: see below.
* ε is the configured maximum clock error between validator replicas (0 with a single validator).

### Who can break a lock
1. The holder (that is Release).
2. Anyone, by Acquire, once the lease is expired. Nothing is "broken"; the old holder simply lost it.
3. A principal with the `lock-admin` role, any time. A break writes a tombstone note in `tx/<txid>` (who, whom, reason) so
   the old holder can be told. The old holder's unpublished commits are never touched: they fail at publish with
   `LockBroken` and stay local for the user to rescue (see publish below).

Roles need authentication and authorization, which do not exist today ("no authn/authz" in the known limitations).
Until they do, the principal is whatever the validator is configured to trust, and `Break` cannot be restricted.

## Publish with locks

Input: a Transaction with ref updates `(zone, branch, old → new)` and optionally lock ops. At `Load`, for each ref update the
validator computes the **net changed paths** between the `old` head commit's tree and the `new` commit's tree (add, modify,
delete, mode change, and both sides of a rename), then, for every changed path with a live lock, applies:

| Situation | Result |
|---|---|
| path has no live lock | allowed (unless `require-lock`, below) |
| live lock held by the tx's authenticated principal, and the tx names the lock's current `token` | allowed |
| live lock held by someone else | tx rejected whole: `LockedBy{path, holder, expires_at}` |
| tx names a token that is not the current one (lock broken, expired and re-taken) | rejected: `LockBroken{path}` |
| lock expired and unclaimed, no `require-lock` | allowed, treated as unlocked |
| `require-lock` policy matches the path and no live lock held by the principal | rejected: `LockRequired{path}`, unless the same tx contains `Acquire(path)` (acquire-on-publish, atomic) |

Rejection is whole-transaction (the existing atomicity: no partial publish). Nothing is applied, no manifest write
happens. Only the **net** diff of the published head matters: a commit that touches a locked file and a later commit that
reverts it nets to no change and is allowed. Intermediate commits are not inspected (they are content-addressed history, not
state).

`require-lock` is a per-zone policy: path globs for which holding a lock is mandatory to modify. It is what makes binaries
safe even when nobody remembered to lock. Without it, locks only protect files someone chose to lock.

### What the user sees when publish is rejected
The local commits remain. `vcs publish` prints the holder, expiry and the paths, and does not retry. The user's options:
wait/ask the holder, ask a `lock-admin` to break, or drop their change to those paths (`restore` from the remote head) and
publish the rest. There is no merge for these files, so no automatic resolution is offered. This differs from `Diverged`
(retry after fetch) because fetching does not fix a lock rejection.

## Offline behavior

| Action offline | Behavior |
|---|---|
| acquire / renew / release / break | impossible; they are remote CAS operations. The CLI says so and does not fake a local lock |
| edit and commit a locked or lockable file | allowed (local-first); advisory warning from the last fetched manifest |
| keep editing under an already held lease | allowed until the lease ends; the client stops trusting the lease at `expires_at - ε` by its own clock |
| publish later, lease still live and token current | allowed |
| publish later, lease expired but nobody took it | allowed (treated as unlocked), or atomically re-acquired under `require-lock` |
| publish later, someone else took or was given the lock | rejected `LockedBy`/`LockBroken`; local work preserved |

Offline work on a binary someone else locks is the loss case locks cannot prevent: the lock only stops the *publish*, not the
wasted editing. The client's best effort is the advisory display below.

## Advisory vs enforced

| Mechanism | Class | Notes |
|---|---|---|
| Rejecting publish of a locked path | **enforced**, only with the validating writer and authenticated principals | with direct manifest write access any client can bypass it |
| Fencing token check | **enforced** (same condition) | |
| Lease expiry decisions | **enforced**, validator clock | |
| `vcs status`/`log` showing "locked by X" | **advisory** | from the last fetched manifest; stale by design |
| Read-only worktree bit on files locked by others | **advisory** | filesystem hint, user can chmod |
| Warning at `add`/`commit` of a locked path | **advisory** | local-first means commit cannot be refused |
| `require-lock` | **enforced** (same condition as first row) | |
| Local-only lock (no remote) | **advisory**, labelled as such in output | matches the roadmap statement |

Anything that does not pass through the validating writer is advisory. Backend credentials that allow a client to PUT the
manifest directly void enforcement; deployments must not hand them out.

## Failure table

| Event | Outcome | Covered by |
|---|---|---|
| holder crashes while holding | lease expires; others Acquire after `expires_at + ε`. Holder's unpublished work survives locally | model: crash then time advance |
| holder crashes right after Acquire CAS, before ack | lock held; retry returns `AlreadyApplied`, same token | model: lost-ack |
| lost Acquire request | no lock; retry converges | model: fault at CAS |
| two writers acquire same path | exactly one CAS wins; loser sees `LockedBy` | model: acquire race |
| holder publishes while another breaks | break CAS and publish CAS race on the manifest; whichever wins, the other revalidates: publish after break gets `LockBroken` | model: break vs publish |
| validator crashes between Load and CAS | nothing written; client retries, revalidates against fresh manifest | model: crash at any step |
| client clock fast (thinks lease valid) | irrelevant to enforcement; validator rejects if truly expired and taken. Client may waste edits | model: skew param has no effect on invariants |
| client clock slow (thinks lease expired) | client stops editing early or re-acquires; no safety effect | |
| validator clock jumps backward | `time_hw` high-water mark: validator uses `max(now, time_hw)`, so leases never un-expire and never extend by a backward jump | model: non-monotonic clock input |
| validator clock jumps forward | leases expire early; holders get `LockBroken` at publish. Liveness-only loss, no double holder | model |
| two validator replicas with skewed clocks | locks may be judged expired up to ε early by one replica; ε margin on Acquire/Break-by-expiry keeps mutual exclusion if skew ≤ ε. **If skew > ε, mutual exclusion can fail**; this is an assumption, not a guarantee | stated assumption; model checks with skew ≤ ε only |
| offline writer edits locked file | publish rejected, work kept locally | unit/integration test (planned) |
| offline writer's lease expires, another acquires | `LockBroken` at publish | model: expiry then reacquire |
| stale token reused after re-acquire | rejected, tokens never reused | model: fencing invariant |
| lock holder leaves the project | lease expires, or admin Break | policy |
| manifest CAS contention from renewals | publishes retry via `Load`; disjoint writers still succeed | **unmeasured**; benchmark before choosing lease lengths |
| backend lacks conditional PUT | cannot be authoritative (unchanged rule) | architecture.md |
| client bypasses validator | enforcement void; advisory only | deployment rule above |

## Model checker plan

Extend `crates/vcs-remote/tests/model.rs` (same exhaustive-interleaving style, same fault injection), in a new
`tests/model_locks.rs` sharing helpers.

**Model.** The validator is a pure function `validate(manifest, tx, now) -> Accept(new_manifest) | Reject(reason)` called at
`Load`; `Cas` is as today. Lock ops are new `Step` variants of the publisher, so all existing faults (crash at any step,
lost request, lost ack) apply to them unchanged. Time is an explicit schedule event `Tick` (bounded count, e.g. 3) so
expiry interleaves with every other step. Each writer carries its own skewed client clock (input to client-side decisions
only) and the validator input clock may additionally be fed one backward jump and one forward jump.

**Scenarios** (each with 2 or 3 writers, 1 path unless noted):
1. acquire race (both Acquire; exactly one holds)
2. holder publishes while contender tries to publish same path without lock (contender rejected, holder accepted)
3. holder crashes, tick past expiry, contender acquires and publishes, original holder recovers and retries publish (rejected `LockBroken`)
4. break vs publish race (admin Break vs holder publish)
5. renew vs expiry vs acquire by contender
6. lock ops combined with multi-zone ref updates (atomicity: rejected tx leaves no partial effect)
7. duplicate submission of an Acquire (applied once, same token)
8. `require-lock`: acquire-on-publish atomic, and unlocked publish rejected
9. clock anomalies: backward and forward jumps of the validator clock

**Invariants** checked on the final state and on every manifest version in `b.history()`:
* L-ATOMIC: each manifest version = previous + exactly one whole transaction (existing check, extended to `locks`, `next_token`).
* L-EXCL: at every version and every path, at most one lock record, and a tx accepted at version *v* never changes a path
  locked live at *v* by another principal (recomputed independently from the stored `tx/` record, not from validator
  output, to avoid checking the validator with itself).
* L-FENCE: `next_token` strictly increases; no two lock records across history share a token; an accepted publish names the
  token current at its CAS version.
* L-EXPIRY: an Acquire by another principal is accepted only if the previous lock was expired by `now ≥ expires_at + ε` at
  the validator time recorded for that version; `time_hw` never decreases.
* L-IDEM: no lock-op tx applied twice.
* L-VISIBLE: a reported success is in the final state; a reported rejection is not (as in the existing outcome check).
* L-LIVE (liveness, checked in the recovery phase): after enough `Tick`s with all faults off, any writer can Acquire a free or
  expired path and publish.
* Existing closure and atomicity invariants still hold with lock ops mixed in.

**Mutation checks** (each must make the model fail, as done for idempotency and upload order today): drop the ε margin with
skewed replicas; evaluate locks outside the CAS'd manifest version (check-then-CAS TOCTOU); reuse tokens; skip the token
comparison at publish; let `Release` accept a non-holder; let the validator clock go backward without `time_hw`.

**Bounds.** Schedule count grows with the added step kinds; keep 2 writers plus one admin in at most one scenario, and
print schedule counts like the existing tests. Assert a lower bound on explored schedules so an accidentally tiny search fails.

**Not covered by the model:** real clocks, S3 behavior, multi-replica validators beyond the ε assumption, authentication, and
contention cost. Those need integration tests and a measurement.

## Open questions and non-goals
* Directory and glob locks; locking per branch; lock inheritance across renames (a rename is treated as touching both paths).
* Lease and hold-time defaults: measure CAS contention first.
* Lock-admin and principal model: depends on the authn/authz work.
* Large manifests: a repo with many binaries held at once grows the manifest; may force the sharding decision earlier.
* This doc adds no code. Implementation issues (manifest format v2, validator, CLI, model) should be filed as separate tasks once this is accepted.
