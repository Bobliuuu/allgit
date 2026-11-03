# Validating-writer server: survey, proposal and threat model

Status: **[design / research]**. Nothing here is implemented. Tracks issue #99. Related: [../locks.md](../locks.md) (assumes this
component), [../architecture.md](../architecture.md) (remote design), [../research.md](../research.md).

> **Provenance.** Sources were fetched on 2026-10-31 (date accessed for every URL below). Where a fetch returned only a table of
> contents or nothing relevant, the claim is marked **[unverified]**. No performance or capacity numbers appear in this
> document; anything quantitative is deferred to a measurement task.

## 1. Why a server at all

`locks.md` (L3) needs a component that sees the transaction diff before the manifest CAS succeeds. The same component is the only
place where authenticated identity, per-zone permissions, audit and rate limits can be enforced, because the object store only
compares opaque version tags. Without it, everything is advisory (locks.md, "Advisory vs enforced").

## 2. Survey

### 2.1 Perforce Helix Core
* **Authn.** Default method authenticates against the server's own user table; LDAP can be configured without authentication
  triggers; ticket-based login exists. Source: [P4 Command Reference configurables](https://ftp.perforce.com/perforce/r19.1/doc/manuals/cmdref/Content/CmdRef/configurables.configurables.html),
  [Server Admin Guide, protections](https://www.perforce.com/manuals/v19.1/p4sag/Content/P4SAG/protections.set.html) (accessed 2026-10-31).
  Ticket lifetime and brute-force handling: **[unverified]**, the fetched page was only an introduction.
* **Authz.** A protections table: lines of (permission level, user/group, host, path). Admin level covers creating depots/triggers,
  editing protections and shutting down. Same source. Before the first `p4 protect`, every user is a superuser (same source).
* **Locks.** `p4 lock` stops other users from submitting an opened file; it fails if another user holds it; released when the
  holder submits; requires `write` access; `server.locks.global` makes edge-server locks global.
  Source: [p4 lock](https://help.perforce.com/helix-core/server-apps/cmdref/current/Content/CmdRef/p4_lock.html) (accessed 2026-10-31).
  Who may force-unlock: **[unverified]**.
* **Audit, rate limits, triggers, replication.** Triggers exist and auth triggers are mentioned (same configurables page); trigger
  semantics, audit logging and rate limiting: **[unverified]** (page not retrieved; the fetch of the triggers page returned 404).
* **Takeaway.** Locks are server state checked at submit, not client advice, which is exactly the model of locks.md. The central
  server is the availability point (research.md already notes this).

### 2.2 Sapling Mononoke
* Server for Sapling, written in Rust, with MySQL, SQLite, file and S3 storage backends in the open source build; MySQL failover
  is not supported (process restart needed). Source: [Mononoke README](https://raw.githubusercontent.com/facebook/sapling/main/eden/mononoke/README.md) (accessed 2026-10-31).
* The docs index lists a Bookmark Service, server-side pushrebase, and push-time hooks of bookmark, changeset and file kinds
  ([Mononoke docs index](https://raw.githubusercontent.com/facebook/sapling/main/eden/mononoke/docs/README.md), accessed 2026-10-31).
  That page is a table of contents; **how bookmarks are stored/moved, ACLs, authentication and audit are not described there: [unverified]**.
* Sapling README says Mononoke is used in production at Meta but is not supported for external use
  ([facebook/sapling](https://github.com/facebook/sapling), via search result, accessed 2026-10-31).
* **Takeaway.** Policy runs as hooks over the pushed change before a ref moves; we have the same shape (validate net diff, then CAS).
  We cannot cite its authn/authz design.

### 2.3 Gerrit
* **Authn.** Not-signed-in users are only in Anonymous Users; signing in adds Registered Users; with OpenID, effective groups are
  limited unless identities match `auth.trustedOpenID`.
* **Authz.** Group-based only ("Access rights cannot be granted to individual users"); per-ref patterns (exact, namespace, regex);
  BLOCK overrides child grants; exclusive flag; label vote ranges; inheritance from a root config. Source:
  [Gerrit access control](https://gerrit-review.googlesource.com/Documentation/access-control.html) (accessed 2026-10-31).
* **Audit and rate limits.** That page describes neither (it mentions forced pushes are logged). The config page names request
  logs for httpd and sshd only in passing; quota and rate limiting not found
  ([config-gerrit](https://gerrit-review.googlesource.com/Documentation/config-gerrit.html), first 100k of ~267k characters read, accessed 2026-10-31). **[unverified]** beyond that.
* **Multi-replica.** Replica mode is read-only and rejects pushes; in multi-primary clusters some caches must be disabled because
  "change updates are not communicated between Gerrit servers" (same config page).
* Submit records are stored in NoteDb (git-backed) with rule names ([Gerrit change a300b1d](https://gerrit.googlesource.com/gerrit/+/a300b1d00e8978b56e18b7ddc8c958fd9602266a), search snippet only, accessed 2026-10-31).
* **Takeaway.** Group-based roles with per-ref patterns map well to per-zone, per-path-glob permissions. Cache incoherence in
  multi-primary is a warning: our server must keep no authoritative state outside the manifest.

### 2.4 WalGit
* Single binary in front of S3/GCS/in-memory store, no database; machines are disposable caches. `manifest.pb` is the
  linearization point, updated by CAS; on a 412 the server re-reads, re-validates each ref's old value and retries; any
  instance can accept a push; many `serve` hosts share a bucket; roles `serve`/`maintain`/`events`.
  Auth modes: `none` (everyone anonymous with write/admin, loopback only), static `token` (Bearer or Basic), `oidc` with
  server-issued HMAC-signed access tokens. `leases/` holds CAS-based leases with a TTL and is the only cross-instance mutex.
  Source: [tobi/walgit README](https://github.com/tobi/walgit) (accessed 2026-10-31). Only the README was read, not the code.
  Audit logs and rate limits: **[unverified]** (not mentioned).
* Note: research.md previously flagged WalGit as unread; this entry replaces that gap at README level only.
* **Takeaway.** Confirms stateless replicas plus manifest CAS. Its leases are infrastructure mutexes, not user file locks, and
  nothing in the README shows a validator that inspects tree diffs.

### 2.5 Object-store primitives we rely on
* S3 `If-None-Match` (create-only) and `If-Match` (ETag CAS) on PutObject; conflicts return 412, and 409 can occur on concurrent
  modification; bucket policy condition keys `s3:if-match` / `s3:if-none-match` with a Deny+Null condition reject writes missing
  the header; non-conditional writers can bypass conditional logic on the same key, so policy must cover all writers and
  CompleteMultipartUpload. Sources: [AWS What's New, 2024-11-25](https://aws.amazon.com/about-aws/whats-new/2024/11/amazon-s3-functionality-conditional-writes),
  [AWS Storage Blog](https://aws.amazon.com/blogs/storage/building-multi-writer-applications-on-amazon-s3-using-native-controls) (both accessed 2026-10-31).
  The 409 handling detail was truncated in the fetch: **[unverified]**.
* Access-point or bucket-policy restriction of the manifest key to one IAM principal: standard IAM, not re-verified here; see the
  deployment task for a test against a real bucket. **[unverified]**

### 2.6 Comparison

| Concern | Perforce | Mononoke | Gerrit | WalGit | Ours (proposed) |
|---|---|---|---|---|---|
| Authn | built-in users, LDAP, tickets | [unverified] | OpenID/HTTP/LDAP, groups | none/token/OIDC | bearer tokens (hashed), OIDC later |
| Authz | protections table (path, host, user) | hooks; ACL [unverified] | group ACL per ref pattern, labels | [unverified] | roles per zone + path globs |
| Locks | server `p4 lock` | [unverified] | none documented in fetched pages | CAS leases (infra only) | manifest-resident (locks.md) |
| Audit | [unverified] | [unverified] | logs; submit records in NoteDb | [unverified] | immutable `tx/` + `audit/` objects |
| Rate limits | [unverified] | [unverified] | none found | [unverified] | per-principal limiter (section 3.6) |
| Multi-replica | central server | [unverified] | read replicas; multi-primary caveats | stateless, manifest CAS | stateless, manifest CAS |
| Clock | n/a in sources | n/a | n/a | lease TTL, details [unverified] | validator time + high-water mark |

## 3. Proposal: `vcs-server`

Principle: **one new trusted process, no new state.** Everything durable stays in the bucket, as in architecture.md.

```
 client (vcs) ── HTTPS ──► vcs-server ──(sole credential: PUT manifest, If-Match)──► bucket
      │                        │ reads: manifest, tx/, trees (to compute the net diff)
      └─ scoped create-only PUT of objects/ (immutable, content-addressed) ─────────► bucket
```

### 3.1 What the server does and does not do
* Does: authenticate; authorize; load the manifest; verify the transaction's closure exists; compute the net changed paths per
  ref update (locks.md "Publish with locks"); evaluate lock rules and permission rules; stamp validator time; CAS the manifest;
  write the `tx/<txid>` record and an audit record; return the outcome.
* Does not: store objects (clients upload them directly, dependency-first as today), merge, or hold state across requests.
  Keeping object bytes off the server keeps it small and removes it from the bandwidth path.
* A server crash at any point leaves the same states the existing step machine and model already cover (nothing written, or
  written and idempotent via `applied`).

### 3.2 API (v1, JSON over HTTPS)
All requests carry `Authorization: Bearer <token>`. Errors are typed (`Unauthorized`, `Forbidden`, `LockedBy`, `LockBroken`,
`LockRequired`, `Diverged`, `RateLimited`, `MissingObject`).

| Method and path | Purpose |
|---|---|
| `GET /v1/manifest` | current manifest and its version tag (read-through; clients may instead read the bucket if given read access) |
| `POST /v1/tx` | submit a Transaction id (already uploaded): ref updates and lock ops; returns `Applied`, `AlreadyApplied` or a typed rejection |
| `POST /v1/objects/prepare` | optional: returns short-lived create-only upload credentials scoped to `objects/` for the caller's zones |
| `GET /v1/locks?zone=&path=` | convenience listing (advisory view of the manifest) |
| `GET /v1/whoami` | principal and roles, for CLI diagnostics |
| `GET /v1/audit?since=` | audit records, `audit-reader` role |
| `GET /healthz` | liveness, no auth |

The transaction body is the existing canonical-codec object; the server decodes it, it does not invent a second format.

### 3.3 Authn
* Phase 1: long-lived bearer tokens. The server config stores only a hash of each token plus its principal; tokens are
  revocable by config reload. (WalGit's static-token mode is the precedent; the hashing is our addition.)
* Phase 2: OIDC sign-in exchanging for short-lived server-signed tokens, so revocation and expiry do not need a config edit.
* The authenticated **principal**, never `Transaction.author` (self-asserted), is what locks.md calls `holder`. The server
  records both in `tx/`.
* No `none` mode in release builds; a loopback-only dev flag at most.

### 3.4 Authz
Roles attached to principals (or groups, as in Gerrit): `reader`, `writer`, `lock-admin`, `zone-admin`, `audit-reader`. Policy is a
file (versioned, hash recorded in each audit record) mapping `(zone, path glob)` to the roles allowed to publish, acquire and
break. `require-lock` globs (locks.md) live in the same policy. Default deny. The policy file's own edits require
`zone-admin` and are an audited operation. Policy storage location (manifest vs signed file in bucket): open, see issue list.

### 3.5 Audit
Every decision, accepted or rejected, produces an audit record `audit/<time-sortable id>` written create-only
(`If-None-Match: *`): principal, token id (not secret), operation, zone, paths summary, txid, outcome, validator time, policy hash.
Accepted transactions are additionally in `tx/<txid>` (including break tombstones per locks.md). Tamper evidence (hash chain or
object lock) is future work; as written, a holder of bucket write credentials could alter audit objects, see threats T6.

### 3.6 Rate limits
Per principal and per operation class, enforced in the server (token bucket, in memory per replica). Because replicas do not share
state, the effective global limit is per-replica limit times replica count; stated plainly, not hidden. Lock `Renew` and `Acquire`
get their own class because they contend on the manifest key (locks.md notes contention is unmeasured). **No default numbers are
proposed**; they come from the contention measurement task.

### 3.7 Multi-replica coordination
Replicas are identical and stateless. Mutual exclusion is the manifest CAS: a loser reloads the manifest, re-validates and retries
(same loop as the client step machine). No leader. A backend without conditional PUT cannot host the server (unchanged rule).
Idempotency: `tx/<txid>` plus the `applied` set make a retried `POST /v1/tx` safe against any replica.

### 3.8 Clock handling
Validator time is the only time that decides lease expiry (locks.md L4). Each replica uses `max(now, manifest.time_hw)` and writes
the result back as the new `time_hw` in the same CAS, so time never goes backward across replicas. Replica skew up to ε is an
**assumption**; ε is a deployment setting and operators are told to run NTP-class time sync (no value proposed). If skew exceeds
ε, locks.md already states mutual exclusion can fail. A forward jump expires leases early (liveness loss only).
S3's own timestamps are not used for decisions (not verified as monotonic across regions: **[unverified]**).

### 3.9 Deployment
1. **Single host.** One `vcs-server` process on a box with the bucket (or the `Fs` backend for a local/NAS setup). ε = 0. Simplest;
   availability is the host.
2. **S3 plus N replicas.** N identical containers behind a load balancer, TLS terminated at the balancer or the process. The IAM
   role of the server is the only principal allowed to PUT the manifest key (bucket policy; clients get create-only on
   `objects/` and nothing on the manifest). A bucket policy requiring `s3:if-match` on the manifest key stops non-conditional
   writes even from the server's own bugs (from the AWS source above).
No database in either; the server reads its config (tokens hashes, policy) at start and on reload.

## 4. Threat model

Assets: manifest integrity (branch heads, locks), object integrity, confidentiality of zones, audit trail, availability of publish.
Trust boundaries: client to server; server to bucket; clients to bucket (objects/ only); operator to config.

| # | Threat | Actor | Mitigation in this design | Residual |
|---|---|---|---|---|
| T1 | Direct manifest write bypasses locks | client with bucket creds | bucket policy: manifest writable only by server role; `s3:if-match` Deny+Null; clients get create-only on `objects/` | misconfigured bucket voids enforcement (locks.md deployment rule); needs a deployment self-check |
| T2 | Stolen bearer token | attacker | hashed at rest, revocable, short-lived in phase 2, token id in audit | lifetime of a stolen long-lived token in phase 1 |
| T3 | Forged author / identity confusion | any writer | holder = authenticated principal, not `Transaction.author` | none known |
| T4 | Lock denial of service: hold every file, or spam acquire | insider/outsider | roles, per-principal limiter, lease cap per policy, `lock-admin` break | needs measured limits |
| T5 | Manifest contention DoS (renew/acquire storm) | writer | rate-limit class for lock ops; reject early before manifest load | one shared manifest key is a bottleneck by design (locks.md) |
| T6 | Audit tampering | holder of bucket write creds, malicious operator | create-only writes, server-only credentials | no tamper evidence yet; hash chain/object lock is follow-up |
| T7 | Replay of a captured `POST /v1/tx` | network attacker | TLS; `applied` makes replays idempotent | none beyond TLS trust |
| T8 | Missing or tampered object referenced by a tx | client | closure check at validation; objects are content-addressed and fetch already rejects tampered ones | cost of closure check unmeasured |
| T9 | Path tricks evading lock/permission globs (case folding, unicode, `..`, symlinks, rename) | writer | use the core's validated path type; both sides of a rename are checked (locks.md); case-insensitive filesystems are issue #89 | glob semantics on non-normalized names need a test corpus |
| T10 | Clock manipulation to expire others' leases | operator/compromised replica | `time_hw` never decreases; clients cannot supply time | a compromised replica can still jump forward (liveness loss) |
| T11 | Compromised server | attacker | minimal surface, no object bytes, no state, least-privilege IAM, audit shipped to a second sink | a compromised server can write any manifest: it is the root of trust |
| T12 | Policy file tampering / downgrade | insider | edits need `zone-admin`, audited, policy hash in each record | policy storage choice open |
| T13 | Information leak through error messages or `GET /v1/locks` | reader | authorize reads by zone; errors name only what the caller may read | enforcement of read ACLs when clients read the bucket directly: either give no direct read or accept bucket-level read |
| T14 | Replica skew greater than ε | operations | documented assumption, startup check against `time_hw` | by assumption (locks.md) |
| T15 | Token or secret in logs | operator | structured logging with a redaction test | none known |

Out of scope: malicious bucket provider, client-side exfiltration, denial of service at the network layer.

## 5. Open questions
* Policy storage: signed file in bucket vs section of the manifest (the latter adds contention and a format bump).
* Do clients read objects with their own read credentials, or via presigned URLs from the server? Affects T13.
* Per-replica vs shared rate limits; needs the contention measurement first.
* Tamper-evident audit.
* Authz for reads in sparse-fetch (per-zone read ACL).

## 6. Follow-up tasks
Filed as issues #121 (skeleton), #122 (authn and roles), #123 (permission policy), #125 (lock validator, blocked by #67), #127 (audit), #128 (rate limits), #129 (S3 deployment and self-check), #130 (model extension for replicas and epsilon). Order: server skeleton, authn, policy, validator (blocked by manifest
format v2 in the locks tasks), audit, limits, deployment/self-check, model extension for ε and replicas.
