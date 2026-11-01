# Garbage collection

Status: **local sweep and remote sweep implemented.** `vcs gc` (`Repo::gc_sweep`) deletes for real under the repo lock;
`vcs gc --remote --yes` runs protocol G1 against a directory remote; `--dry-run` reports for both. Details in "Implemented"
below. Tracks issues #33, #43 and #50. The destructive paths are labelled DESTRUCTIVE in their docs and in the CLI output.

## Safety argument

An object is deleted only if (1) it was **not reachable** from any root in a mark taken after the object list was
captured, **and** (2) its last write or touch is **older than the grace period**, which must exceed the longest time
a writer can hold an object it has written but not yet referenced. Writers always store objects before the pointer
(ref, `op_head`, manifest) that names them, and a reader or later writer only learns an id by following a pointer,
so a pointer that is installed after the mark names only objects that were written (or re-touched) after the mark began;
those are young and survive the grace check. The one hole is a dedup hit: `put` of an id that already exists writes
nothing, so an old orphan that a new commit re-adopts stays old. **A destructive sweep therefore requires `put` to
refresh the object's timestamp on a dedup hit (or the remote equivalent), and a lease for any writer that can outlive
the grace period.** With those two, deleting never removes an object a live writer is about to reference.
If the grace period is wrong (a writer stalls longer than it), data can be lost; this is why the sweep stays unimplemented.

## Implemented

**Local sweep** (`vcs gc`, `Repo::gc_sweep_at`). Holds the repo lock. Refuses a grace period at or below the store's
touch-on-dedup threshold (3.5 days; `GcOptions::allow_short_grace` overrides, unsafe). Order: clean crash leftovers (files in
`objects/tmp` and `.pack` files without a `.idx`, both only if older than the grace, because a fresh index-less pack can be a
write in progress); plan twice (object list, then roots, then ages, each time) and delete only the intersection; rewrite every
pack that holds a doomed object (new pack and index first, then the old index, then the old pack, so a crash leaves duplicates
or an index-less pack, never a missing live object); finally unlink doomed loose files. Pending (unsynced) objects are never
removed. A reader process that opened before the rewrite reloads its pack list when a pack file has vanished. Tests:
`crates/vcs-repo/tests/gc.rs` (including a crash at each of the three sweep points), the store unit tests, and the `gc` step of
`crates/vcs-repo/tests/model.rs`, which runs the sweep (after a repack on every other step, with a fresh orphan) inside the random
operation model and asserts that every model commit, tree, blob and logged operation survives, `fsck` is clean and the state
still matches the model; mutation-checked by removing `op_head` from the roots (the test fails).

Local roots added to the table below: a stopped rebase (original head, cursor, current, todo) and the cherry-pick / revert marker.
In-flight transactions: every stored `Transaction` object is kept, which covers them conservatively.

