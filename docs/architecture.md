# Architecture

Status legend: **[impl]** implemented and tested · **[design]** specified, not built.

## Crates

```
vcs-objects   ids, canonical codec, object types, name/path validation   (no I/O, no async)      [impl]
vcs-store     ObjectStore trait; MemoryStore, LocalStore (loose objects), packs (sync I/O)       [impl]
vcs-repo      zones, refs, index, workspace, status/diff/commit/log, snapshots, fsck, op log, locks,
              stash, rebase, GC, git/p4 import and export, publish/fetch/sync/clone              [impl]
vcs-remote    Backend trait, Mem/Fs backends, manifest, publish step machine, remote packs,
              sparse and lazy fetch, model tests                                                 [impl]
vcs-store-s3  S3-compatible Backend (selectable with s3:// remotes; https behind feature `tls`) [impl]
vcs-merge     merge-base, diff3, tree merge, rename-aware merge, optional structural merge       [impl]
vcs-server    `vcs-server` binary: HTTP validating writer + HttpBackend (auth, audit, TLS)       [impl]
vcs-assist-claude  helper binary for `merge --assist` (Anthropic API, never run live)            [impl]
vcs-cli       `vcs` and `git-remote-allgit` binaries (clap)                                     [impl]
vcs-bench     dataset generator, criterion benchmarks, s3bench, storebench                       [impl]
```
Outside the cargo workspace (research only, not built by CI): `vcs-storage-lab`, `vcs-storage-lab-packing`,
`vcs-compress-lab`. `fuzz/` is its own cargo-fuzz project (decoders, pack, import, S3 list parsing).
Crates are only created when a real boundary exists. The core never depends on Tokio or any S3 SDK.

## Lazy blob fetch [impl]

`fetch` and `clone` store commits and trees eagerly and leave file blobs (and chunks) on the remote. A blob is pulled the
first time something reads it: `Repo::prefetch_zone_files` collects the missing blob ids reachable from the head tree
(restricted to the sparse set, if any), downloads them in one batch (`fetch_many`; one ranged read per remote pack, not
one request per object) and hash-verifies every object before it is stored. Order of effects for `sync`, `zone add` and
clone checkout: (1) fetch commits and trees, (2) prefetch the head's blobs, (3) only then rewrite the worktree and index,
(4) move the ref last. A backend that fails or serves a tampered blob during step 2 therefore leaves worktree, index and
ref untouched (`backend_failing_mid_fetch_leaves_no_partial_worktree_write_on_sync`,
`tampered_head_blob_fails_the_batch_prefetch_and_the_clone` in `crates/vcs-repo/tests/lazyfetch.rs`); blobs already
verified stay in the store and the rerun fetches only the rest. `vcs prefetch [--zone Z]` runs step 2 alone. `fsck`
reports a blob that is merely not cached locally as "refetched on demand", not as a problem. History blobs (not in any
subscribed head) are fetched on first use, one object at a time.

## Local repository layout [impl]

