# Object model (format v1)

Status: **implemented** in `crates/vcs-objects` (tests: unit + proptest). Items marked *(designed)* are not yet implemented.

## Identity

```
object bytes = kind:u8 || format_version:u8 || payload
ObjectId     = BLAKE3 derive_key("all-git 2026 object identity v1")(object bytes)   // 256 bit
```

* Domain separation: the derive-key context isolates this system's hashes from any other BLAKE3 use; the kind byte
  separates object types (`Blob` and `Tree` with identical payload bytes get different ids – tested).
* The format version is *inside* the hashed bytes. A future v2 encoding yields different ids by construction; readers
  reject unknown versions (`DecodeError::Version`).
* Stores verify `hash(bytes) == id` on every read (`StoreError::Corrupt`).
* Why BLAKE3 over SHA-256: ~several× faster on one core and tree-parallel for large blobs (relevant to the 1M-file and
  large-asset targets); 256-bit security. Cost: no hardware-SHA ubiquity, no FIPS. Revisit only if a compliance
  requirement appears. *Not benchmarked yet* – claim is from BLAKE3's published design, not our measurements.

## Canonical encoding

Hand-written codec (`vcs-objects::codec`) instead of a general serializer, so canonicality is a property we can test:

* integers: unsigned LEB128, **minimal form only** (decoder rejects `0x80 0x00`);
* strings/bytes: varint length + bytes; strings must be UTF-8;
* collections: sorted by a total order and **verified sorted on decode** (trees by name bytes, snapshot areas, transaction
  updates by `(area, branch)`); duplicates rejected;
* decode is strict: trailing bytes are an error;
* element counts are bounded by remaining input (no attacker-controlled preallocation).

Property tested: `decode(bytes) = Ok(o)  ⇒  encode(o) == bytes` (one logical value ⇔ one byte string), plus
round-trip, order-independence of tree construction, and no panics on arbitrary input.

## Objects

| Kind | Payload | Notes |
|---|---|---|
| Blob | raw bytes | Whole-file for now; chunk manifests are Milestone 5 *(designed)* |
| Tree | sorted `(name, mode, id)` | modes: File, Executable, Directory. Names validated (no `/`, `\`, NUL, `.`, `..`, `.vcs` any case) |
| Commit | `area, parents[], root, change_id, author, message` | one **area**; `root` is the tree of that area, paths area-relative |
| Snapshot | `area → commit`, author, message | pins a consistent set of area versions |
| Transaction | sorted `RefUpdate{area, branch, old?, new}`, author, message | immutable record of one atomic publication |
| Operation | parents[], view (local branch heads + workspace subs), description, author | Kind 6; see operations.md |
| Conflict | – | *(designed, Milestone 4)* |

Symlinks are intentionally unsupported in v1 (they are the main path-escape vector); they are skipped with a warning on
`add` and refused on checkout traversal.

## Graph invariants

1. Objects are immutable; ids are content hashes ⇒ the object graph is acyclic by construction (no object can reference a
   descendant's hash before it exists).
2. `Commit.parents` are commits of the **same area** as the commit. (Checked by `Repo::log` and `fsck`; the codec cannot
   check it because it does not see the parents.)
3. `Commit.root` resolves to a Tree; Tree entries resolve to Blob (File/Executable) or Tree (Directory). (`fsck`)
4. A branch ref `(area, branch)` points to a Commit whose `area` equals the ref's area. (`fsck`)
5. Area paths are pairwise non-overlapping; `root` owns everything not owned by another area. Every tracked path has exactly
   one owner, and a commit of area A only contains paths owned by A. (Enforced at area creation, on checkout `transition`,
   and when decoding the areas file.)
6. A Snapshot's commits each belong to the area they are keyed under *(checked when created from refs; `fsck` currently
   checks existence only)*.

## One global graph, many areas – the decision

**Problem.** Areas must evolve independently (no repo-wide linear history, so `engine` and `assets` writers don't conflict),
yet a cross-area change must be atomic and a consistent cross-area state must be nameable.

**Alternatives considered**

| Model | Independent area history | Atomic cross-area change | Cost |
|---|---|---|---|
| One commit with several area roots | ✗ every commit touches the global root; writers serialize on one head | ✓ | Git-monorepo contention, defeats sparse sync |
| Global snapshot commit per change | ✗ same as above | ✓ | same |
| Per-area commits + **Transaction** record + **Snapshot** pins | ✓ | ✓ (via single ref CAS, below) | one extra small object per publish |

**Chosen:** per-area Commit DAGs; each atomic publication writes an immutable **Transaction** listing every
`(area, branch, old → new)`; **Snapshot** objects pin named consistent combinations. All three kinds live in one
content-addressed store, so there is one object graph (shared blobs/trees dedupe across areas; one GC reachability
walk), but there is no forced total order between areas. Cross-area consistency is expressed by Transactions (what moved
together) and Snapshots (what is pinned together), never by shared ancestry.

**Reachability roots (for GC, designed):** all branch refs → commits → parents/trees/blobs; all named snapshots →
their commits; Transactions referenced by the retained manifest history window; in-flight (unpublished) transactions
protected by a lease/epoch (see architecture §GC). Transactions are *not* reachable from commits; they are reachable
from the transaction log.
