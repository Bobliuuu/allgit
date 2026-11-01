# 3. Branches, merging, rebasing and undo

A branch is a name for a commit. Branches are cheap. Make one for every piece of work.

## Create and switch

```text
$ vcs branch
* root/main                     14082a6ee152
$ vcs branch feature
created root/feature at 14082a6ee152
$ vcs branch
  root/feature                  14082a6ee152
* root/main                     14082a6ee152
$ vcs switch feature
zone root: switched to feature
$ echo "feature work" > feature.txt && vcs add feature.txt && vcs commit -m "add feature file"
staged 1 file(s), 0 removal(s)
[root/feature 94af6b57cadb] add feature file
$ vcs log --oneline --graph
* 94af6b57cadb add feature file
* 14082a6ee152 initial
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
root: fast-forward 14082a6ee152 -> 94af6b57cadb
$ vcs log --oneline --graph
* 94af6b57cadb add feature file
* 14082a6ee152 initial
```

## A real merge

When both sides changed different parts of a file, the merge is automatic and makes a merge commit.

```text
$ vcs branch left && vcs branch right
created root/left at 94af6b57cadb
created root/right at 94af6b57cadb
$ vcs switch left && sed -i "s/^one$/ONE (left)/" notes.txt && vcs add notes.txt && vcs commit -m "left edits top"
zone root: switched to left
staged 1 file(s), 0 removal(s)
[root/left 02ff9828837b] left edits top
$ vcs switch right && sed -i "s/^five$/FIVE (right)/" notes.txt && vcs add notes.txt && vcs commit -m "right edits bottom"
zone root: switched to right
staged 1 file(s), 0 removal(s)
[root/right 0a0b6612a2c7] right edits bottom
$ vcs switch left
zone root: switched to left
$ vcs merge right
root: merged, new commit e075df2a1bc2
$ cat notes.txt
ONE (left)
two
three
four
FIVE (right)
$ vcs log --oneline --graph
* e075df2a1bc2 Merge branch 'right' into left
| \
| * 0a0b6612a2c7 right edits bottom
* | 02ff9828837b left edits top
/ |
  * 94af6b57cadb add feature file
  * 14082a6ee152 initial
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
created root/c1 at e075df2a1bc2
created root/c2 at e075df2a1bc2
$ vcs switch c1 && sed -i "s/^version = 1/version = 2/" config.toml && vcs add config.toml && vcs commit -m "bump version to 2"
zone root: switched to c1
staged 1 file(s), 0 removal(s)
[root/c1 a0191d8c6464] bump version to 2
$ vcs switch c2 && sed -i "s/^version = 1/version = 3/" config.toml && vcs add config.toml && vcs commit -m "bump version to 3"
zone root: switched to c2
staged 1 file(s), 0 removal(s)
[root/c2 31bfefb12316] bump version to 3
$ vcs merge c1
automatic merge stopped: 1 conflict(s)
  conflict: config.toml

Resolve the files, `vcs add` each one, then `vcs commit`.
Inspect with `vcs conflict list|show <path>`; give up with `vcs merge --abort`.
error: merge has conflicts
[exit 1]
$ vcs status
zone root: branch c2 @ 31bfefb12316
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
[root/c2 f9702707769e] merge c1, keep version 3
$ vcs log --oneline -n 5
f9702707769e merge c1, keep version 3
a0191d8c6464 bump version to 2
e075df2a1bc2 Merge branch 'right' into left
31bfefb12316 bump version to 3
0a0b6612a2c7 right edits bottom
```

You can walk away at any time. `vcs merge --abort` restores the state from before the merge.
This run also shows `--theirs`.

```text
$ vcs branch d1 && vcs branch d2
created root/d1 at f9702707769e
created root/d2 at f9702707769e
$ vcs switch d1 && echo "d1" > clash.txt && vcs add clash.txt && vcs commit -m "d1 adds clash"
zone root: switched to d1
staged 1 file(s), 0 removal(s)
[root/d1 3c55af07d85d] d1 adds clash
$ vcs switch d2 && echo "d2" > clash.txt && vcs add clash.txt && vcs commit -m "d2 adds clash"
zone root: switched to d2
staged 1 file(s), 0 removal(s)
[root/d2 6b26c5d7f7e2] d2 adds clash
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
zone root: branch d2 @ 6b26c5d7f7e2
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
[root/d2 870df6ae2d7d] merge d1, take theirs
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
created root/topic at 94af6b57cadb
zone root: switched to topic
$ echo "topic" > topic.txt && vcs add topic.txt && vcs commit -m "topic work"
staged 1 file(s), 0 removal(s)
[root/topic 64db2f20d350] topic work
$ vcs switch main
zone root: switched to main
$ echo "main moved" > main-news.txt && vcs add main-news.txt && vcs commit -m "main moves on"
staged 1 file(s), 0 removal(s)
[root/main 2bd3a6ebdcfb] main moves on
$ vcs switch topic
zone root: switched to topic
$ vcs rebase main
rebased 1 commit(s): 64db2f20d350 -> 320724b6a701
$ vcs log --oneline --graph
* 320724b6a701 topic work
* 2bd3a6ebdcfb main moves on
* 94af6b57cadb add feature file
* 14082a6ee152 initial
```

## Cherry-pick and revert

`vcs cherry-pick <rev>` copies one commit onto the current branch.
`vcs revert <rev>` adds a commit that undoes one.

