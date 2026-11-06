# Rewriting history: `vcs rewrite`

`vcs rewrite` covers the core of `git filter-branch` and `git filter-repo`. It rebuilds every
commit of one zone through a pipeline of filters, then moves the branches and tags onto the new
commits. Everything is built in the object store first; the refs, index and working tree move
last, through the same machinery as `vcs op undo`. A run is **one operation**: `vcs op undo`
puts every branch, tag, the index and the files back, and the old commits stay in the store
until `vcs gc`.

```text
vcs rewrite [--subdirectory-filter DIR] [--keep PATH]... [--remove PATH]...
            [--msg-filter CMD] [--env-filter SCRIPT] [--tree-filter CMD]
            [--tag-name-filter CMD] [--all-branches] [--keep-empty]
            [--force] [--dry-run] [--zone Z]
```

Git command lines map through `vcs git filter-branch ...` / `vcs git filter-repo ...`
(see [git-to-vcs.md](git-to-vcs.md)).

## Filters, in the order they run for each commit

1. **Path filters.** `--subdirectory-filter DIR` makes `DIR` the new root (files outside it are
   gone). `--keep PATH` keeps only that file or directory (repeat for several). `--remove PATH`
   drops it. Paths are relative to the zone, because a zone's history is its own.
2. **`--tree-filter CMD`.** The tree is checked out into a scratch directory under `.vcs/`, `CMD`
   runs there with `sh -c`, and whatever is left becomes the tree. Executable bits and symlinks
   are kept. This is one checkout and one re-hash per commit, so it is slow on long histories.
   Prefer the path filters when they are enough.
3. **`--env-filter SCRIPT`.** Runs with `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`, `GIT_AUTHOR_DATE`
   (`@SECONDS`, optionally followed by a zone offset which is ignored), the `GIT_COMMITTER_*`
   equivalents, and `GIT_COMMIT` set. Change the author variables to change the author.
4. **`--msg-filter CMD`.** Old message on stdin, new message on stdout.
5. **`--tag-name-filter CMD`.** After the commits: tag name on stdin, new name on stdout. Tags
   that point at rewritten commits are moved to the new commits whether or not this flag is
   given. An annotated tag stays annotated and keeps its message and tagger.

A filter that exits non-zero, or leaves an empty author, stops the run before any ref moves.

## What is rewritten

- The checked-out branch of the zone (`--zone Z` or the zone of the current directory), or every
  branch of the zone with `--all-branches`. Other zones are never touched.
- Tags of that zone whose commit is among the rewritten commits.
- Commits that the filters make empty are dropped (the `filter-repo` default; git needs
  `--prune-empty`). A tag on a dropped commit moves to the nearest kept ancestor, or is deleted if
  there is none. `--keep-empty` keeps them. A commit that was already empty before is kept.
- Commits the filters do not change keep their id, so an unaffected prefix of history is
  identical before and after, and running the same rewrite twice changes nothing the second time.
- Every rewritten commit keeps its change id.
- Merge commits are rewritten with their parents mapped; a merge that becomes a copy of one
  parent is dropped.

## Safety

- The zone must have no uncommitted changes, no merge or rebase in progress, and no linked
  worktrees.
- **Published history is refused without `--force`.** If any commit the rewrite would change is
  reachable from a remote-tracking branch or tag (`origin`, as last fetched), `vcs rewrite` stops
  and changes nothing. Run `vcs fetch` first so that the check sees the remote's current state.
  Rewriting only the commits after the published ones needs no `--force`. After a forced rewrite
  the next `vcs publish` is a non-fast-forward and needs `vcs publish --force`; other clones keep the old commits.
- `--dry-run` runs the filters and prints the counts, and moves nothing.

## What it does not do, and why

- **One signature per commit.** A vcs commit has an author and no separate committer.
  `GIT_COMMITTER_*` are exported to the env filter with the author's values; changes to them are
  ignored, and the run says how many commits were affected.
- **No `--index-filter`, `--parent-filter`, `--commit-filter`, callbacks, `--replace-text`,
  `--mailmap`, `--path-rename`, `--path-regex`.** Use `--tree-filter` (with `sed`, `git`-free
  shell) for content changes.
- **Signed commits.** A signature is part of the message. A commit whose content, parents or
  message changes no longer verifies (`vcs verify-commit` reports it as bad) and is not re-signed.
  Annotated tag signatures are dropped for the same reason.
- **Published check is local.** It uses the remote-tracking state from the last fetch. It cannot
  know about commits somebody else published since.
- **Only the checked-out branch or all branches**, not an arbitrary list of refs.
- **Not parallel.** Commits are rebuilt one at a time; a tree filter dominates the time.

Tests: `crates/vcs-cli/tests/history_rewrite.rs`.
