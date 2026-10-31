# Operation log

Status: **implemented** (`vcs-objects` Kind 6, `vcs-repo::ops`, `vcs op …`). `vcs rebase` records one operation (see rebase.md).

A Jujutsu-style log of *what you did to the repository*, separate from the commit DAG. It makes local mistakes recoverable.

## Object

`Operation` (Kind byte 6, canonical codec, strict decode, proptested):
`parents[]` (previous op, normally one), `view`, `description`, `author`.
`view` = sorted map of **local** branch heads `(zone, branch) -> commit` + workspace subscriptions `zone -> branch`.
Remote-tracking refs are not part of the view.

## Recording and crash safety

Every mutating `Repo` operation (`commit`, `create_branch`, `checkout`, `create_zone`, `zone_add`, `zone_remove`,
`snapshot_create`, a `sync` that fast-forwards, `init`) finishes its normal write, then:
1. stores the `Operation` object, 2. atomically replaces `.vcs/op_head` (framed, tmp+fsync+rename).

A crash between 1 and 2 leaves the previous `op_head` valid and the new object an unreferenced orphan (tested).
A crash between the state write and step 1 leaves a change that is simply not in the log yet; the next operation's
view includes it. `fsck` walks the chain from `op_head` (objects present, decodable, views point to commits of the right zone).

## Commands

```
vcs op log               newest first; @ marks the current operation
vcs op show <id-prefix>  description, author, parents, full view
vcs op undo              restore the view of the current op's parent; recorded as a new op
vcs op restore <id>      restore the view as it was after <id>; recorded as a new op
```
`undo` is "go to the parent op's view", so repeating it toggles (undoes the undo). Use `restore` to jump further back
or to redo. Snapshots are named refs and are not part of the view: undoing `snapshot create` changes nothing.

## Safety rules (all tested)

* Only local branch refs, workspace subscriptions, index and worktree move. `remote-refs` and the remote are never touched.
* Refused if a zone whose materialized state would change has staged or unstaged changes; nothing is modified.
* The whole target view is validated first; a commit missing locally is an error and nothing changes.
* No object is ever deleted. Worktree/index are brought to the target by the same `transition` preflight as `checkout`
  (refuses to overwrite untracked files, never writes through symlinks).
* Not atomic across several affected zones: if a later zone's preflight fails after an earlier one was applied, the
  worktree may be ahead of `refs`/`workspace` (shown as ordinary changes, no data loss; re-run or restore).
