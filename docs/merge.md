# Merge engine

Status: **implemented** in `crates/vcs-merge` (M4, part 1) and **wired into the CLI** (`vcs merge`, M4 part 2; see
"Merge command" below). Pure functions: no filesystem, network or async. Stores are
read through `ObjectStore`/`BlobSource`/`CommitGraph`; nothing is written. The on-disk object format is unchanged.

## Algorithms

* **Merge base** (`merge_base`): ancestors of both commits, intersected, minus anything that is a strict ancestor of
  another common ancestor. Returns *all* best common ancestors sorted by id: empty for unrelated histories, several for
  criss-cross (caller decides; recursive base merging is not done here). O(reachable commits) by default. With the
  commit-graph cache (`.vcs/commit-graph`, see architecture.md) the walk paints down from both tips in generation order
  and only visits commits newer than the bases; the result is identical.
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

Copy detection, directory-level rename inference, semantic/structural merge beyond the prototype below (M6), histogram diff, recursive merge of
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
* `vcs resolve` settles conflicts without hand-running `vcs add`, in both merge and rebase (they share
  `.vcs/conflicts`). `vcs resolve <path> --ours|--theirs|--base|--file <f>` writes that content, stages it and marks the
  conflict resolved; choosing a side that lacks the file (a delete/modify conflict, add/add with no base) removes it.
  "Ours" is the recorded left side (merge: the current branch; rebase: the branch being rebased onto), "theirs" the
  right (merge: the merged-in branch; rebase: the commit being replayed). Re-resolving a path is allowed.
  `vcs resolve <path> --mark` accepts the hand-edited working file (the same rule as `vcs add`: refused while the file
  is still the untouched marker file). `vcs resolve --check` exits non-zero while any conflict is unresolved or a
  resolved, staged text file still holds a `<<<<<<<` ... `>>>>>>>` block; binary files are not scanned. `--mark` itself
  does not scan, so `--check` is the guard to run before `vcs commit` / `vcs rebase --continue`.
* `vcs conflict list` and `vcs conflict show <path>` inspect the state. `vcs merge --abort` restores the pre-merge
  tree, index and files exactly (discarding edits to merge-touched files); it refuses if any other file in the zone
  changed. `checkout`/`zone remove` refuse during a merge.
* Not done: file<->directory conflicts (the merge is refused before anything is touched), merging several zones in
  one command. Rename detection exists but is opt-in (`--renames`, see "Rename-aware merge"). Merge quality of Myers vs patience was only compared on a
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

### Python, TypeScript and Go (features `structural-python`, `structural-typescript`, `structural-go`)

Issue #91. The item-matching code is shared; each language is a small table in `structural/langs.rs` (grammar, comment
kinds, which nodes are containers, how an item is keyed). Each feature is independent and off by default
(`vcs-merge`, and pass-through features of the same names in `vcs-repo` and `vcs-cli`); `structural::lang_for_path`
picks the driver by extension (`.py`, `.ts`/`.mts`/`.cts`, `.go`, plus `.rs`), and `merge_flat_with` uses it, so
`vcs merge` and `vcs rebase` use these drivers when built with the feature. The same fallbacks and parse guarantee apply.

* Python: `def` and `class` (also decorated) by name, top-level `name = ...` assignments by target, anything else by its
  normalized text. Class bodies are merged member by member; an indentation-delimited body gives its indentation to the
  first member so that moving it keeps the file valid.
* TypeScript (`.ts` only, no `.tsx`): declarations by kind and name, `export` unwrapped, `const/let/var` by first
  declarator name. Class, interface and namespace bodies are merged member by member.
* Go: functions by name, methods by receiver and name, `type`/`const`/`var` by the first spec name, imports by text.
  Methods are top-level declarations, so there is no nested container.

Measured with `cargo run -p vcs-merge --features structural,structural-python,structural-typescript,structural-go --example structural_corpus --release -- 50`
(550 generated scenarios per language, 50 per kind, Myers; same eleven kinds as above, generator
`structural/corpus.rs::generate_for`, deterministic). Conflicted files / conflict hunks:

