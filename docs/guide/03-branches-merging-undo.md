# 3. Branches, merging, rebasing and undo

A branch is a name for a commit. Branches are cheap. Make one for every piece of work.

## Create and switch

```text
$ vcs branch
* root/main                     5ba830665858
$ vcs branch feature
created root/feature at 5ba830665858
$ vcs branch
  root/feature                  5ba830665858
* root/main                     5ba830665858
$ vcs switch feature
zone root: switched to feature
$ echo "feature work" > feature.txt && vcs add feature.txt && vcs commit -m "add feature file"
staged 1 file(s), 0 removal(s)
[root/feature efab2d3bd584] add feature file
$ vcs log --oneline --graph
* efab2d3bd584 add feature file
* 5ba830665858 initial
$ vcs switch main
zone root: switched to main
$ ls
config.toml
notes.txt
```

## Fast-forward merge

If the branch you merge is straight ahead of yours, `vcs merge` just moves your branch.
No new commit is made.

```text
$ vcs merge feature
root: fast-forward 5ba830665858 -> efab2d3bd584
$ vcs log --oneline --graph
* efab2d3bd584 add feature file
* 5ba830665858 initial
```

## A real merge

When both sides changed different parts of a file, the merge is automatic and makes a merge commit.

```text
$ vcs branch left && vcs branch right
created root/left at efab2d3bd584
created root/right at efab2d3bd584
$ vcs switch left && sed -i "s/^one$/ONE (left)/" notes.txt && vcs add notes.txt && vcs commit -m "left edits top"
zone root: switched to left
staged 1 file(s), 0 removal(s)
[root/left e6df137ded99] left edits top
$ vcs switch right && sed -i "s/^five$/FIVE (right)/" notes.txt && vcs add notes.txt && vcs commit -m "right edits bottom"
zone root: switched to right
staged 1 file(s), 0 removal(s)
[root/right e8849cec28bf] right edits bottom
$ vcs switch left
zone root: switched to left
$ vcs merge right
root: merged, new commit 16d3bc055303
$ cat notes.txt
ONE (left)
two
three
four
FIVE (right)
$ vcs log --oneline --graph
* 16d3bc055303 Merge branch 'right' into left
| \
| * e8849cec28bf right edits bottom
* | e6df137ded99 left edits top
/ |
  * efab2d3bd584 add feature file
  * 5ba830665858 initial
```

## Conflicts

When both sides changed the same lines, the merge stops. Nothing is lost.
The file gets conflict markers in diff3 style (ours, base, theirs).
The steps are:

1. `vcs conflict list` and `vcs conflict show <path>` to see what is wrong.
2. Fix the file by hand, then `vcs resolve <path> --mark`. Or pick a side with
   `vcs resolve <path> --ours`, `--theirs` or `--base`. Or take a file with `--file <FILE>`.
3. `vcs resolve --check` fails while conflicts or marker lines remain.
4. `vcs commit`.

In a merge, "ours" is the branch you are on. In a rebase, "ours" is the branch you are rebasing onto.

```text
$ vcs branch c1 && vcs branch c2
created root/c1 at 16d3bc055303
created root/c2 at 16d3bc055303
$ vcs switch c1 && sed -i "s/^version = 1/version = 2/" config.toml && vcs add config.toml && vcs commit -m "bump version to 2"
zone root: switched to c1
staged 1 file(s), 0 removal(s)
[root/c1 815c34e23c67] bump version to 2
$ vcs switch c2 && sed -i "s/^version = 1/version = 3/" config.toml && vcs add config.toml && vcs commit -m "bump version to 3"
zone root: switched to c2
staged 1 file(s), 0 removal(s)
[root/c2 09b3d3d3b728] bump version to 3
$ vcs merge c1
automatic merge stopped: 1 conflict(s)
  conflict: config.toml

Resolve the files, `vcs add` each one, then `vcs commit`.
Inspect with `vcs conflict list|show <path>`; give up with `vcs merge --abort`.
error: merge has conflicts
[exit 1]
$ vcs status
zone root: branch c2 @ 09b3d3d3b728
  staged:
    modified:  config.toml
merging c1 into root/c2: 1 unresolved conflict(s)
    unresolved config.toml (modify/modify)
  (fix, `vcs add`, `vcs commit`; or `vcs merge --abort`)
$ vcs conflict list
unresolved modify/modify  config.toml
$ vcs conflict show config.toml
config.toml (modify/modify, unresolved)
  base:   ddd9e20aebed
  ours:   051c40db01bb
  theirs: 884d14b8636f [c1]

title = "app"
<<<<<<< ours (root/c2)
version = 3
||||||| base
version = 1
=======
version = 2
>>>>>>> c1
$ cat config.toml
title = "app"
<<<<<<< ours (root/c2)
version = 3
||||||| base
version = 1
=======
version = 2
>>>>>>> c1
$ vcs resolve --check
unresolved: config.toml
error: 1 unresolved, 0 with conflict markers
[exit 1]
$ printf "title = \"app\"\nversion = 3\n" > config.toml
$ vcs resolve config.toml --mark
resolved config.toml
0 conflict(s) left
$ vcs resolve --check
all conflicts resolved, no conflict markers left
$ vcs commit -m "merge c1, keep version 3"
[root/c2 e3eafd819808] merge c1, keep version 3
$ vcs log --oneline -n 5
e3eafd819808 merge c1, keep version 3
815c34e23c67 bump version to 2
09b3d3d3b728 bump version to 3
16d3bc055303 Merge branch 'right' into left
e8849cec28bf right edits bottom
```

