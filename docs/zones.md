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

Works for any zone, with or without history. Chosen design: an **alias layer**, not history rewriting (see
[object-model.md](object-model.md#zone-names-ids-and-display-names)).

* The name embedded in a zone's commits is its permanent **id**. A rename stores only a **display name** in the
  `zones` file (`AGA2` encoding; a zone that was never renamed keeps the old `AGAR` bytes).
* Branches, snapshots, workspace, index, operation views and the remote manifest stay keyed by id, so no commit id
  changes and nothing else is rewritten. `vcs log`, branches, `snapshot checkout`, `fsck` keep working.
* Every command accepts the display name or the id. Output shows the display name. Both stay reserved: a new zone
  or another rename cannot take either.
* Refuses: the root zone, an unknown zone, a name already in use, a dirty zone, a zone mid-merge.
* Operation log: a rename is recorded (`zone rename old new`), but names are not part of an operation view. So
  `vcs op undo` immediately after a rename is **refused** with a message (rename back instead), and
  `op undo`/`op restore` of other operations works across a rename and keeps the new name.
* Published repositories: `publish` records `id -> display name` in the remote manifest (v2; v1 bytes unchanged when
  no zone is renamed) together with that publish's transaction, so it appears with the next publish that touches the
  zone (a rename alone does not publish). `fetch`/`sync` and `clone` adopt a remote name unless the local zone was
  renamed already or the name is taken; a local rename wins until it is published. Last publisher wins.

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
