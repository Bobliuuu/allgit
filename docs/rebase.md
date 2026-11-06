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

* Needs a clean zone. The replayed range is every commit of the branch that the target (or `--onto` upstream) lacks;
  unrelated histories are an error.
* Target already contained in the branch: "already up to date". Branch contained in the target: fast-forward (0 commits replayed).
* Otherwise the commits between merge base and head are replayed oldest first. Each pick is a three-way tree merge
  (base = the picked commit's parent, ours = the new tip so far, theirs = the picked commit) with the same Myers
  text merge as `vcs merge`. A merge commit in the range is refused before anything changes, unless `--rebase-merges` is given.
* Clean picks are computed in memory; the working tree, index and branch ref move once at the end, as one
  transaction, and **one** operation is recorded (`rebase <zone>/<branch> onto <branch>`). `vcs op undo` therefore
  restores the exact previous head, index and files (tested).
* On conflict the replay stops. The branch ref is at the last good tip, the working tree has the merged files plus
  diff3 markers, and the structured conflicts are in `.vcs/conflicts` exactly as for a stopped merge
  (`vcs conflict list|show`). The cursor (original head, onto label, stopped commit, remaining commits) is in
  `.vcs/rebase` (framed `AGRB`). Resolve with `vcs add`, then `vcs rebase --continue`; later picks may stop again.
* While stopped: `vcs commit`, `vcs merge` (including `--abort`), a second rebase and `vcs op undo` are refused with a
  pointer to `--continue`/`--abort`. A stopped or aborted rebase records no operation.

## `--rebase-merges[=rebase-cousins|no-rebase-cousins]`

`vcs rebase --rebase-merges <branch>` (also `-r`, and with `--onto NEW UPSTREAM` / `--onto NEW --root`) recreates the merge
commits of the range as git's `rebase --rebase-merges` does:

* The range is replayed in topological order (parents first). A persisted map original -> replacement drives it: a plain
  commit is a three-way pick on the replacement of its parent; a merge commit is **recreated** by merging the
  replacements of its parents (`git merge -C`: message, author and change id of the original, a freshly merged tree
  with the natural merge base, which is a virtual base for a criss-cross history). Parents outside the range stay as
  they were, except the merge bases, which become the new base.
* Default `no-rebase-cousins`: commits that do not descend from the merge base keep their original parents (they are
  not replayed). `rebase-cousins` replays them onto the new base too.
* Conflicts, in a pick or in a recreated merge, stop exactly like a linear rebase: the branch is at the replacement of
  the (first) parent, the working tree has the markers, `--continue` commits the merge with both parents, `--abort`
  restores the original head, `--skip` replaces the stopped commit or merge by its first parent. The map is part of
  the `.vcs/rebase` file (framed `AGR3`), and the commits in it are GC roots.
* One operation is recorded when the rebase finishes; `vcs op undo` restores the previous head.
* Not supported: combining it with `-i`/`-x` (there are no `label`/`reset`/`merge` todo lines); an octopus merge whose
  recreation conflicts (clean octopus merges work); a root commit of
  unrelated history inside the range.

## Not done

Rebasing onto a remote-tracking branch with `--onto`, rename detection, dropping picks that become empty (they are kept), `label`/`reset`/`merge` lines in the interactive todo list.
A crash between writing the conflict files and moving the ref leaves stale state files that are ignored; the
index then shows the conflict placeholders as staged changes against the original head.