You can walk away at any time. `vcs merge --abort` restores the state from before the merge.
This run also shows `--theirs`.

```text
$ vcs branch d1 && vcs branch d2
created root/d1 at e3eafd819808
created root/d2 at e3eafd819808
$ vcs switch d1 && echo "d1" > clash.txt && vcs add clash.txt && vcs commit -m "d1 adds clash"
zone root: switched to d1
staged 1 file(s), 0 removal(s)
[root/d1 7c363afb6ba0] d1 adds clash
$ vcs switch d2 && echo "d2" > clash.txt && vcs add clash.txt && vcs commit -m "d2 adds clash"
zone root: switched to d2
staged 1 file(s), 0 removal(s)
[root/d2 d3d50e078f17] d2 adds clash
$ vcs merge d1
automatic merge stopped: 1 conflict(s)
  conflict: clash.txt

Resolve the files, `vcs add` each one, then `vcs commit`.
Inspect with `vcs conflict list|show <path>`; give up with `vcs merge --abort`.
error: merge has conflicts
[exit 1]
$ vcs merge --abort
merge aborted; working tree restored
$ vcs status
zone root: branch d2 @ d3d50e078f17
clean
$ vcs merge d1
automatic merge stopped: 1 conflict(s)
  conflict: clash.txt

Resolve the files, `vcs add` each one, then `vcs commit`.
Inspect with `vcs conflict list|show <path>`; give up with `vcs merge --abort`.
error: merge has conflicts
[exit 1]
$ vcs resolve clash.txt --theirs
resolved clash.txt
0 conflict(s) left
$ cat clash.txt
d1
$ vcs commit -m "merge d1, take theirs"
[root/d2 1174b05fdc28] merge d1, take theirs
```

Merge flags worth knowing:

- `--renames` follows renamed files and merges edits into the new name. It is off by default
  (set `merge.renames` to turn it on).
- `--remote` merges the remote-tracking branch as last fetched.
- Structural and assisted merge are in [chapter 9](09-structural-and-assisted-merge.md).

## Rebase

`vcs rebase <onto>` replays your branch on top of another one. Each commit keeps its change id,
so tools can tell it is the same change. A conflict stops the rebase. Fix it with `vcs resolve`,
then run `vcs rebase --continue`, or give up with `vcs rebase --abort`.

```text
$ vcs switch main
zone root: switched to main
$ vcs branch topic && vcs switch topic
created root/topic at efab2d3bd584
zone root: switched to topic
$ echo "topic" > topic.txt && vcs add topic.txt && vcs commit -m "topic work"
staged 1 file(s), 0 removal(s)
[root/topic b025dfabd655] topic work
$ vcs switch main
zone root: switched to main
$ echo "main moved" > main-news.txt && vcs add main-news.txt && vcs commit -m "main moves on"
staged 1 file(s), 0 removal(s)
[root/main 6b139d0f957e] main moves on
$ vcs switch topic
zone root: switched to topic
$ vcs rebase main
rebased 1 commit(s): b025dfabd655 -> 5f333be98122
$ vcs log --oneline --graph
* 5f333be98122 topic work
* 6b139d0f957e main moves on
* efab2d3bd584 add feature file
* 5ba830665858 initial
```

## Cherry-pick and revert

`vcs cherry-pick <rev>` copies one commit onto the current branch.
`vcs revert <rev>` adds a commit that undoes one.

```text
$ vcs switch main
zone root: switched to main
$ vcs log --oneline -n 3
6b139d0f957e main moves on
efab2d3bd584 add feature file
5ba830665858 initial
$ vcs cherry-pick topic
root: cherry-pick done, new commit 0924038dcd95
$ vcs revert HEAD
root: revert done, new commit a667220d4d4c
$ vcs log --oneline -n 4
a667220d4d4c Revert "topic work"
0924038dcd95 topic work
6b139d0f957e main moves on
efab2d3bd584 add feature file
```

## Undo with the operation log

Every command that changes the repository is written to the operation log.
`vcs op log` lists them, newest first. The `@` marks the current one.
`vcs op undo` goes back to the view before the current operation. That includes branch heads,
which zones you subscribe to, and the files in your working tree.
It is recorded as a new operation, so you can undo the undo.
Use `vcs op restore <id>` to jump further back.

