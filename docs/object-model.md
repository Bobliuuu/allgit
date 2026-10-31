# Object model (format v3)

Status: **implemented** in `crates/vcs-objects` (tests: unit + proptest). Items marked *(designed)* are not yet implemented.

## Identity

```
object bytes = kind:u8 || format_version:u8 || payload
ObjectId     = BLAKE3 derive_key("all-git 2026 object identity v3")(object bytes)   // 256 bit
```

* Domain separation: the derive-key context isolates this system's hashes from any other BLAKE3 use; the kind byte
  separates object types (`Blob` and `Tree` with identical payload bytes get different ids – tested).
* The format version is *inside* the hashed bytes. A new encoding yields different ids by construction; readers
  reject unknown versions (`DecodeError::Version`). **v2** added the chunk manifest kind (below); the bump changed every
  object id, so v1 repositories and remotes are not readable by v2 code (there is no migration; the project is pre-release).
* Stores verify `hash(bytes) == id` on every read (`StoreError::Corrupt`).
* Why BLAKE3 over SHA-256: ~several× faster on one core and tree-parallel for large blobs (relevant to the 1M-file and
  large-asset targets); 256-bit security. Cost: no hardware-SHA ubiquity, no FIPS. Revisit only if a compliance
  requirement appears. *Not benchmarked yet* – claim is from BLAKE3's published design, not our measurements.

## Canonical encoding

Hand-written codec (`vcs-objects::codec`) instead of a general serializer, so canonicality is a property we can test:

* integers: unsigned LEB128, **minimal form only** (decoder rejects `0x80 0x00`);
* strings/bytes: varint length + bytes; strings must be UTF-8;
* collections: sorted by a total order and **verified sorted on decode** (trees by name bytes, snapshot zones, transaction
  updates by `(zone, branch)`); duplicates rejected;
* decode is strict: trailing bytes are an error;
* element counts are bounded by remaining input (no attacker-controlled preallocation).

Property tested: `decode(bytes) = Ok(o)  ⇒  encode(o) == bytes` (one logical value ⇔ one byte string), plus
round-trip, order-independence of tree construction, and no panics on arbitrary input.

## Objects