| language | text files | structural files | text hunks | structural hunks | resolved by structural | unparsable clean outputs |
|---|---|---|---|---|---|---|
| rust | 200 | 150 | 200 | 150 | 350 | 0 |
| python | 262 | 150 | 262 | 150 | 350 | 0 |
| typescript | 200 | 150 | 200 | 150 | 350 | 0 |
| go | 200 | 150 | 200 | 150 | 350 | 0 |

In every language the 150 remaining conflicts are the three genuine kinds (same line, delete versus edit, both add at the
end), and `move_fn_vs_edit_it` (43 text conflicts) and `move_fn_vs_edit_other` (7) drop to 0. Python additionally gains
in `add_method_vs_edit_last` (50 to 0) and `edit_method_vs_insert_after` (12 to 0): with no closing brace, line diff3 sees
the last method of a class and the next top-level lines as adjacent. The corpora are synthetic, ASCII, gofmt/prettier-like
and small; they say nothing about real code, and real-repository conflict counts were not measured.

## Assisted merge (`vcs merge --assist`)

An external command may *propose* resolutions for text conflicts. No model is bundled: the command is whatever you
supply (a script, a wrapper around a hosted model, ...). Implemented in `vcs-merge/src/assist.rs` (pure protocol and
validation) and `vcs-repo/src/assist.rs` (process running, proposal storage).

```text
vcs merge <branch> --assist "<command>" [--assist-timeout SECS]   # default 30s per conflict
vcs rebase <onto> --assist "<command>" [--assist-timeout SECS]    # same, for the stopped step
vcs conflict proposals                # each proposal as a diff from our side, plus the assistant's explanation
vcs resolve <path> --proposal         # apply it: the ordinary `vcs resolve` path (writes, stages, marks resolved)
vcs commit                            # (or `vcs rebase --continue`) once every conflict is resolved
```

* The merge or rebase runs normally first. Only if it stops with conflicts is the command run, once per eligible
  conflict, through `sh -c` in the repository root (`cmd /C` off unix), with the request on stdin. Eligible: add/add and
  modify/modify where both sides and the base are UTF-8 text without NUL. Others (binary, delete/modify, mode,
  file/directory) are skipped and reported.
* **Never applied silently.** Running the assistant writes only `.vcs/proposals/`. The working tree keeps its conflict
  markers, the conflict stays unresolved, and `vcs commit` still refuses. `vcs resolve <path> --proposal` is the only
  way in; it overwrites the working file, like `resolve --file`. Proposals are deleted when the merge or rebase ends,
  are tied to the two heads of the stopped step (a proposal from an earlier rebase step is ignored), and a re-run
  replaces them.

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

A ready-made assistant for the Anthropic API, with its own validation, is described in [assist.md](assist.md); the
opt-in `merge.assist` setting makes `vcs merge`/`vcs rebase` run an assistant without `--assist`.

Not validated: that the resolution is a *good* merge, or that it parses in the file's language. That is what the diff
review is for. On timeout only the shell is killed; a grandchild that ignores that may linger until it exits. Not done:
a `--reject` command, and batching several conflicts per call.

Tested with scripted fake assistants (shell scripts) in `crates/vcs-cli/tests/assist.rs`: good answers, request shape,
garbage of eight kinds, oversized output, timeout, binary skip, rebase; protocol unit tests live beside the
parser.

## Rename-aware merge (`--renames`, `merge.renames`; off by default)

Issue #109, `crates/vcs-merge/src/rename.rs`. Plain `merge_flat` treats a rename as delete+add, so "A renames a file,
B edits it" is a delete/modify conflict plus an unrelated new file. `merge_flat_renames` (library) and
`vcs merge|rebase --renames` (or `vcs config merge.renames on`, env `VCS_MERGE_RENAMES`) fix that. `--no-structural`
is unchanged and composes with it.

