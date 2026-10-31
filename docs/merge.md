# Merge engine

Status: **implemented** in `crates/vcs-merge` (M4, part 1). Pure functions: no filesystem, network or async. Stores are
read through `ObjectStore`/`BlobSource`/`CommitGraph`; nothing is written. The on-disk object format is unchanged.

## Algorithms

* **Merge base** (`merge_base`): ancestors of both commits, intersected, minus anything that is a strict ancestor of
  another common ancestor. Returns *all* best common ancestors sorted by id: empty for unrelated histories, several for
  criss-cross (caller decides; recursive base merging is not done here). O(reachable commits); generation numbers and a
  commit-graph index are M5.
* **Line diff** (`Algorithm::{Myers, Patience}`): Myers is the O(ND) shortest edit script (LCS-optimal). Patience
  anchors on lines unique to both sides via a longest increasing subsequence, recurses between anchors, and falls back
  to Myers when there are none. Histogram is not implemented. Neither uses hashing order or randomness.
* **Text merge** (`merge3`, diff3): lines keep their `\n`; operates on bytes. Base lines kept by *both* sides split the
  file into regions; per region: one side changed -> take it, both identical -> take it, else a `Conflict{base,left,right}`
  hunk quoting all three verbatim. `render_markers` prints `<<<<<<<` / `|||||||` / `=======` / `>>>>>>>`.
* **Tree merge** (`merge_flat`, `merge_trees`): per path, unchanged / one-side / both-same resolve silently; add/add,
  modify/modify, delete/modify, mode conflicts and file<->directory become structured `Conflict { path, kind, base, left,
  right (+modes), hunks }`. Mode and content merge independently. Binary (NUL in first 8000 bytes) modify/modify is a
  conflict without hunks. Conflicted paths are *not* in `entries`. Clean text merges produce new blobs returned in
  `new_blobs`; the caller stores them.

## Guarantees (property tested)

* `merge(x,x,x) = x`; `merge(b,l,b) = l`; `merge(b,b,r) = r`.
* Clean merges are symmetric in left/right; output is deterministic.
* Edits to non-adjacent lines always merge cleanly.
* No data loss: every line a side introduced is in the output or inside a conflict hunk; conflict hunks carry all three
  versions. A missing final newline is a real difference, never normalised (the marker renderer adds a `\n` only so the
  next marker starts on its own line).

## Not done

Rename/copy detection (a rename is delete+add), semantic/structural merge (M6), histogram diff, recursive merge of
multiple bases, whitespace-insensitive merge, subtree-skipping tree merge (the whole tree is flattened; M5), writing
merged trees/conflict objects (`Conflict` is not persisted yet). Merge quality of Myers vs patience was only compared on a
small hand-written corpus, not on real repositories; no timing numbers exist.
