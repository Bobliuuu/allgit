# Architecture

Status legend: **[impl]** implemented and tested · **[design]** specified, not built.

## Crates

```
vcs-objects  ids, canonical codec, object types, name/path validation   (no I/O, no async)      [impl]
vcs-store    ObjectStore trait; MemoryStore, LocalStore (loose objects) (sync I/O)              [impl]
vcs-repo     zones, refs, index, workspace, status/diff/commit/log, snapshots, fsck             [impl]
vcs-cli      `vcs` binary (clap)                                                                [impl]
vcs-remote   Backend trait, Mem/Fs backends, manifest, publish step machine, sparse fetch, model tests [impl]
vcs-store-s3, vcs-merge, vcs-operations, …                                                      [design]
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

## Remote design [impl: protocol + Fs/Mem backends; S3 backend tracked in issues]

**Required backend capabilities** (`vcs-remote::Backend`): read-after-write for new keys, range GET, create-only PUT
(S3 `If-None-Match: *`), and compare-and-swap PUT on an opaque version tag (S3 `If-Match`). Providers lacking either
conditional primitive cannot be authoritative. A shared conformance suite (`backend::conformance`) runs against every backend.

**Layout**
```
objects/<hh>/<id>   immutable objects (create-only)
tx/<txid>           immutable Transaction records (create-only)
manifest            THE authoritative state: generation, branch heads, snapshots, applied txids, zone definitions
```
**Design decision (changed from the first draft).** One manifest object, not per-zone manifests. Rationale: a single CAS
gives multi-zone atomicity with no coordinator and no two-phase commit. Cost: all writers contend on one key. Mitigation
implemented: on CAS loss the writer reloads and re-validates only its own `old` heads; writers on disjoint branches
therefore succeed without user-visible conflict (model-checked). Sharding the manifest by zone is deferred until a benchmark
shows CAS contention matters.

**Publish step machine** (`Publisher::step`; every step idempotent):
`Collect → Upload* → PutTx → Load → Cas`, back to `Load` on CAS loss or any CAS error.
* Collect walks the local closure of the new heads, pruning at objects the remote has. **Upload is dependency-first
  (post-order)**, so "object exists remotely ⇒ its whole closure exists" holds even after a crash mid-upload.
* Load: if the txid is already in `manifest.applied` ⇒ `AlreadyApplied` (lost-ack and duplicate-submit safety); if any
  update's `old` differs from the current head ⇒ `Diverged` (never overwrites).
* Cas: create-only PUT on an empty repo, otherwise `If-Match`. **The successful CAS is the linearization point.**
* `Unavailable` retries a step up to 8 times, then `Unknown` (outcome undetermined; re-running the same tx converges).

**Executable model** (`crates/vcs-remote/tests/model.rs`): explores every interleaving of 2 writers' steps with either one
crash at any point or one lost request / lost response at any CAS, then recovers by retrying, for four scenarios
(same-branch race, disjoint zones, multi-zone vs single-zone, duplicate submission) – about 1.7M schedules. Invariants:
every manifest version equals the previous plus exactly one whole transaction (atomicity, no lost update); no tx applied
twice; every stored object's dependencies are stored; every reported success is durably visible. Mutation-checked: removing
the idempotency check or reversing upload order makes the model fail.

**Failure table**

| Event | Outcome (covered by) |
|---|---|
| client crash before CAS | orphan objects only; retry converges (model) |
| crash after CAS, before ack | committed; retry returns `AlreadyApplied` (model) |
| upload timeout | step retried; content-addressed so safe |
| response lost after CAS | `Load` finds txid applied (model, `lost_ack_then_retry_is_idempotent`) |
| CAS fails | reload; disjoint writers reconcile, same-branch loser gets `Diverged` (model) |
| two writers, same branch | exactly one wins (model) |
| tx references missing object | impossible by upload order; fetch rejects missing/tampered objects (`tampered_remote_object_is_rejected`) |
| partial upload | object invisible until complete; closure invariant (model) |
| store unavailable | `Unknown`, no local state marked published (`unavailable_backend_yields_unknown_not_success`) |
| reader sees old manifest | consistent older state |
| GC races publish | **no GC exists yet**; design in §GC |

**Sparse fetch**: `fetch_heads(zones)` reads the manifest then only objects reachable from the requested zones' heads;
tests assert an unsubscribed 500 KB zone is never read. Every fetched object is hash-verified and stored dependencies-first.

**Local side** (`vcs-repo::sync`): remote-tracking refs in `.vcs/remote-refs`; `publish` (one atomic tx for all subscribed
branches with local changes; refuses non-fast-forward with guidance; unchanged branches never block others), `fetch`,
`sync` (fast-forward only; dirty zones and divergence are reported, never touched), `clone_from` (sparse by zone).
Zone definitions travel in the manifest, merged atomically with the publishing tx, with name/path/overlap conflicts rejected.

**Not done:** S3 backend, server-side policy/locks (needs a validating writer), packs/range-read optimisation, GC,
manifest `applied` window > 4096 (older retries are rejected as Diverged, never double-applied).

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
