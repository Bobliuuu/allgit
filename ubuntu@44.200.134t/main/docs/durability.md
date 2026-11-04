# Ref-update durability: the operation journal

A ref-changing operation (`tag`, `branch`, `commit`, `merge` fast-forward, `snapshot`, ...) must
never lose an acknowledged update, never leave a torn ref, and stay safe with concurrent
processes. Before the journal it paid four serial fsyncs per operation (the `refs` file, the
operation object, its directory, `op_head`). Now it pays **one**.

## The protocol

State files involved: `refs`, the `Operation` object, `op_head`, and `.vcs/oplog.journal`, a fixed
64 KiB file preallocated with zeros (an append overwrites existing blocks, so `fdatasync` needs no
metadata commit).

1. Data objects the new state names (commits, trees, annotation) are made durable by the usual
   `store.sync()` barrier. Unchanged.
2. Append one record `{operation id, complete new refs file, encoded Operation}` (length +
   generation + checksum framed) to the journal and `fdatasync` it. **This is the commit point.**
3. Rename the new `refs` into place, store the operation object as a loose file, rename the new
   `op_head` into place. None of these is fsynced.

A reader or crash can see any prefix of step 3 (and, after a power cut, any subset of it). All of
it is derived data: the journal record holds every byte.

## Recovery (roll forward)

The newest record is re-applied if `refs`, the operation object or `op_head` differ from it:

- in `Repo::lock`, right after the lock is taken and before any caller reads `refs`: a process
  killed after the commit point is repaired by the next writer;
- at open after a reboot: the journal header stores the boot id (Linux
  `/proc/sys/kernel/random/boot_id`, macOS `kern.bootsessionuuid`) of the boot that started the
  current generation. A power cut implies a reboot, so a different boot id means unsynced renames
  may be gone: every journalled operation object is re-created from its record, `refs` and
  `op_head` rewritten, everything made durable, and the journal emptied. A mere process crash
  loses nothing in the page cache, so the common open pays only a small read of the header.

A record torn by a crash fails its checksum and ends the scan: it was never acknowledged, so the
operation "did not happen" (all-or-nothing; `refs` and the operation log never disagree).

## Checkpoint

The journal must not grow. A *checkpoint* fsyncs `refs`, `op_head`, the journalled operation
objects and the directory, then starts a new generation (header rewritten and `fdatasync`ed; old
records are ignored by generation). It runs

- when the 64 KiB journal is full (every few hundred small operations), and
- before **any other writer** of `refs` or `op_head`: `Repo::write_refs` and `write_op_head` are
  the fully synced funnel for operations that have not been converted (rebase steps, imports,
  checkout, ...). This keeps the invariant that makes roll-forward safe: while the journal holds
  records, `refs`/`op_head` change only through `commit_refs_op`. A test
  (`no_raw_refs_writes_outside_the_journal_funnel`) fails the build if a new raw `refs` write
  appears.

If `refs` is neither the newest record nor an older state of the same generation (hash of the
file at generation start plus every record's refs), somebody else wrote it (an older vcs, a hand
edit). Recovery then leaves `refs` and `op_head` alone and drops the journal; it never rolls such
a write back.

## Why this is safe

- Commit point = one `fdatasync` of data already written to the journal; everything the record
  names (objects) was made durable before it.
- Before the commit point nothing observable changed, so a crash there is a clean "never
  happened". After it the record can reproduce all derived state, in any partial order the OS
  chooses to persist.
- Concurrent processes serialise on the repository lock exactly as before; recovery runs inside
  the lock.
- Directory fsync is only needed at journal creation and at checkpoint; renames of `refs`,
  `op_head` and op objects are repairable from the journal until the checkpoint makes them
  durable.

## Limits (honest)

- Without a boot id (Windows, other unix) the journal is disabled and the old fully synced
  protocol is used.
- An *older vcs binary* does not know the journal. If it rewrites `refs` back to exactly a state
  the journal also holds, the next new vcs would roll it forward. Use one version per repository.
- The crash tests model power loss by reverting the unsynced files to a baseline image; they do
  not cut real power.
- On macOS `fdatasync` is `F_FULLFSYNC`, so the single sync is the expensive hardware flush.

Tests: `crates/vcs-repo/tests/journal_crash.rs`.

## Deferred durability for ref-only operations (default, like git)

Git does not fsync refs by default; neither does vcs for operations that only move refs:
`branch` (create, `-d`), `tag` (create, delete), `switch -c`. They still append the same journal
record and rename `refs`/`op_head` atomically, but skip the `fdatasync` of the journal (the 6 ms
cost that made them 4-6x slower than git on wall time, issue #494). A file `.vcs/oplog.dirty`
marks "journal records not yet synced".

**What can be lost.** After a *power cut or kernel crash* only, the most recent ref-only updates
since the last flush (a branch or tag just created or deleted). The repository is never torn:
the journal checksum drops a partial record, `refs` is replaced by rename, and the previous
consistent state (refs, op log and `op_head` agreeing) is what you get back. After `kill -9` or a
process crash nothing is lost: the data is in the page cache. **Committed objects are never lost
or corrupted by this**: commits, trees, blobs, annotations, `commit`, `merge`, `rebase`, `snapshot`,
imports and every other operation keep their full durability, and an update is only ever deferred
after the objects it names were made durable.

**When it becomes durable.** The next object-writing barrier (`add`, `commit`, ... any
`ObjectStore::sync` with something pending), any journalled operation that syncs (it covers the
earlier records, they share the file), a checkpoint, `gc`, or `Repo::flush_refs()` for embedders.

**Restoring per-op durability.** `vcs config set core.fsync refs` (repo or `--user`), or the
environment `VCS_FSYNC_REFS=1` (or `VCS_FSYNC=refs`). Every ref-only operation then pays its
`fdatasync` at commit time, exactly as described above. Library users call
`vcs_repo::set_fsync_refs(true)`.

**Not weakened.** The server and remote paths do not go through this journal: publishing a
manifest (`put_if_match`/`put_if_absent` in the directory backend, the S3 CAS) writes and fsyncs
its temp file before the rename, and the server's audit log is fsynced at graceful shutdown.
Pushes stay durable.

Tests: `crates/vcs-cli/tests/ref_durability.rs` (defer + flush behaviour, the knob, 120 randomly
timed `kill -9`s of ref operations each followed by a clean `fsck`),
`crates/vcs-repo/tests/journal_crash.rs` (power cut losing the deferred tail).