| Kind | Payload | Notes |
|---|---|---|
| Blob | raw bytes | A whole file below 1 MiB, or one chunk of a large file |
| Chunked (kind 7) | `total_len`, ordered `(chunk_id, len)` | A file of at least 1 MiB; see Chunked blobs |
| Tree | sorted `(name, mode, id)` | modes: File, Executable, Directory, Symlink. Names validated (no `/`, `\`, NUL, `.`, `..`, `.vcs` any case) |
| Commit | `zone, parents[], root, change_id, author, message` | one **zone**; `root` is the tree of that zone, paths zone-relative |
| Snapshot | `zone → commit`, author, message | pins a consistent set of zone versions |
| Transaction | sorted `RefUpdate{zone, branch, old?, new}`, author, message | immutable record of one atomic publication |
| Operation | parents[], view (local branch heads + workspace subs), description, author | Kind 6; see operations.md |
| Conflict | – | *(designed, Milestone 4)* |

## Symlinks and portable paths (format v3)

Format v3 adds the tree entry mode `Symlink` (byte 4). Its id names a Blob holding the raw link target, so identical
targets dedupe like any blob. **This is a format bump:** `FORMAT_VERSION` went 2 to 3, every object id changed, and
v1 and v2 objects are rejected with `DecodeError::Version` (the golden id in `golden_blob_id_is_stable` was updated on purpose).
There is no migration tool: existing v1/v2 repositories must be re-created (for example by exporting and re-importing).

A target is accepted only if it provably stays inside the worktree (`vcs_repo::portable::check_symlink_target`):
relative, valid UTF-8, no NUL or backslash, no empty component, never names `.vcs`, and `..` only as a leading run that
does not climb above the worktree root. `..` after a normal component is refused because that component may itself be a
link, which makes a lexical resolution unsound. `add` stages a valid link as a Symlink entry (an explicit pathspec with
an escaping link is an error; a directory walk skips and reports it). Checkout re-validates every link from the stored
blob (a crafted commit can contain anything), creates it by renaming a temporary link into place so it never writes
through an existing one, and still refuses to write beneath a symlinked directory. Merges never text-merge two link
targets: differing targets are a conflict. `export` writes mode 120000. Git import still skips symlinks.

Case-insensitive filesystems: `add` and checkout refuse two paths (or directory prefixes) whose names are equal after
Unicode NFD normalization and lowercasing, e.g. `README` and `readme`, or a precomposed and a decomposed `e` with
an accent. The check is applied everywhere, not only on a folding filesystem, so a repository stays checkout-able on
macOS and Windows. It uses simple lowercasing, not full Unicode case folding or Windows 8.3 short names; exotic
equivalences (for example `ß` against `SS`) are not detected.

Windows limits: symlink entries are tracked and hashed, but checkout of a Symlink entry fails with a clear error on
non-unix platforms (creating links needs privilege or Developer Mode, and file-versus-directory links differ). Reserved
device names (`CON`, `NUL`, ...), trailing dots or spaces and the 260-character path limit are not checked. None of the
Windows behavior is exercised by tests (the suite runs on unix).

## Graph invariants

1. Objects are immutable; ids are content hashes ⇒ the object graph is acyclic by construction (no object can reference a
   descendant's hash before it exists).
2. `Commit.parents` are commits of the **same zone** as the commit. (Checked by `Repo::log` and `fsck`; the codec cannot
   check it because it does not see the parents.)
3. `Commit.root` resolves to a Tree; Tree entries resolve to Blob (File/Executable) or Tree (Directory). (`fsck`)
4. A branch ref `(zone, branch)` points to a Commit whose `zone` equals the ref's zone. (`fsck`)
5. Zone paths are pairwise non-overlapping; `root` owns everything not owned by another zone. Every tracked path has exactly
   one owner, and a commit of zone A only contains paths owned by A. (Enforced at zone creation, on checkout `transition`,
   and when decoding the zones file.)
6. A Snapshot's commits each belong to the zone they are keyed under *(checked when created from refs; `fsck` currently
   checks existence only)*.

## Zone names, ids and display names

Commits embed their zone name and are immutable, so a rename cannot rewrite them without changing every commit id
(which would diverge every published clone). **Decision:** the embedded name is the zone's permanent *id*; renaming sets
a *display name* kept in the `zones` file and, once published, in the remote manifest. Refs, snapshots, transactions,
workspace, operation views and manifest branches all use ids; user-facing commands resolve a display name (or the id) to
the id first. Rejected: rewriting history (new commit/snapshot/op ids, breaks published clones and the op log).
Invariant: ids and display names share one namespace, so a name is never ambiguous.

## One global graph, many zones – the decision

**Problem.** Zones must evolve independently (no repo-wide linear history, so `engine` and `assets` writers don't conflict),
yet a cross-zone change must be atomic and a consistent cross-zone state must be nameable.

**Alternatives considered**

| Model | Independent zone history | Atomic cross-zone change | Cost |
|---|---|---|---|
| One commit with several zone roots | ✗ every commit touches the global root; writers serialize on one head | ✓ | Git-monorepo contention, defeats sparse sync |
| Global snapshot commit per change | ✗ same as above | ✓ | same |
| Per-zone commits + **Transaction** record + **Snapshot** pins | ✓ | ✓ (via single ref CAS, below) | one extra small object per publish |

**Chosen:** per-zone Commit DAGs; each atomic publication writes an immutable **Transaction** listing every
`(zone, branch, old → new)`; **Snapshot** objects pin named consistent combinations. All three kinds live in one
content-addressed store, so there is one object graph (shared blobs/trees dedupe across zones; one GC reachability
walk), but there is no forced total order between zones. Cross-zone consistency is expressed by Transactions (what moved
together) and Snapshots (what is pinned together), never by shared ancestry.

**Reachability roots (for GC, designed):** all branch refs → commits → parents/trees/blobs; all named snapshots →
their commits; Transactions referenced by the retained manifest history window; in-flight (unpublished) transactions
protected by a lease/epoch (see architecture §GC). Transactions are *not* reachable from commits; they are reachable
from the transaction log.

## Packs (Milestone 5, local store implemented)

Loose objects (`objects/<aa>/<rest>`) stay the write path. `vcs repack` moves every loose object into one pack under
`objects/pack/`; readers consult loose files first, then packs. Object ids and encodings are unchanged: a pack only
stores the same encoded bytes, so hashes, sync and GC reachability are unaffected.

`pack-<name>.pack` (all integers little-endian), `<name>` = hex of the trailer:

| Field | Size | Notes |
|---|---|---|
| magic `VCSPACK1` | 8 | |
| count | u32 | number of entries |
| entries | repeated | `len: u32` then `len` bytes of the encoded object, sorted by id |
| trailer | 32 | BLAKE3 of everything before it |

`pack-<name>.idx`:

| Field | Size | Notes |
|---|---|---|
| magic `VCSPIDX1`, count u32, pack length u64, pack trailer | 8+4+8+32 | ties the index to one pack |
| entries | 44 each | `id[32]`, `offset u64` (of the payload), `len u32`; strictly sorted by id |
| trailer | 32 | BLAKE3 of the index before it |

- Lookup: the index is loaded in memory, binary-searched, then exactly one ranged read `[offset, offset+len)` fetches the
  object; its hash is checked against the id like any loose read. The ranged read is what a remote `Backend` range
  request would use later (not implemented yet).
- Write order: pack renamed into place, then index. A pack without an index is ignored (crash-safe). Loose files are
  deleted only after both are durable.
- Open refuses a pack whose length differs from the index (truncation) or an index with a bad checksum/ordering, rather
  than silently hiding objects. `fsck` additionally re-hashes the whole pack, checks each entry's framing and every
  object hash.
- Not done: compression, deltas, multi-pack merging, range reads in `Backend`, chunking (see issue #9).

### When packs help, and when they do not

Measured (docs/benchmarks.md, 100 files x 256 KiB random data x 4 versions): with no compression or deltas, a pack is
the same size as the loose objects (apparent bytes +0.02%; allocated bytes -1.6%). What packs change today:

- **Help:** file count (421 files became 2), per-object filesystem overhead, and remote fetch. A reader can GET the small
  index and then one byte range of the pack (`vcs_remote::packfetch::fetch_from_pack`, tested to read well under 10% of
  a 50-object pack) instead of one request per loose object or the whole pack.
- **Do not help:** total bytes, because nothing is compressed or delta-encoded, so they will not shrink random or
  already-compressed binaries. Whole-pack `fsck` costs one extra hash pass over the pack on top of the per-object
  hashes. Repack rewrites all loose objects into a new pack each time; there is no incremental or multi-pack merge yet.
- **Not measured:** remote latency benefit, checkout speed from packs, and repos with many small text objects.

## Chunked blobs (format v2, implemented)

Measured dedup of the chunker prototype (docs/benchmarks.md): chunks stored 41-43% of whole-blob bytes at 8 KiB average
size and 66-67% at 32 KiB, on versioned random binaries with small edits. Implemented as:

- Object kind **Chunked** (7): `total_len: varint`, `count: varint`, then `count` x `(chunk_id: 32 bytes, len: varint)`.
  Chunks are ordinary Blob objects, so they dedupe across files, versions and zones and travel through packs, sync and
  GC unchanged. A Chunked object has its own id and a Tree entry points at it exactly as it would at a Blob.
- One canonical id per content: the writer chunks **iff `len >= 1 MiB`** (`CHUNK_THRESHOLD`), with gear-hash CDC
  parameters min/avg/max = 4/16/64 KiB fixed by the format. `vcs_objects::content_id(bytes)` is the id a file has in a
  tree; `ObjectStore::put_content` / `get_content` write and reassemble. Changing the threshold or parameters is a format
  bump, never silent. The manifest/chunk golden test pins the chunker output.
- Decode is strict: at least 2 chunks, `total_len >= threshold`, every chunk length in `1..=64 KiB`, lengths sum to
  `total_len`, no trailing bytes.
- Reads: `get_content` fetches each chunk (hash-verified like any object), checks its length against the manifest and
  concatenates. A missing chunk is `NotFound(chunk)`, so lazy fetch pulls just that chunk.
- `fsck`: every chunk of a manifest must exist (or be refetchable) and be a Blob of the recorded length. GC reachability
  and sync dependency order (chunks before the manifest) follow manifest entries. Manifests cannot nest because a chunk
  must be a Blob.
- Remote: `fetch_object` falls back to packs under `packs/<name>.pack|.idx` when an object is not stored loose, using
  `fetch_from_pack` (one index GET plus one ranged GET). `packfetch::upload_pack` puts a local pack there. Publish still
  uploads loose objects; nothing uploads packs automatically yet.
- Not done: ranged reads that map a byte range to chunks (reads reassemble the whole file in memory), streaming
  checkout, and avoiding the full-file re-hash in `status` when mtime/size changed (it now also re-chunks large files).