* Detection (`detect_renames`, base against one side): only a path that vanished paired with a path that appeared.
  First by identical blob id and mode (same basename preferred; several identical candidates with no unique pairing are
  copies and are left alone). Then by similarity: a fingerprint is the sorted 64-bit hashes of the trimmed, non-blank
  lines (blake3, first 8 bytes); similarity is the Dice coefficient of the two line multisets, threshold 50 %
  (`rename::DEFAULT_THRESHOLD = 500` per mille). A size-ratio check skips most pairs before any comparison. Binary
  files (NUL in the first 8000 bytes), blobs over 1 MiB and symlinks are only matched exactly; inexact matching is
  skipped when deleted x added exceeds 2,000,000 pairs. Pairing is one-to-one by descending score; a tie for the same
  source or target is ambiguous and yields no rename.
* Merge: the base file (and the unrenamed side's file) are moved to the new path and the ordinary tree merge runs, so
  the other side's edits are text/structurally merged into the renamed file and the result lives at the new path.
  Symmetric in left/right. Both sides renaming to the same path is clean.
* Stays a conflict: both sides rename one file to different paths (`ConflictKind::RenameRename`, "rename/rename", path =
  the base path, left/right = the two renamed files; none of the three paths is in `entries`); the target of a rename
  already exists on the other side (rename not applied, old behaviour); rename on one side with content edits against a
  delete on the other (delete/modify at the new path). A rename without edits against a delete resolves to the delete.
* Not done: copy detection, directory renames (a new file added by one side into a directory the other side moved is
  not relocated), rename detection in `stash`.

Tests: unit tests in `rename.rs` (exact, similar, rename+edit, both edit, rename/rename, delete, copies), and
`crates/vcs-repo/tests/merge.rs` (`rename_*`): default still conflicts, `set_renames(true)` merges.

## Real-history replay (issue #108)

`scripts/replay.sh <bare-git-dir-or-url> <rev> [max]` runs `git fast-export --show-original-ids` (git is used only by
this script, never by the core), `vcs import-git`, then `crates/vcs-repo/examples/replay.rs`. For each merge commit
of the imported branch with exactly one merge base (sampled evenly, newest first, at most `max`, default 300) it re-runs
the merge of the two parent trees with four engines (text = line diff3; struct = structural drivers; text+ren,
struct+ren = with rename detection) and compares with the tree of the real merge commit. Counters: `clean` = merges with
no conflict; `exact_tree` = clean and the whole tree equals the real merge commit's tree; per language, `conflicts`
= conflicted paths, `content-merged` = files merged into a new blob, `exact` = such blobs identical to the real file,
`parse ok` = such blobs that parse with the language's tree-sitter grammar (only for rs/py/go/ts, features on).
The real merge is the reference, so a conflict here means "this engine would have stopped where the human did not
have to", and a clean-but-different result would show up as clean > exact_tree.

Run on the 30-core Lambda box, 2026-10-31, binary built with all four structural features, Myers. Revisions are the
default-branch heads at clone time (`git clone --bare`).

| repo (revision) | language | merge commits in history | replayed | clean: text / struct / text+ren / struct+ren | conflicted paths: text / struct / text+ren / struct+ren |
|---|---|---|---|---|---|
| https://github.com/pallets/flask (49943b5a94413a530761350f04e896f76e8d620e) | python | 1734 | 299 | 275 / 276 / 276 / 277 | 79 / 78 / 60 / 59 |
| https://github.com/psf/requests (3c7baaa05ab4aa49cc819ce3a819d260de19c157) | python | 1612 | 300 | 295 / 295 / 295 / 295 | 4 / 4 / 4 / 4 |
| https://github.com/clap-rs/clap (ccc89b83875474399e9d9d70841163f6319d1a41) | rust | 2270 | 299 | 297 / 297 / 297 / 297 | 7 / 7 / 7 / 7 |
| https://github.com/gin-gonic/gin (0f09c3a9b4626d9fe9979cf4d0521d6ae32ab646) | go | 216 | 215 | 194 / 194 / 194 / 194 | 20 / 20 / 20 / 20 |
| https://github.com/spf13/cobra (adbc8813901bba65827259daa8e22ff94ec1f30e) | go | 114 | 114 | 114 / 114 / 114 / 114 | 0 / 0 / 0 / 0 |
| https://github.com/typeorm/typeorm (5bdc67882dee5a02d975d5ed140c9c3f91661116) | typescript | 869 | 298 | 276 / 276 / 276 / 276 | 46 / 45 / 45 / 44 |
| https://github.com/nestjs/nest (8d9dd9d174921cc03882d1b39e91fb7bb1b1ca5a) | typescript | 9275 | 300 | 288 / 289 / 289 / 290 | 82 / 80 / 81 / 79 |
| https://github.com/sindresorhus/got (6b5aafc1cd538998728be6f45c65b0f80bae01b2) | javascript (no driver) | 73 | 73 | 72 / 72 / 72 / 72 | 0 / 0 / 0 / 0 |

(Conflicted paths include docs and other non-code files; per-language splits are in the script output.) A clean
result equalled the real merge's tree in every replayed merge except in two repositories: gin has 194 clean merges but 193 exact trees, and nest has one fewer exact tree than clean
merges in each engine (288/287, 289/288, 289/288, 290/289), identically across engines (merges where the human's result
differs from the mechanical merge; not investigated).
Skipped: 1 merge each in flask, clap and gin and 2 in typeorm (several merge bases).

Per-language file-level numbers (content-merged files / exact / parse ok, all of them from the same runs):

| repo, language | engine | conflicts | content-merged | exact | parse ok |
|---|---|---|---|---|---|
| flask, python | text | 41 | 47 | 46 | 47/47 |
| flask, python | struct | 40 | 48 | 46 | 48/48 |
| flask, python | text+ren | 23 | 65 | 64 | 65/65 |
| flask, python | struct+ren | 22 | 66 | 64 | 66/66 |
| requests, python | all four | 4 | 56 | 55 | 56/56 |
| clap, rust | all four | 7 | 31 | 29 | 31/31 |
| gin, go | all four | 20 | 63 | 63 | 63/63 |
| cobra, go | all four | 0 | 25 | 25 | 25/25 |
| typeorm, typescript | text / text+ren | 45 / 44 | 96 | 94 | 96/96 |
| typeorm, typescript | struct / struct+ren | 44 / 43 | 97 | 95 | 97/97 |
| nest, typescript | text / struct / text+ren / struct+ren | 14 / 12 / 13 / 11 | 15 / 17 / 15 / 17 | 13 / 14 / 13 / 14 | 15/15, 17/17, 15/15, 17/17 |

Every parse-checked output also parsed in the real history (the baseline column of the script output equals parse ok in
all rows). `exact` below `content-merged` (flask 1, requests 1, clap 2, typeorm 2, nest 2 of 15 typescript and 11 of 669 other files) means a mechanically merged file
differs from the file in the real merge commit; these are identical across engines and were not investigated.

What this says, and what it does not:

* Rename-aware merge removed 19 of 79 conflicted paths in flask (python: 41 to 23), 1 of 46 in typeorm and 1 of 82 in nest; no change
  in the other five repositories (they have few or no rename-versus-edit overlaps in the sampled merges). It never
  produced a conflict that the plain merge did not, and never made a clean merge differ from history (only the one gin merge above differs, in all engines), in the sampled merges.
* Structural merge gained 1 file in flask, 1 in typeorm and 2 in nest over text and nothing elsewhere. It is about 4x
  slower in total merge time in flask (245 ms text vs 914 ms struct for 299 merges; rename detection added 245 to 372
  ms); in nest, whose merges touch hundreds of files, 300 merges took 65.6 s text, 66.9 s struct, 68.4 s text+ren, 69.5 s
  struct+ren (the merge call only; flattening and tree reads are outside the timer).
  The structural driver's benefit on real merges is small in this sample.
* Sampling is the newest-first even stride of the merge commits, at most 300 per repository; only eight
  repositories, four languages with drivers plus one without; most real merges are trivially clean, so differences are
  concentrated in few merges. Merges with several merge bases were skipped. These numbers do not cover rebase or
  cherry-pick, conflicts that history resolved by hand differently from either side, or large monorepos.
* Default: unchanged. Rename detection stays opt-in. It was never worse in the data, but it removed a sizeable share of conflicts in only one of eight
  repositories (flask) and one or two paths in two others, so the data does not show a strictly better result across the board; turning it on by default is a
  one-line change in `Repo` (`renames: true`) if the owner decides the evidence is enough.
