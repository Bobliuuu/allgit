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

Rename/copy detection (a rename is delete+add), semantic/structural merge beyond the prototype below (M6), histogram diff, recursive merge of
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

## Structural merge prototype (feature `structural`, off by default)

Research prototype for issue #10, `crates/vcs-merge/src/structural/`. `cargo build` does not compile tree-sitter; enable
it with `--features structural`. It is a library function (`structural::merge_rust`) and is **not** wired into
`merge_trees` or `vcs merge`.

* Rust only. base, left and right are parsed with tree-sitter-rust. Top-level items (and, recursively, the items inside
  `impl`, `trait` and inline `mod` bodies) are keyed by kind and name; comments and attributes travel with the item that
  follows them. The key sequences are merged with the same diff3 (so moves and independent insertions resolve), then each
  item is resolved: identical, one side unchanged, or a clean text merge of just that item (containers recurse).
* It declines, and the ordinary text merge result is returned, when an input has a parse error, when key order conflicts
  (both sides insert at the same spot), or when an item conflicts (same lines edited, delete versus modify, add/add).
* Parse guarantee: when all three inputs parse, a structural result is returned only if the output parses. If the text
  fallback is clean but its output does not parse, the result is downgraded to a single whole-file conflict.

Measured with `cargo run -p vcs-merge --features structural --example structural_corpus --release -- 50` (550 generated
scenarios, 50 per kind, Myers; the generator is `structural/corpus.rs` and is deterministic). "Files" counts scenarios
that end conflicted; "hunks" counts conflict hunks.

| kind | text files | structural files | text hunks | structural hunks |
|---|---|---|---|---|
| move_fn_vs_edit_it | 43 | 0 | 43 | 0 |
| move_fn_vs_edit_other | 7 | 0 | 7 | 0 |
| add_fn_both_at_end | 50 | 50 | 50 | 50 |
| delete_vs_edit_same_fn | 50 | 50 | 50 | 50 |
| same_line_conflict | 50 | 50 | 50 | 50 |
| add_method_vs_edit_last, delete_fn_vs_edit_other, edit_different_fns, edit_method_vs_insert_after, edit_two_methods, left_syntax_error (6 kinds, 300 scenarios) | 0 | 0 | 0 | 0 |
| total (550) | 200 | 150 | 200 | 150 |

All 550 clean structural outputs parsed (0 unparsable). The gain is entirely in moves; line diff3 already merges edits
to different functions when blank lines separate them. The corpus is synthetic and small, so these numbers say nothing
about real code.

## Assisted merge (`vcs merge --assist`)

An external command may *propose* resolutions for text conflicts. No model is bundled: the command is whatever you
supply (a script, a wrapper around a hosted model, ...). Implemented in `vcs-merge/src/assist.rs` (pure protocol and
validation) and `vcs-repo/src/assist.rs` (process running, proposal storage).

```text
vcs merge <branch> --assist "<command>" [--assist-timeout SECS]   # default 30s per conflict
vcs resolve                       # show each proposal as a diff from our side, plus the assistant's explanation
vcs resolve --accept [paths...]   # write the proposal into the file and stage it (marks the conflict resolved)
vcs commit                        # as usual, once every conflict is resolved
```

* The merge runs normally first. Only if it stops with conflicts is the command run, once per eligible conflict, through
  `sh -c` in the repository root (`cmd /C` off unix), with the request on stdin. Eligible: add/add and modify/modify
  where both sides and the base are UTF-8 text without NUL. Others (binary, delete/modify, mode, file/directory) are
  skipped and reported.
* **Never applied silently.** Running the assistant writes only `.vcs/proposals/`. The working tree keeps its conflict
  markers, the conflict stays unresolved, and `vcs commit` still refuses. `vcs resolve --accept` is the only way in.
  Accept refuses a file you edited since the merge stopped (`--force` overrides). Proposals are deleted when the merge
  ends (commit or `--abort`), are tied to the two merge heads, and a re-run replaces them.

Request (one JSON object on stdin; `base` is `null` for add/add):

```json
{"version":1,"path":"src/a.rs","language":"rust","base":"...","ours":"...","theirs":"..."}
```

Response (one JSON object on stdout, exit status 0):

```json
{"resolution":"<the complete resolved file>","explanation":"optional, shown to the reviewer"}
```

Validation; anything failing is ignored (reported as "assistant answer ignored", the conflict is left as it was):
stdout over 1 MiB (the process is killed), not UTF-8, not JSON (nesting deeper than 32 is refused), not an object, no
string `resolution`, a NUL in it, or any line starting with a conflict marker (`<<<<<<<`, `|||||||`, `=======`,
`>>>>>>>`). Unknown fields are ignored; `explanation` is truncated to 4 KiB. A non-zero exit, a failure to start, or
running past the timeout also yields no proposal. stderr is discarded.

Not validated: that the resolution is a *good* merge, or that it parses in the file's language. That is what the diff
review is for. On timeout only the shell is killed; a grandchild that ignores that may linger until it exits. Not done:
`vcs rebase --assist`, a `--reject` command, batching several conflicts per call, and issue #86's wider `vcs resolve`
(this change provides the minimal `resolve` / `resolve --accept` it needs).

Tested with scripted fake assistants (shell scripts) in `crates/vcs-cli/tests/assist.rs`: good answers, request shape,
garbage of eight kinds, oversized output, timeout, edited-file refusal, binary skip; protocol unit tests live beside the
parser.
