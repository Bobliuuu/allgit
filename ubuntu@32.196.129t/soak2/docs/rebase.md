# Rebase

Status: **implemented** (`vcs-repo::rebase`, `vcs rebase`). Built on the merge engine (see merge.md) and the operation log (operations.md).

## Change ids

Commits already carry a 16-byte `change_id` (object format v1, see object-model.md), so the object format is unchanged
and the format version was not bumped. `commit` and `merge` mint a fresh change id; **rebase reuses the original one**, so
a replayed commit has a new commit id (new parent and tree) but the same change id, author and message. `vcs log`
shows `change <id>` per commit.

## Commands

```
vcs rebase <branch> [--zone A]   replay the checked-out branch onto local <branch>
vcs rebase --continue            commit the resolved pick and carry on
vcs rebase --abort               restore the original head, index and working tree
```

## Behaviour

* Needs a clean zone. Merge base = the single best common ancestor (none or several are errors, as for merge).
* Target already contained in the branch: "already up to date". Branch contained in the target: fast-forward (0 commits replayed).
* Otherwise the commits between merge base and head are replayed oldest first. Each pick is a three-way tree merge
  (base = the picked commit's parent, ours = the new tip so far, theirs = the picked commit) with the same Myers
  text merge as `vcs merge`. Only linear ranges: a merge commit in the range is refused before anything changes.
* Clean picks are computed in memory; the working tree, index and branch ref move once at the end, as one
  transaction, and **one** operation is recorded (`rebase <zone>/<branch> onto <branch>`). `vcs op undo` therefore
  restores the exact previous head, index and files (tested).
* On conflict the replay stops. The branch ref is at the last good tip, the working tree has the merged files plus
  diff3 markers, and the structured conflicts are in `.vcs/conflicts` exactly as for a stopped merge
  (`vcs conflict list|show`). The cursor (original head, onto label, stopped commit, remaining commits) is in
  `.vcs/rebase` (framed `AGRB`). Resolve with `vcs add`, then `vcs rebase --continue`; later picks may stop again.
* While stopped: `vcs commit`, `vcs merge` (including `--abort`), a second rebase and `vcs op undo` are refused with a
  pointer to `--continue`/`--abort`. A stopped or aborted rebase records no operation.

## Not done

Merge commits in the rebased range, rebasing onto a remote-tracking branch, file/directory conflicts (refused, as in
merge), rename detection, dropping picks that become empty (they are kept), interactive editing of the todo list.
A crash between writing the conflict files and moving the ref leaves stale state files that are ignored; the
index then shows the conflict placeholders as staged changes against the original head.