```text
$ vcs switch main
zone root: switched to main
$ vcs log --oneline -n 3
2bd3a6ebdcfb main moves on
94af6b57cadb add feature file
14082a6ee152 initial
$ vcs cherry-pick topic
root: cherry-pick done, new commit 7a79a2f3303e
$ vcs revert HEAD
root: revert done, new commit a73d0639bff3
$ vcs log --oneline -n 4
a73d0639bff3 Revert "topic work"
7a79a2f3303e topic work
2bd3a6ebdcfb main moves on
94af6b57cadb add feature file
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
@ f4401f4e800b  1793500020  ubuntu <ubuntu@localhost>
    revert 7a79a2f3303e onto root/main
  3ee19a161fe0  1793500020  ubuntu <ubuntu@localhost>
    cherry-pick 320724b6a701 onto root/main
  45b17a94b386  1793500020  ubuntu <ubuntu@localhost>
    checkout root/main
  69f67c3db0da  1793500020  Ada Lovelace <ada@example.com>
    rebase root/topic onto main
  a8d06687a51e  1793500020  ubuntu <ubuntu@localhost>
    checkout root/topic
  633a7ef6f322  1793500020  Ada Lovelace <ada@example.com>
    commit: main moves on
$ vcs branch -d topic
error: branch root/topic (320724b6a701) has commits not reachable from any other branch or snapshot; merge it, or pass --force to delete anyway (the commit stays in the operation log)
[exit 1]
$ vcs branch -d topic --force
deleted root/topic (was 320724b6a701)
$ vcs branch
  root/c1                       a0191d8c6464
  root/c2                       f9702707769e
  root/d1                       3c55af07d85d
  root/d2                       870df6ae2d7d
  root/feature                  94af6b57cadb
  root/left                     e075df2a1bc2
* root/main                     a73d0639bff3
  root/right                    0a0b6612a2c7
$ vcs op undo
undone; recorded operation b7d26709cc7a
$ vcs branch
  root/c1                       a0191d8c6464
  root/c2                       f9702707769e
  root/d1                       3c55af07d85d
  root/d2                       870df6ae2d7d
  root/feature                  94af6b57cadb
  root/left                     e075df2a1bc2
* root/main                     a73d0639bff3
  root/right                    0a0b6612a2c7
  root/topic                    320724b6a701
$ vcs op log -n 3
@ b7d26709cc7a  1793500020  ubuntu <ubuntu@localhost>
    undo operation 94c485dd4ac9
  94c485dd4ac9  1793500020  ubuntu <ubuntu@localhost>
    delete branch root/topic
  f4401f4e800b  1793500020  ubuntu <ubuntu@localhost>
    revert 7a79a2f3303e onto root/main
```

The full log from that session:

```text
$ vcs op log -n 20
@ b7d26709cc7a  1793500020  ubuntu <ubuntu@localhost>
    undo operation 94c485dd4ac9
  94c485dd4ac9  1793500020  ubuntu <ubuntu@localhost>
    delete branch root/topic
  f4401f4e800b  1793500020  ubuntu <ubuntu@localhost>
    revert 7a79a2f3303e onto root/main
  3ee19a161fe0  1793500020  ubuntu <ubuntu@localhost>
    cherry-pick 320724b6a701 onto root/main
  45b17a94b386  1793500020  ubuntu <ubuntu@localhost>
    checkout root/main
  69f67c3db0da  1793500020  Ada Lovelace <ada@example.com>
    rebase root/topic onto main
  a8d06687a51e  1793500020  ubuntu <ubuntu@localhost>
    checkout root/topic
  633a7ef6f322  1793500020  Ada Lovelace <ada@example.com>
    commit: main moves on
  6225cf99ca04  1793500020  ubuntu <ubuntu@localhost>
    checkout root/main
  cf983f43e00b  1793500020  Ada Lovelace <ada@example.com>
    commit: topic work
  5e59a17d4e23  1793500020  ubuntu <ubuntu@localhost>
    checkout root/topic
  38a8ba539b1d  1793500020  ubuntu <ubuntu@localhost>
    create branch root/topic
  ddcac2cfe0ac  1793500020  ubuntu <ubuntu@localhost>
    checkout root/main
  3fb726f6e0df  1793500020  Ada Lovelace <ada@example.com>
    commit: merge d1, take theirs
  c81fb3bd65a9  1793500020  ubuntu <ubuntu@localhost>
    merge --abort root/d2
  7da4a327f009  1793500020  Ada Lovelace <ada@example.com>
    commit: d2 adds clash
  7eea93855a50  1793500019  ubuntu <ubuntu@localhost>
    checkout root/d2
  4e6312e5db12  1793500019  Ada Lovelace <ada@example.com>
    commit: d1 adds clash
  8aea4c8194c6  1793500019  ubuntu <ubuntu@localhost>
    checkout root/d1
  5f0622c37692  1793500019  ubuntu <ubuntu@localhost>
    create branch root/d2
```

Rules:

- Only local branches, subscriptions and the working tree move. The remote is never touched.
- It refuses if a zone it would change has uncommitted work. Nothing is modified in that case.
- No object is ever deleted. Details: [../operations.md](../operations.md).

Operations made by `branch`, `switch` and similar commands show your OS user name as author.
Commits show the identity you configured.

Next: [chapter 4, zones](04-zones.md).
