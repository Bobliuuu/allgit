# Merge engine

Status: **implemented** in `crates/vcs-merge` (M4, part 1) and **wired into the CLI** (`vcs merge`, M4 part 2; see
"Merge command" below). Pure functions: no filesystem, network or async. Stores are
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
conflict *objects* in the object graph (conflicts are persisted only in `.vcs/conflicts`, see below).

## Merge command (`vcs merge`)

Implemented in `crates/vcs-repo/src/merge.rs`; the pure engine above does the work, the repo layer only reads trees,
stores `new_blobs`, and moves the working tree/index/ref.

* `vcs merge <branch> [--zone A]` merges the local branch into the zone's checked-out branch. `--remote` merges the
  remote-tracking head of `<branch>` (as last fetched, shown as `origin/<branch>`) instead; `vcs sync` prints this
  command when a branch has diverged.
* Target already contained in head: "already up to date". Target descends from head: fast-forward (no commit).
  Otherwise the merge base is computed; unrelated histories and criss-cross histories (several best bases) are
  errors (recursive merge is not implemented). A dirty zone (staged or unstaged changes) refuses to start.
* Clean: a merge commit with two parents (`[ours, theirs]`), same zone, fresh change id, a one-update transaction;
  the working tree and index move first, the branch ref last.
* Conflicts: nothing is lost. Merged files are written; each conflicted file is materialized with diff3 markers
  (text), or as our side (binary, mode) / the surviving side (delete/modify) with no markers. The structured list
  is stored in `.vcs/conflicts` (framed `AGMC`, atomic): zone, branch, ours/theirs heads, touched paths, and per
  conflict path, kind, base/left/right ids and modes. A valid file whose "ours" head still equals the branch head
  is the "merging" state (`vcs status` shows it). A file whose head no longer matches is stale (crash after the ref
  moved) and ignored; a corrupt file is an error.
* `vcs commit` refuses while any conflict is unresolved. A conflict is resolved by `vcs add <path>` (editing the file
  first; deleting it and adding the removal also counts). For marker files the staged content must differ from the
  marker file, so unedited markers cannot be committed by accident. The commit gets the recorded second parent.
* `vcs conflict list` and `vcs conflict show <path>` inspect the state. `vcs merge --abort` restores the pre-merge
  tree, index and files exactly (discarding edits to merge-touched files); it refuses if any other file in the zone
  changed. `checkout`/`zone remove` refuse during a merge.
* Not done: file<->directory conflicts (the merge is refused before anything is touched), merging several zones in
  one command, rename detection. Merge quality of Myers vs patience was only compared on a
small hand-written corpus, not on real repositories; no timing numbers exist.
