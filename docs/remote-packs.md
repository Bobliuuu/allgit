# Remote packs

Since this change, `Publisher` writes the new objects of a transaction as **one pack** under `packs/`, and clone/fetch read
packs by index plus byte ranges. Loose remote objects (`objects/xx/...`) are still read, and a remote may hold both.
`Publisher::with_loose_objects()` keeps the old write layout (used by tests of the legacy layout).

Pack format: unchanged, see [object-model.md](object-model.md). Remote names are `packs/pack-<hex of pack checksum>.pack`
and `.idx`; the name is content-addressed, so re-uploading after a crash is idempotent.

## Publish

```text
Collect   find the new objects without asking the remote about individual objects
PutPack   put_if_absent packs/<name>.pack
PutIdx    put_if_absent packs/<name>.idx        <- the pack becomes visible here
PutTx     put_if_absent tx/<txid>
Load/Cas  manifest read, then compare-and-swap  <- the publish becomes visible here
```

A crash after `PutPack` leaves a pack without an index: readers only enumerate `.idx` keys, so it is ignored (an orphan; there
is no remote delete, so it stays). A crash after `PutIdx` leaves a complete, closed pack that no manifest references; also
harmless. Zero objects: no pack. One object: a loose object (a pack would cost two requests instead of one).
Objects inside the pack are ordered commits, then trees, then everything else, so a clone can read the metadata as a prefix.

### Why skipping the per-object HEAD walk is sound

Invariant (as before): **if an object is visible on the remote, its whole dependency closure is visible.** Visible means: a
loose object, or an object listed in a pack whose index exists.

Preserved by writes: a loose upload is dependency-first (unchanged). A pack becomes visible by one PUT (the index, written
after the pack). The pack holds every object the walk found missing, and the walk prunes only at objects in the set `known`
described next, whose closures are visible. So at the instant the index appears, every dependency of every pack object is in
the pack or already visible. The model test checks this after every single write, not only at the end.

`known` (packed Collect, `Publisher::collect_packed`) contains only:

1. every `update.old` of the transaction. `Load` aborts with `Diverged` unless the manifest head equals `old` at the moment of
   the CAS, so if the transaction commits, `old` was a published head, and heads' closures are visible (the invariant plus
   "nothing is ever deleted"). If it does not commit, nothing was published and the extra pruning did no harm.
2. when some update creates a branch (`old = None`): all branch heads of the manifest read at Collect. Manifest heads only
   ever point at objects whose closure was visible when the manifest was written, and visibility is monotone.
3. ancestors of those heads (commits reachable through local parent links), and commits for which the loose `HEAD` succeeds
   (legacy remotes; one HEAD per unknown commit, never per tree entry). Closure of a visible commit is visible.
4. entries of a tree that is the same-named directory entry of the root (recursively) of a present parent commit: the parent's
   closure is visible, so each entry id listed in that tree is. Chunk ids of the previous version of a chunked file likewise.

What is not detected (re-sent, harmless because pack objects are deduplicated by content on the reader): content moved to a
different directory or renamed, and trees whose parent commit is not derivable from known heads. This costs bytes only.
An object already present remotely and sent again does not break anything: readers resolve an id to bytes by hash.

Consequences for the cost: a 1-file change in a 5,000-entry directory costs the same requests as in a 5-entry directory.
The local work is one read of the old trees along the changed path plus a local ancestor walk when a new branch is
published from a commit that is not a head.

Mixed layouts: packed Collect treats loose and packed history the same (it never asks about trees), so a loose publish on top
of packs and a packed publish on top of loose both work (`mixed_loose_and_packed_remote`).

## Fetch

`RemoteSource` lists `packs/` once and downloads each index once (`1 + packs` requests), then answers "which pack has it" from
memory. The first time a walk needs an object from a pack it reads ahead: for a lazy walk (commits and trees) the leading
metadata in ranged GETs of 4 KiB growing x4 to 4 MiB, stopping at the first file blob; for a full walk of all zones the whole
pack in one GET; for a zone-filtered full walk, one ranged GET per object. A pack the walk never touches is not read, so an
incremental fetch costs about the new packs. Everything read ahead is keyed by the hash of its bytes, so a lying pack can only
cause a miss, never plant an object; ranged reads are verified against the requested id.
Objects not in any pack are fetched loose, as before. Many small packs (one per publish) mean one index GET each per fetch;
there is no compaction yet (no remote delete in `Backend`).

## Tests

`crates/vcs-remote/tests/model.rs` explores every interleaving, crash and lost CAS for loose and packed layouts and checks
the closure invariant after each write. `tests/packs.rs` covers the request counts, mixed layouts, a new branch off an old
commit, moved directories, crash between pack and index, and a corrupt pack.