**Remote sweep** (`vcs gc --remote`, `vcs_remote::gc::remote_gc`). Protocol G1 of
`docs/research/storage/concurrency-and-correctness.md`, on the backend's clock. Needs `--yes` unless `--dry-run`; grace must exceed
twice `--hold-secs` (default 1 h); the backend must support `delete`, `now_ms`, `modified_ms` and `touch`
(`Backend::supports_gc`). Supported: directory remotes (`FsBackend`), `MemBackend::with_gc()` (tests), **S3** and the **HTTP server**
(#232). S3 (`crates/vcs-store-s3/src/gcops.rs`): `touch` is a server-side copy of the object onto itself with
`x-amz-metadata-directive: REPLACE` (needs `s3:GetObject` and `s3:PutObject`; the ETag of a single-PUT object is unchanged; an
object over 5 GiB cannot be copied), `modified_ms` is the `LastModified` of a ListObjectsV2 on the exact key, `now_ms` is the server's
`LastModified` of a touch of `<prefix>gc/clock`, read once and then advanced with the local monotonic clock (re-read every 10 minutes),
so publishers pay one extra request per process, not per reading. Resolution is one second on S3. HTTP (`vcs-server`):
`GET /v1/clock` (any role), `GET /v1/mtime/<key>` (admin), `POST /v1/touch/<key>` (write; never the manifest or shelf keys for non-admins);
the client reports `supports_gc` only if the server answers `/v1/clock` (an older server or a backend that cannot be collected: false, as
before). Tested: `s3_conformance_gc` and `s3_sweeper_deletes_old_garbage_keeps_the_head_and_touch_protects` in
`crates/vcs-store-s3/tests/gc_s3.rs` (gated on `VCS_TEST_S3_ENDPOINT`; run on RustFS 1.0.4 only, not AWS S3 or MinIO), and
`gc_primitives_over_http_follow_roles_and_the_backend` in `crates/vcs-server/tests/server.rs` (HTTP over a directory backend, `conformance_gc`).
The interleaving/model tests (`gc_real.rs`, `model_gc.rs`) run on `MemBackend`; nothing here re-runs them against S3 timing. **Still not
done:** remote pack rewriting (a pack with any live object stays whole) and renewal of the sweeper's lock lease during a very long sweep.
`delete` itself is implemented for all four (S3 `DeleteObject`; HTTP `DELETE /v1/o/<key>`, admin token only, never the manifest).
**S3 has no conditional delete** (no `If-Match` on DELETE), so deletes are unconditional and the tombstone handshake is what makes
them safe.

Deletion units are a loose object (`objects/xx/...`) or a whole pack (`packs/<name>.idx` + `.pack`, index deleted first, so no
reader is pointed at a vanishing pack). A pack is deleted only if none of its objects is needed; a pack holding any live
object is kept whole (`packs_with_garbage_kept` in the report; **no remote pack rewriting**). A `.pack` with no index (a publisher
that died between PutPack and PutIdx) is a unit with no objects and is deleted once old. Roots: branch heads, snapshots, tags, and the
old/new heads of every transaction in the manifest's applied window (the retention window), and the base commits of every unexpired shelf (`shelves/*/*/meta`, docs/shelves.md). Expired shelves are deleted by a non-dry-run sweep. Never deleted: the manifest, `tx/` and the packs of live shelves.

Publisher side (`Publisher`, only against GC-capable backends). What a publish relies on without having just created it is
guarded: a legacy loose object Collect pruned at (`commit_on_remote`), and a pack or index that `put_if_absent` found already there
(a retry of an aborted publish re-creates the identical, content-named pack: the dedup hole, for packs). Guard = touch, then check
no `tomb/<unit>` exists, then check the key still exists; loose objects are guarded BEFORE anything is uploaded, a deduplicated
pack right after PutIdx. The CAS is fenced to `hold_ms` (default 1 h) after Collect. A failed check or the fence restarts at
Collect (the loose layout then also distrusts what this publish already uploaded). The packed Collect prunes only at heads the CAS
re-verifies (`update.old`), which stay roots, so it needs nothing else; a *new* branch prunes at the manifest heads read at Collect,
protected only while their transactions stay in the applied window (4096).
**Deviation from the research text:** G1 puts the handshake after the uploads. For loose objects that lets a restarted publish prune
at an object it uploaded earlier whose child the sweeper has since deleted (a visible parent with a missing child), found by the
exhaustive search in `gc_real.rs`.
Sweeper side: single instance via `gc/lock` (create-only, expiry on the backend clock, takeover by CAS), stale tombstones cleared
under the lock, candidates tombed, remark, delete parents before children (dependency cycles between unreachable packs are broken
arbitrarily), untomb. **Second deviation:** the remark's keep set is closure(roots) plus closure of *every object of a non-candidate
unit* plus touched candidates, and the decision uses exactly the ages that keep set saw, so a stalled sweeper, a clock step or a
touch landing between the two reads cannot delete a child while its parent survives. Both were found by the interleaving search.

Tests (`crates/vcs-remote/tests/gc_real.rs`): the `model_gc.rs` world run against the real code with an exhaustive search over all
interleavings of publisher steps and sweeper phases (four variants: packed publisher adopting a loose orphan, loose-object publisher,
adopting the orphan as the head, and re-publishing an aborted pack; a few hundred thousand schedules), invariant G-REACH after every
phase, and all seven model mutations ported as `GcQuirks` switches that must each produce a violation. Plus fault injection (lost
delete, crashed sweeper's stale tombstones, held and expired lock, damaged remote left untouched, unsafe grace, unsupported
backend) and pack tests (old orphan pack deleted, young one kept, pack with a live object kept whole, index-less pack).

Still not covered: rewriting a pack to drop its garbage (compaction below merges, it does not filter); sweeper that outlives its lock TTL; readers (fetch/clone) slower than the grace period
can lose objects of a replaced head; clock-rate skew (A1 is assumed, not tested); real S3.

## Roots

Local (`Repo::gc_plan`, all covered by tests unless noted):

| Root | Why |
|---|---|
| `refs`: branch heads and named snapshots | current work and pinned states |
| `remote-refs` | what the remote had at last fetch; needed to publish incrementally |
| `op_head` and the whole operation chain | `op undo/restore` must keep working; each view names commits |
| index entries (blob ids) | staged but uncommitted content (`vcs add` then GC must not lose it) |
| merge state (`ours`, `theirs`, conflict base/left/right/placeholder) | a merge in progress |
| every stored `Transaction` | not reachable from commits, and there is no local transaction log yet, so all are kept (conservative) |
| stopped rebase (`rebase` file: orig, cursor, current, todo) and cherry-pick marker | a rebase / pick in progress |

Traversal: Commit -> parents, root tree; Tree -> entries; Snapshot -> commits; Transaction -> old/new heads;
Operation -> parents and view heads. Missing referenced objects are counted (`missing_referenced`), never fatal in a dry run.

Not roots: loose files in `objects/tmp/` (never listed), pending unsynced objects (reported as new).

**Ordering inside the plan.** The object list is captured first, the roots second. Anything created after the list
is not a candidate; anything listed and referenced after the roots were read is young. A destructive local sweep must
also hold the repo lock while it deletes, and re-check each candidate's age and a fresh root read just before removal.

## Grace period and the publisher race

Publish uploads dependencies first and writes the manifest last (`Collect -> Upload* -> PutTx -> Load -> Cas`), so an
object legitimately exists remotely before the manifest names it, for as long as the publish takes, plus any retry
loop (`Unknown` outcomes are re-run later and must still find their uploads). A naive sweep between `Upload` and `Cas`
would delete live data, and the post-order invariant ("exists => closure exists") would break if a child were swept
while its parent survived.

* Default local grace: 14 days (`--grace-secs`). Unknown age (store cannot say) means keep.
* Sweep children only after parents (reverse of upload order), or sweep a whole unreachable closure atomically in
  one batch, so a crash mid-sweep never leaves a parent whose child is gone.
* Local and remote clocks differ: the remote must use its own object timestamps, never the client's.

## What the remote needs (built for directory remotes; see Implemented)

The remote is S3-like: create-only PUT, CAS on `manifest`, no multi-key transactions. Needs:

1. **Object timestamps and touch-on-dedup.** `put_if_absent` hitting an existing key (it returns `false`) must refresh
   `LastModified` before the caller treats the object as stored. S3: server-side copy-to-self with
   `x-amz-metadata-directive: REPLACE` (rewrites `LastModified` without moving data; needs `s3:GetObject`+`PutObject`;
   cost is one request per dedup hit, so uploaders should batch or skip when `LastModified` is already recent), or a
   sidecar `touch/<key>` object. The filesystem backend can use mtime as locally. Without it the argument above fails.
   Not implemented: `Backend` has no touch operation yet.
2. **Roots on the remote:** manifest `branches`, `snapshots`, retained history window of manifests, and the `tx/<txid>`
   records inside that window. Manifest history is not kept today (single key); GC needs either versioned manifests or
   an explicit "retained generations" list. Decision needed before building.
3. **Leases for in-flight publishes.** A publisher registers `lease/<txid>` (create-only, with expiry) before the first
   upload and deletes it after `Cas`. Objects named by an unexpired lease's tx are roots. This replaces guessing a
   grace period for long uploads. Expiry needs a trusted clock (the backend's, not the client's).
4. **A mark phase that is consistent with concurrent CAS.** Mark reads the manifest at generation G; any publish
   landing after G only references objects that are young or lease-protected. The sweep then re-reads the manifest
   and aborts if a deleted candidate is now reachable (cheap double-check, delete-then-verify is not enough since
   delete is irreversible, so verify before each batch).
5. **A GC lock/epoch** (`gc/lock`, create-only with expiry) so two collectors do not run concurrently.
6. **Applied-window interplay:** `manifest.applied` is capped at 4096; a swept `tx/` record must not be one a retry could
   still need.
7. **A model test** extending `crates/vcs-remote/tests/model.rs` with a GC actor, as the architecture requires before
   any destructive GC.

## Plan

1. (done) `vcs gc --dry-run`, local, report only.
2. (done) Touch-on-dedup in `LocalStore::put`: a dedup hit on an object older than `touch_after` (default 3.5 days, a quarter
   of the default grace; `LocalStore::set_touch_after`) refreshes its file mtime, and a failure to touch is an error. A hit
   on a younger object does nothing, so an object's age after any write that names it is at most `touch_after`, and a
   real sweep's grace must exceed `touch_after` plus the writer hold time (`--grace-secs` below 3.5 days is unsafe for a
   sweep; the dry run does not enforce this). An old object that lives in a pack is re-adopted by writing a fresh loose
   copy, because packs are shared and immutable. Tested by `readopted_old_orphan_is_kept_after_dedup_put` and the store
   unit tests; cost in `docs/benchmarks.md`. `MemoryStore` reports no age, so GC keeps its objects anyway.