```
.vcs/
  format            "all-git repository format 1" (written last by init ⇒ crashed init is not a repo)
  objects/aa/bbbb…  loose immutable objects, written to tmp/, then flushed and renamed in batches at the durability barrier (see Durability)
  refs              all branch heads + named snapshots in ONE file, replaced atomically
  op_head           id of the current Operation (operation log, see operations.md)
  zones             zone name → owned path prefix (root = "")
  workspace         subscribed zones and the branch checked out in each
  index             staged tree: path → (blob id, mode, size, mtime_ns) + write time
  commit-graph      optional cache: per commit parents, time, generation number (pure cache, see below)
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
4. `ObjectStore::sync()` durability barrier (see below): all of the above objects are durable
5. `refs.apply` (CAS on `old`), atomic rename of `refs` ← commit point
6. crash before 5: orphan objects only (harmless, collectable). crash after 5: committed. No state in between.

### Durability [impl]
**Guarantee:** an object that is visible under its final name in the local store is complete and durable, and no
ref, index or manifest is written that names an object before that object is durable.

Mechanism (`LocalStore`, batched instead of one fsync per object, #21):
- `ObjectStore::put` writes the object to `objects/tmp/` **without fsync** and records it as pending. If the object
  already exists (published or pending) nothing is written. Pending objects are readable through the same store
  instance, so a flow can write a blob/tree and read it back before the barrier.
- `ObjectStore::sync()` is the **durability barrier**. It (1) flushes every pending file's data, (2) renames each
  file to `objects/aa/rest`, (3) flushes the touched directories. On macOS, where `File::sync_all` is the very slow
  `F_FULLFSYNC`, steps 1 and 3 use plain `fsync` per file/directory plus **one** hardware flush per step (the same idea
  as git's `core.fsync=batch`); elsewhere `fsync` already is the flush. No-op when nothing is pending.
- Callers place the barrier immediately **before** the pointer write: `Repo::add` before the index is saved,
  `Repo::commit` and `snapshot_create` before `refs`, and the fetch path before any ref moves to fetched objects.
  The pointer files themselves (`index`, `refs`, ...) still use write-temp, fsync, rename.
- Objects only get a final name inside the barrier, after their data is durable, so a crash can never leave a
  truncated or empty file under an object name (which "skip if exists" would otherwise trust forever).

Crash outcomes: crash before the barrier loses only the not-yet-referenced objects (stray files remain in
`objects/tmp/`, never listed or read, harmless); crash after the barrier but before the pointer write leaves
unreferenced but complete objects (collectable); crash after the pointer write is fully committed. `vcs fsck`
additionally verifies that every index entry's blob exists. Tested in `crates/vcs-repo/tests/durability.rs`
and `crates/vcs-store` unit tests.

Limits: the barrier assumes the OS/drive honour `fsync` (and `F_FULLFSYNC` on macOS); a drive that lies defeats any
such scheme. The durability numbers were measured on APFS only; Linux/ext4 behaviour of the batching is not measured.

### Commit-graph cache [impl]
`.vcs/commit-graph` stores, for every commit reachable from a branch head, its parents, author time and generation
number (1 + max over parents). It is a **pure cache**: every byte can be recomputed from commit objects, so it is
never authoritative. It is written lazily (by `merge_bases` and by `is_ancestor` once its short walk is exhausted),
extended incrementally when branch heads moved past it, and ignored and rebuilt if it fails any check on load
(blake3 checksum, sorted unique ids, parent indexes in range, every generation equal to 1 + max of parents). If some
history is missing or malformed the graph is simply not used. `vcs fsck` compares every entry with the real commit
and reports a corrupt file or a mismatch. Used by merge/rebase merge-base, `Repo::is_ancestor`, `merge-base`, `describe`, `bisect`, `~N` revisions, a long `log`
(commit order comes from the graph and the commits are then read on all cores) and the path walks below.
`Repo::set_commit_graph(false)` disables it and the changed-path filters. Tested: random branch/merge histories give
identical `log`, `merge_bases` and `is_ancestor` with and without it (`crates/vcs-repo/tests/commit_graph.rs`,
`vcs-merge` unit proptests). One file per zone (`.vcs/commit-graphs/<hash of zone id>`).

### History walks: subtree skipping, changed-path filters [impl]
Path-limited `log`, `--follow`, `-S`, `blame`, `show`/`log --name-status` no longer flatten the whole tree of every
commit (cost O(commits x files)); they compare the entries at the asked paths (`treeops::entry_at`) or diff two trees
without entering directories whose id is equal (`treeops::diff_trees`), so the cost follows the depth of the path and the
size of the change. `Repo::log_paths` stops at the `-n`-th match instead of reading all history.
`.vcs/path-filters/<hash of zone id>` holds a Bloom filter per (commit, parent) of the file paths that differ from
that parent plus every directory above them (12 bits and 7 hashes per path, none for root commits or diffs of more
than 512 paths). A walk that has examined 256 commits builds the filters of the commits it passes and stores them;
later walks pass a commit whose filters rule the paths out without reading it (`log`, `blame`, `--follow`). A filter hit
is only a "maybe": the real entries are compared, so a false positive costs time, never a wrong answer. Like the
commit-graph it is a pure cache: blake3-checked, discarded when anything is wrong, and `vcs fsck` recomputes every
stored filter and reports a mismatch. `-S` tests commits on all cores and counts a blob once. Tests:
`crates/vcs-repo/tests/history_walks.rs` (every result equals the whole-tree computation, with and without filters,
corrupt and lying filter files), `crates/vcs-cli/tests/history_cmds.rs` (CLI output identical cold and warm).
Measurements and the generator are in `docs/history-walks.md`.

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

## Remote design [impl: protocol, Fs/Mem/S3/HTTP backends, remote packs]

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
therefore succeed without user-visible conflict (model-checked). Sharding the manifest by zone is deferred: the benchmark (docs/manifest-scaling.md) shows
CAS contention at 8 or more publishers and a prototype (feature `sharded`), but not yet an integrated design.

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
| GC races publish | **no destructive GC exists**; design in [gc.md](gc.md) |

**Sparse fetch**: `fetch_heads(zones)` reads the manifest then only objects reachable from the requested zones' heads;
tests assert an unsubscribed 500 KB zone is never read. Every fetched object is hash-verified and stored dependencies-first.

**Local side** (`vcs-repo::sync`): remote-tracking refs in `.vcs/remote-refs`; `publish` (one atomic tx for all subscribed
branches with local changes; refuses non-fast-forward with guidance; unchanged branches never block others), `fetch`,
`sync` (fast-forward only; dirty zones and divergence are reported, never touched), `clone_from` (sparse by zone).
Zone definitions travel in the manifest, merged atomically with the publishing tx, with name/path/overlap conflicts rejected.

**Not done:** S3 and HTTP remote GC and remote pack compaction (#232, #248), S3 connection reuse (#243), a cloud TLS interop test, manifest `applied` window > 4096 (older retries are rejected as Diverged, never double-applied). Locks are enforced when publishing through `vcs-server` ([server.md](server.md)); against a bare directory or bucket they are validated by the client only ([locks.md](locks.md)). Remote objects are published as one pack per transaction ([remote-packs.md](remote-packs.md)).

## Garbage collection [impl: local and directory remotes]
Mark from: all branch heads, named snapshots, tags, protected operation history, in-flight transactions (leases).
Sweep only objects older than the grace window and absent from the mark set; `put` refreshes the age of a deduplicated
object so a re-adopted orphan is not swept. `vcs gc` deletes locally; `vcs gc --remote --yes` does the same for directory
remotes; `--dry-run` only reports. S3 and HTTP remotes are refused (#232). Models: sharded commit and GC race in
`crates/vcs-remote/tests`. Full design: [gc.md](gc.md).

## Merge / operations / semantic layers [impl]
See [merge.md](merge.md), [rebase.md](rebase.md), [operations.md](operations.md), [assist.md](assist.md). Structural merge (Rust, Python, TypeScript, Go) is behind cargo features and off by default; rename-aware merge is opt-in; assisted merge has never been called live. Hooks used: `Commit.change_id` (fresh per new commit today; rewrite operations
will carry it forward), `Transaction` as the unit that an operation-log entry will reference.

## Change identity [impl for create, rebase; others partial]
A new logical change gets a fresh 128-bit `ChangeId` at commit. Rules adopted for the rewrite milestone: amend and rebase
keep the id; squash keeps the id of the top change; split gives the original id to the first part and fresh ids to the rest;
cherry-pick creates a new id (records the source in metadata); two live commits with the same id are shown as *divergent*
(visible state, never silently resolved). Creation and rebase (keeps ids) are implemented; cherry-pick and revert make new commits.

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
- Merge: criss-cross merges and file/directory conflicts are errors; rename detection is opt-in. Rebase handles linear ranges only.
- Operation log covers local refs/workspace only: snapshots, remote-tracking refs and unrecorded crash-window changes are not undoable, and `undo` across several zones is not atomic ([operations.md](operations.md)).
- Packs: `repack` only, no delta or entry compression, loose objects cost 4 KiB each on ext4 (no automatic repack yet). Chunk manifests cover big files.
- GC: S3 and HTTP remotes are not supported; remote packs are never rewritten or compacted, so pack count grows with publishes.
- Locks: enforced only through `vcs-server`; `unlock --break` is unrestricted; no clock-skew margin without `--skew-epsilon-ms` ([locks.md](locks.md)).
- Server: plain HTTP unless built with `--features tls`; TLS never tested against a real cloud endpoint or public CA.
- Git: import and an incremental export; the remote helper is new. Perforce: fixture-tested import only.
- Windows is compile-checked in CI, never run. Symlink handling on Windows is untested.
- S3 numbers come from one run from one residential connection; no in-region measurement.
- No filesystem monitor; a coarse repo lock is the only local concurrency control; `log` shows first-parent history per zone branch; `status` on 100k-file flat repos is slower than git.
