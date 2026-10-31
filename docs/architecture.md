# Architecture

Status legend: **[impl]** implemented and tested · **[design]** specified, not built.

## Crates

```
vcs-objects  ids, canonical codec, object types, name/path validation   (no I/O, no async)      [impl]
vcs-store    ObjectStore trait; MemoryStore, LocalStore (loose objects) (sync I/O)              [impl]
vcs-repo     zones, refs, index, workspace, status/diff/commit/log, snapshots, fsck             [impl]
vcs-cli      `vcs` binary (clap)                                                                [impl]
vcs-store-s3, vcs-transaction (remote), vcs-merge, vcs-operations, …                            [design]
```
Crates are only created when a real boundary exists. The core never depends on Tokio or any S3 SDK.

## Local repository layout [impl]

```
.vcs/
  format            "all-git repository format 1" (written last by init ⇒ crashed init is not a repo)
  objects/aa/bbbb…  loose immutable objects, written tmp+fsync+rename; tmp/ for in-flight writes
  refs              all branch heads + named snapshots in ONE file, replaced atomically
  zones             zone name → owned path prefix (root = "")
  workspace         subscribed zones and the branch checked out in each
  index             staged tree: path → (blob id, mode, size, mtime_ns) + write time
  lock              O_EXCL repository lock (stale lock ⇒ explicit error telling user to delete it)
```
All state files are framed `magic || version || body`, produced with the same canonical codec, and rejected if malformed.

