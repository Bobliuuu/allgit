# Snapshot checkout, area rename/delete, branch delete

Status: **implemented** (`vcs-repo::manage`). All four commands take the repository lock, validate before the first
write, move worktree and index before `refs`/`workspace`, never delete an object, and record one entry in the
[operation log](operations.md). A dirty area (staged or unstaged changes, or a merge in progress) always makes them refuse.

## `vcs snapshot checkout <name>`

Restores every **subscribed** area whose checked-out branch is not already at the pinned commit.

* Branches are not moved backwards. Each moving area gets a branch `snapshot/<name>` at the pinned commit and that
  branch is checked out, so the previous branch keeps all its commits (there is no detached HEAD in the workspace model).
* Refuses, changing nothing, when: a moving area is dirty or mid-merge; a pinned area no longer exists; a pinned commit
  is missing locally; `snapshot/<name>` already exists at a different commit; or a file would overwrite untracked work.
* Pinned areas that are not subscribed are listed as skipped, not materialized. Areas the snapshot does not pin are untouched.
* `vcs op undo` returns to the previous branches and files.

## `vcs area rename <old> <new>`

Only for an area **without history**: no local or remote-tracking branches, not pinned by a snapshot, not mid-merge.
Commits embed their area name and are immutable, so renaming an area with commits would need history rewriting.
The path does not change, so no workspace file moves. The root area cannot be renamed.

## `vcs area delete <name> [--force]`

* Always refuses when the area is dirty (also with `--force`) and for the root area.
* If subscribed, its tracked (committed) files are removed from the worktree; untracked files stay and become
  untracked files of the root area. Nothing uncommitted is discarded.
* An area with branches or snapshot pins needs `--force`, which drops its branch refs. Commits stay in the object
  store and in the operation log; a snapshot that pinned the area can no longer be checked out (it reports the
  missing area). `vcs op undo` cannot restore a deleted area (the operation view refuses: "area no longer exists").
* Remote-tracking refs and the remote's area definition are not touched.

## `vcs branch -d <name> [--area A] [--force]`

Refuses the checked-out branch of the area, a missing branch, and (without `--force`) a branch whose head is not
reachable from another branch of the same area or from a snapshot, since its commits would lose their last name.
`vcs op undo` restores a deleted branch. (The command is a flag on `vcs branch`, not a `branch delete` subcommand,
because `vcs branch <name>` already creates a branch.)

## Limits

* Like `op restore`, snapshot checkout is not atomic across several areas if a later area's preflight fails after an
  earlier area was applied; the result shows as ordinary changes, with no data loss.
* Snapshot checkout does not fetch missing commits.