Here `branch -d` refuses to delete a branch that nothing else points at.
With `--force` it deletes it, and `op undo` brings it back.

```text
$ vcs op log -n 6
@ 2971b029b33b  1793502256  ubuntu <ubuntu@localhost>
    revert 0924038dcd95 onto root/main
  6a74371b569e  1793502256  ubuntu <ubuntu@localhost>
    cherry-pick 5f333be98122 onto root/main
  88b15d34d36f  1793502256  ubuntu <ubuntu@localhost>
    checkout root/main
  22184de366c2  1793502256  Ada Lovelace <ada@example.com>
    rebase root/topic onto main
  d8174e0f0a8a  1793502256  ubuntu <ubuntu@localhost>
    checkout root/topic
  204989acf91b  1793502256  Ada Lovelace <ada@example.com>
    commit: main moves on
$ vcs branch -d topic
error: branch root/topic (5f333be98122) has commits not reachable from any other branch or snapshot; merge it, or pass --force to delete anyway (the commit stays in the operation log)
[exit 1]
$ vcs branch -d topic --force
deleted root/topic (was 5f333be98122)
$ vcs branch
  root/c1                       815c34e23c67
  root/c2                       e3eafd819808
  root/d1                       7c363afb6ba0
  root/d2                       1174b05fdc28
  root/feature                  efab2d3bd584
  root/left                     16d3bc055303
* root/main                     a667220d4d4c
  root/right                    e8849cec28bf
$ vcs op undo
undone; recorded operation 2f8469c2af4f
$ vcs branch
  root/c1                       815c34e23c67
  root/c2                       e3eafd819808
  root/d1                       7c363afb6ba0
  root/d2                       1174b05fdc28
  root/feature                  efab2d3bd584
  root/left                     16d3bc055303
* root/main                     a667220d4d4c
  root/right                    e8849cec28bf
  root/topic                    5f333be98122
$ vcs op log -n 3
@ 2f8469c2af4f  1793502257  ubuntu <ubuntu@localhost>
    undo operation 7c07763f58e7
  7c07763f58e7  1793502257  ubuntu <ubuntu@localhost>
    delete branch root/topic
  2971b029b33b  1793502256  ubuntu <ubuntu@localhost>
    revert 0924038dcd95 onto root/main
```

The full log from that session:

```text
$ vcs op log -n 20
@ 2f8469c2af4f  1793502257  ubuntu <ubuntu@localhost>
    undo operation 7c07763f58e7
  7c07763f58e7  1793502257  ubuntu <ubuntu@localhost>
    delete branch root/topic
  2971b029b33b  1793502256  ubuntu <ubuntu@localhost>
    revert 0924038dcd95 onto root/main
  6a74371b569e  1793502256  ubuntu <ubuntu@localhost>
    cherry-pick 5f333be98122 onto root/main
  88b15d34d36f  1793502256  ubuntu <ubuntu@localhost>
    checkout root/main
  22184de366c2  1793502256  Ada Lovelace <ada@example.com>
    rebase root/topic onto main
  d8174e0f0a8a  1793502256  ubuntu <ubuntu@localhost>
    checkout root/topic
  204989acf91b  1793502256  Ada Lovelace <ada@example.com>
    commit: main moves on
  48b0b373002e  1793502256  ubuntu <ubuntu@localhost>
    checkout root/main
  4cbd2da46b99  1793502256  Ada Lovelace <ada@example.com>
    commit: topic work
  7042360670a7  1793502256  ubuntu <ubuntu@localhost>
    checkout root/topic
  a802b46d8fd4  1793502256  ubuntu <ubuntu@localhost>
    create branch root/topic
  8edf05cc22e0  1793502256  ubuntu <ubuntu@localhost>
    checkout root/main
  9224b476e62f  1793502256  Ada Lovelace <ada@example.com>
    commit: merge d1, take theirs
  001a0454cf7d  1793502256  ubuntu <ubuntu@localhost>
    merge --abort root/d2
  d9a8a810bec6  1793502255  Ada Lovelace <ada@example.com>
    commit: d2 adds clash
  df2791ab2765  1793502255  ubuntu <ubuntu@localhost>
    checkout root/d2
  4ed3e0e7dbf7  1793502255  Ada Lovelace <ada@example.com>
    commit: d1 adds clash
  f8833fabd5fc  1793502255  ubuntu <ubuntu@localhost>
    checkout root/d1
  1e877c54010d  1793502255  ubuntu <ubuntu@localhost>
    create branch root/d2
```

Rules:

- Only local branches, subscriptions and the working tree move. The remote is never touched.
- It refuses if a zone it would change has uncommitted work. Nothing is modified in that case.
- No object is ever deleted. Details: [../operations.md](../operations.md).

Operations made by `branch`, `switch` and similar commands show your OS user name as author.
Commits show the identity you configured.

Next: [chapter 4, zones](04-zones.md).