### Why `refs` is a single file
Multi-zone atomicity locally is then trivial: build the new refs in memory, compare-and-set every `old` value
(`Refs::apply`, all-or-nothing, tested), write one temp file, fsync, `rename`. The rename is the **linearization point**:
a reader sees all zone heads advanced or none. This is deliberately the same shape as the remote protocol
(one manifest object, CAS), so the local code path is a test bed for it. Limit: O(#branches) rewrite per commit – fine
for thousands of branches; the remote design shards by zone (below).

### Commit flow [impl]
1. take repo lock; load zones/workspace/refs/index
2. per subscribed zone: build zone-relative tree from index; skip if unchanged vs parent root
3. write Commit objects, then the Transaction object (immutable data **before** any pointer moves)
4. `refs.apply` (CAS on `old`), atomic rename of `refs` ← commit point
5. crash before 4: orphan objects only (harmless, collectable). crash after 4: committed. No state in between.

### Index, status and the stat cache [impl]
Status compares (head tree ↔ index) and (index ↔ worktree). The worktree comparison trusts `(size, mtime, mode)` only if
`mtime + 2s ≤ index write time`; otherwise it re-hashes ("racy-git" rule). Test: same-size edit with restored mtime is
detected. Missed filesystem events are therefore not a correctness issue today because there is no watcher; when
a watcher is added it must be treated as a hint with periodic full verification.

### Sparse workspace [impl]
A workspace *subscribes* to zones. Unsubscribed zones are not materialized, are excluded from status/add/commit, and are
reported as `not subscribed (… NOT deleted)` – never as deletions. `zone remove` refuses on a dirty zone; `zone add`
refuses to overwrite untracked files. Today the object store is still local-complete; the remote milestone makes
unsubscribed zones genuinely not downloaded because fetch is per-zone.

### Safety rules in checkout/materialization [impl]
Preflight validates every path before touching disk: path syntax, zone ownership, no overwrite of untracked files, no
write through symlinked or file ancestors (tested with a symlink pointing outside the worktree). `.vcs` is rejected
case-insensitively in any component. Dirty zones refuse to switch.

## Remote / S3 design [design – Milestone 3]

**Required backend capabilities:** read-after-write for new keys, `GET` range reads, and **one** conditional write
primitive: either `If-None-Match: *` PUT (create-only) or `If-Match: <etag>` PUT. MinIO and AWS S3 support these;
providers without them cannot be authoritative backends (they can still be read-only mirrors). The client must
feature-detect at `init` and refuse to proceed otherwise; it must not assume uniform semantics.

**Layout**
```
objects/<hh>/<id>            immutable loose objects (small repos / hot objects)
packs/<pack-id>.pack|.idx    immutable packs, zone-tagged; idx enables range reads
manifest/<zone>              per-zone manifest: {branch → commit, pack list, version}; updated by conditional PUT
txlog/<ulid-or-txid>         immutable Transaction records
global/manifest              tiny pointer object listing manifest versions: the multi-zone CAS point
```
**Single-zone publish:** upload objects (idempotent, content-addressed) → write `txlog/<txid>` → conditional PUT of
`manifest/<zone>` (If-Match). Linearization point = successful conditional PUT.

**Multi-zone atomic publish (chosen):** zone manifests are *not* the commit point for cross-zone transactions. Instead
`global/manifest` holds `{zone → manifest version}` and is advanced by one conditional PUT. Alternatives rejected:
2PC across several keys (needs a coordinator and recovery of in-doubt participants; S3 has no multi-key transactions);
a repo-wide single head for *all* publishes (serializes unrelated zone writers; mitigated by giving single-zone
publishes their own CAS target and having `global/manifest` reference *immutable* per-zone manifest versions, so the
global CAS only runs for cross-zone transactions and for snapshots). Open point to settle in M3: whether single-zone
publishes also bump `global/manifest` (simpler reads, more CAS contention) – decide with a benchmark of conflict rates.

**Failure analysis (each is a test requirement in M3; the state-machine model is written before the code)**

| Event | Outcome |
|---|---|
| client crash before CAS | orphan objects/txlog entry; no visible effect; GC after grace period |
| client crash after CAS, before ack | committed; retry with the same `txid` finds it in txlog/manifest ⇒ idempotent success |
| upload timeout | retry (content-addressed ⇒ safe); conditional PUT only after all uploads confirmed |
| response lost after successful CAS | retry sees `txid` already applied ⇒ report success, no double-apply |
| CAS fails (412) | reload manifest; if the target branch is unchanged ⇒ retry; if moved ⇒ surface "remote advanced from X to Y" (sync/rebase/merge) – never overwrite |
| two writers, different zones | disjoint zone manifests ⇒ both succeed (cross-zone txs conflict only on shared zones) |
| two writers, same branch | exactly one CAS wins; loser gets the advance error |
| tx references missing object | publisher verifies existence (HEAD/pack idx) before CAS; readers treat a missing object as corruption, not absence |
| partial multipart/pack upload | object invisible until complete; packs referenced only after idx upload |
| store unavailable | operation fails with retryable error; no local state is marked published |
| reader sees old manifest | consistent older snapshot (all-or-nothing); it fetches the new one on next sync |
| GC races publish | GC only deletes objects unreachable from manifest history **and** older than the max in-flight lease (e.g. 24h); publishers refresh the lease by listing their txid in `txlog` first |

**Offline:** local commits are normal local state; `publish` runs the protocol above. Server-enforced policy
(permissions, locks) is evaluated by whichever component performs the conditional write (a stateless validating service,
or object-store policy + signed manifests); this is *not* designed in detail yet and is not claimed to exist.

## Garbage collection [design]
Mark from: all branch heads in retained manifests, named snapshots, protected operation history, tags, in-flight
transactions (leases), pinned objects. Sweep only objects older than the grace window and absent from the mark set.
No destructive GC will be implemented until the reachability tests (including a concurrent-publish model test) exist.
Local `fsck` (read-only) exists today.

## Merge / operations / semantic layers [design]
Specified in the roadmap. Hooks already present: `Commit.change_id` (fresh per new commit today; rewrite operations
will carry it forward), `Transaction` as the unit that an operation-log entry will reference.

## Change identity [partially impl]
A new logical change gets a fresh 128-bit `ChangeId` at commit. Rules adopted for the rewrite milestone: amend and rebase
keep the id; squash keeps the id of the top change; split gives the original id to the first part and fresh ids to the rest;
cherry-pick creates a new id (records the source in metadata); two live commits with the same id are shown as *divergent*
(visible state, never silently resolved). Only the creation rule is implemented.

## Decisions log
| # | Problem | Choice | Why / cost |
|---|---|---|---|
| D1 | Hash | BLAKE3, domain-separated, version in preimage | speed + clean format evolution; unbenchmarked locally |
| D2 | Codec | hand-written canonical varint codec | testable canonicality; more code than serde |
| D3 | Cross-zone model | per-zone commits + Transaction + Snapshot | independent histories + atomicity, see object-model.md |
| D4 | Overlapping zones | forbidden (enforced) | unambiguous ownership; nested/overlapping is future work |
| D5 | Store trait sync | sync now, async twin at the S3 boundary | keep core free of Tokio |
| D6 | Refs storage | single atomically-replaced file | trivial local multi-ref atomicity; O(n) rewrite |
| D7 | Symlinks | unsupported in v1 | removes the main path-escape vector until a safe design exists |
| D8 | Diff | prefix/suffix trim + LCS (size-capped) | simple and correct; Myers/patience/histogram benchmarked in M4 |
| D9 | Tree flatten | whole-tree per zone | O(n); subtree-skipping + lazy loading in M5 after measurement |

## Known limitations (honest list)
No merge/rebase/undo/operation log yet; no remote/S3; no packfiles or chunking; no lock/lease; no Git/P4 interop; no
filesystem monitor; no authn/authz; `snapshot checkout` and branch deletion are not implemented; `vcs log` has no
merge-commit support because merges don't exist yet; no unit of concurrency control beyond a coarse repo lock.
