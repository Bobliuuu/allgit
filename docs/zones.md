# Snapshot checkout, zone rename/delete, branch delete

Status: **implemented** (`vcs-repo::manage`). All four commands take the repository lock, validate before the first
write, move worktree and index before `refs`/`workspace`, never delete an object, and record one entry in the
[operation log](operations.md). A dirty zone (staged or unstaged changes, or a merge in progress) always makes them refuse.

## `vcs snapshot checkout <name>`

Restores every **subscribed** zone whose checked-out branch is not already at the pinned commit.

* Branches are not moved backwards. Each moving zone gets a branch `snapshot/<name>` at the pinned commit and that
  branch is checked out, so the previous branch keeps all its commits (there is no detached HEAD in the workspace model).
* Refuses, changing nothing, when: a moving zone is dirty or mid-merge; a pinned zone no longer exists; a pinned commit
  is missing locally; `snapshot/<name>` already exists at a different commit; or a file would overwrite untracked work.
* Pinned zones that are not subscribed are listed as skipped, not materialized. Zones the snapshot does not pin are untouched.
* `vcs op undo` returns to the previous branches and files.

## `vcs zone rename <old> <new>`

Only for a zone **without history**: no local or remote-tracking branches, not pinned by a snapshot, not mid-merge.
Commits embed their zone name and are immutable, so renaming a zone with commits would need history rewriting.
The path does not change, so no workspace file moves. The root zone cannot be renamed.

## `vcs zone delete <name> [--force]`

* Always refuses when the zone is dirty (also with `--force`) and for the root zone.
* If subscribed, its tracked (committed) files are removed from the worktree; untracked files stay and become
  untracked files of the root zone. Nothing uncommitted is discarded.
* A zone with branches or snapshot pins needs `--force`, which drops its branch refs. Commits stay in the object
  store and in the operation log; a snapshot that pinned the zone can no longer be checked out (it reports the
  missing zone). `vcs op undo` cannot restore a deleted zone (the operation view refuses: "zone no longer exists").
* Remote-tracking refs and the remote's zone definition are not touched.

## `vcs branch -d <name> [--zone A] [--force]`

Refuses the checked-out branch of the zone, a missing branch, and (without `--force`) a branch whose head is not
reachable from another branch of the same zone or from a snapshot, since its commits would lose their last name.
`vcs op undo` restores a deleted branch. (The command is a flag on `vcs branch`, not a `branch delete` subcommand,
because `vcs branch <name>` already creates a branch.)

## Limits

* Like `op restore`, snapshot checkout is not atomic across several zones if a later zone's preflight fails after an
  earlier zone was applied; the result shows as ordinary changes, with no data loss.
* Snapshot checkout does not fetch missing commits.