3. (done) Local `vcs gc` sweep under the repo lock, with crash tests.
4. (done, directory remotes) Remote design items 1-7 as protocol G1, model test first, then implementation. S3 / HTTP touch, age and clock: done (#232). Open: remote pack rewriting, sweeper lease renewal.

## Limits of the dry run

* Reports what would be deleted by the rules above; it does not prove that a real sweep is safe (see the dedup hole).
* Reads the store without the repo lock, so numbers can be stale if another process is writing.
* Object age comes from the file's modification time, which is not preserved by every copy or backup tool.
* Counts, not bytes: sizes are not reported.


## Compaction (issues #248, #249)

`vcs gc --remote --compact [--dry-run | --yes]` merges the remote's many small packs (one per publish) into few, and
`vcs repack` merges small local packs. Code: `vcs_remote::compact` (remote), `LocalStore::compact_packs` (local), policy and
object order in `vcs_store::layout`. Compaction saves no bytes by deduplication alone (content addressing); it cuts the
requests a reader pays (one index GET per pack) and, with v3 blocks, the bytes at rest.

**Policy.** Tiered, not leveled (leveled costs 4x to 15x more PUTs on hash keys, compaction-experiments 1.3). A pack's tier
is its size in powers of 3 over 4 KiB; three packs in one tier merge, and the plan cascades in memory so each final group is
written once. Groups over 512 MiB shed their largest members (a pack is built in memory). Local: ratio 4 over 1 MiB.
`tests/compact_real.rs::a_thousand_publishes_end_with_few_packs_and_bounded_rewrite_cost` runs the real planner after every one
of 1,000 publishes: at most 12 packs at the end, at most 6.5x the published bytes rewritten (the numbers are printed).

**Pack discovery stays LIST.** Packs are found by listing `packs/`, not named by the manifest. A catalog versioned with the
manifest CAS would make every compaction a manifest write that conflicts with publishers, and buys nothing the rules below
do not already give. The decision is model-checked below.

**Rules** (each is a mutation in `CompactQuirks` that the exhaustive search catches):

| Rule | Statement | Mutation that must fail |
|---|---|---|
| C1 union | the output holds every object of every input, checked by reading the uploaded index back before any delete | `drop_object` + `skip_verify` (I-LIVE); with the check on, the run aborts and leaves the inputs |
| C2 output first | new pack, then its index, are durable before the first input is deleted | `delete_before_upload` (I-LIVE) |
| C2b pack before index | an index never exists without its pack | `idx_before_pack` (I-IDX) |
| C3 index first | an input is removed by deleting `.idx`, then `.pack` | `delete_pack_before_idx` (I-IDX) |
| C4 readers retry | a read that finds an index or pack gone lists again and looks again (`packfetch::RACE_ROUNDS`); a miss costs one extra LIST to rule out a changed pack set | reader with one round fails a schedule |
| C5 one at a time | compaction takes `gc/lock` on a backend with a trusted clock, so a sweeper never sees half a compaction | gc during compaction is refused (`Locked`) |

I-IDX: every `.idx` has its `.pack`. I-LIVE: every object of the head's closure is in some index whose pack exists.
`tests/compact_real.rs` runs the compactor on its own thread, advanced one phase at a time, against (a) a reader (`fetch_many`,
`fetch_from_any_pack`, `RemoteSource`) whose k-th request is preceded by `adv[k]` phases for EVERY non-decreasing `adv`, and
(b) the real publisher step machine for every non-decreasing split of its steps over the phases, checking I-IDX, I-LIVE and
G-REACH (the head's closure is complete) at every phase; plus a crash after each of the mutating calls (the invariants hold, and
a rerun converges), a corrupt input (nothing written or deleted), dry-run, and the v1 rollback format.

A publisher needs nothing new: a publish that deduplicated against an input only relies on the object existing, and C1/C2 keep
it existing. Pack **age is laundered**: the output is young, so garbage inside an input waits one more grace period before a
sweep can take it. Compaction does not drop garbage.

**Backends.** Compaction needs `list`, `get`, `put_if_absent`, `delete`. It runs on directory remotes and on S3 (RustFS tested,
below). On a backend without a trusted clock (HTTP servers in front of a non-GC backend, wrappers) there is no `gc/lock`; there is no sweeper there either (remote GC is
refused), and two concurrent compactors are harmless (C1 makes a merge idempotent; a compactor whose input vanished aborts before
deleting anything). `tests/compact_real.rs::two_lockless_compactors_never_lose_an_object_at_any_phase` runs a second compactor to
completion inside the first one's hook at every phase on a backend without a clock and checks I-IDX, I-LIVE and G-REACH. S3 and the HTTP server now have a clock, so compaction there takes `gc/lock` like a directory remote. The HTTP server's `DELETE` needs the
admin token; not tested here.

**Publish hint.** Every 8th remote generation `vcs publish` lists `packs/` (one extra request per 8 publishes) and prints
`hint: the remote holds N packs ...` when N > 24.

### Pack order and v3 blocks (#249)

Compacted packs are laid out commits (parents first), trees (children first), then file content grouped by (zone, directory,
file name), oldest version first. Written as v3 (docs/object-model.md), related versions share a compression window. Measured
by `crates/vcs-cli/tests/compact_measure.rs` (ignored test; machine: the 30-core Linux box, load1 about 19, one run each, all
numbers are bytes or request counts, not times). History: ripgrep (2,303 commits, 13,710 objects, 122 MB raw) and tokio (4,850
commits, 49,093 objects, 238 MB raw), imported with `vcs import-git`, published as 60 evenly spaced prefixes (60 packs):

| | ripgrep | tokio |
|---|---|---|
| remote after 60 publishes (v2 packs + index) | 49.5 MB, 60 packs | 129.2 MB, 60 packs |
| union of distinct objects, per-object zstd-3 (the v2 layout) | 34.4 MB | 77.0 MB |
| after default tiered compaction (v3, 4 MiB blocks, path order) | 5.4 MB, 2 packs | 13.9 MB, 2 packs |
| after compacting everything into one pack | 4.12 MB, 1 pack | 11.15 MB, 1 pack |
| `git` pack of the same history (`objects/pack` of the source clone, for context only) | 6.3 MB | 20 MB |
| full fetch requests (before / tiered / one pack) | 122 / 6 / 4 | 115 / 6 / 4 |
| full fetch bytes down (before / one pack) | 49.5 MB / 4.13 MB | 123.4 MB / 11.15 MB |
| compaction cost (tiered): requests, bytes read, bytes written | 253, 50.2 MB, 5.4 MB | 253, 131.7 MB, 13.9 MB |

Block size and order on the union (same objects, bytes at rest; per-object zstd-3 = 1.00x):

| block size | id order (no layout) ripgrep / tokio | path order ripgrep / tokio |
|---|---|---|
| 64 KiB | 1.17x / 1.30x | 1.45x / 2.05x |
| 1 MiB | 2.08x / 2.02x | 8.26x / 8.40x |
| 4 MiB | 3.01x / 2.56x | 11.17x / 10.44x |

So the blocks win clearly on these two histories (the acceptance bar was 3x), and the order matters: blocks without the path
order get 2.6x to 3.0x, with it 10.4x to 11.2x. Caveat: these are real git histories imported into allgit, dominated by many
near-identical versions of the same files; a repository of unrelated binaries would not behave like this (the research measured
a flat ratio across block sizes for binaries).

**The cost, measured.** Reading ONE object from a v3 pack transfers its whole compressed block. One mid-sized blob by id:
ripgrep 4 requests and 0.60 MB (tiered) / 0.71 MB (single pack) instead of 7 requests and 59 KB with 60 v2 packs; tokio 4
requests and 1.8 MB / 2.3 MB instead of 4 requests and 74 KB. A sparse checkout of one small file therefore moves 10x to 30x
more bytes than before (but in about the same number of requests, and the whole-history fetch moves 12x to 7x fewer).
`--block-kib 1024` trades bytes at rest (8.3x instead of 11.2x on ripgrep) for smaller single-object reads. Time was not
measured here; the research's 1.5 ms warm 4 KiB read at 4 MiB blocks is not re-measured.

Not run: moto, MinIO, SeaweedFS (RustFS only; its counts matched the directory remote within 3 requests of LIST paging);
macOS; the concurrency tests against a real S3; remote compaction of a repository with chunked (large) files or several zones.
