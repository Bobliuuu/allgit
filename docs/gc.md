# Garbage collection

Status: **design + local dry run + local touch-on-dedup.** `vcs gc --dry-run` (`Repo::gc_plan`) is implemented and tested. No command deletes
anything, and no destructive sweep, local or remote, exists. Tracks issues #33 and #43.

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

## What the remote needs (not built)

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
3. Local `vcs gc` sweep under the repo lock, with crash tests.
4. Remote design items 1-7, model test first, then implementation.

## Limits of the dry run

* Reports what would be deleted by the rules above; it does not prove that a real sweep is safe (see the dedup hole).
* Reads the store without the repo lock, so numbers can be stale if another process is writing.
* Object age comes from the file's modification time, which is not preserved by every copy or backup tool.
* Counts, not bytes: sizes are not reported.
