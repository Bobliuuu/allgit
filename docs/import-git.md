# Importing a Git repository

Status: **implemented** (`vcs import-git`, `Repo::import_git`). The core never runs `git` or links libgit2; it reads a
`git fast-export` stream.

```bash
git fast-export --all --show-original-ids > repo.stream      # run wherever git exists
vcs init mirror && cd mirror
vcs import-git ../repo.stream                                 # or: ... | vcs import-git -
```

## Mapping

* `refs/heads/<name>` becomes branch `<name>` of the **root** zone; tags, remotes and notes are listed as ignored.
* History, file contents and the executable bit are preserved; merges keep both parents. Commit `change_id`s are derived
  from the Git key, so they are stable across re-imports.
* Table `.vcs/git-import` (`<our object id> <git key>` per line) maps Git commit ids (`original-oid`, emitted by
  `--show-original-ids`) to our commit ids. Without `original-oid` the key falls back to `mark:<n>`, which is only
  stable if the exporter keeps marks stable between runs.
* Re-importing the same stream is a no-op: known commits are skipped and everything else is content-addressed, so no new
  objects are written. A longer stream imports only the new commits.

## Behaviour and limits

* Symlinks (120000), submodules (160000) and paths vcs cannot represent (reserved names, non-UTF-8) are skipped and
  reported. Empty directories are not represented.
* Fresh repository: the checked-out branch is materialised in the working tree. A checked-out branch that already has
  history is never moved (check out another branch first); a branch whose local history is not an ancestor of the
  imported head is refused.
* Blob data is read into memory one blob at a time; the per-commit file map is held in memory (whole-tree, like the
  rest of the MVP). Not benchmarked on large repositories.
* Git SHA-1 object ids of blobs/trees are not recorded (a fast-export stream does not contain them for trees).
