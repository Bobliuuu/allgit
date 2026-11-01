# 2. Daily workflow

Add, commit, look, undo. This chapter uses a small notes repository.

## Stage and commit

`vcs add` stages files or whole directories. It also stages deletions.
`vcs commit -m "..."` records what is staged. Without `-m` it opens `core.editor` or `$EDITOR`.

```text
$ printf "line one\nline two\nline three\n" > todo.txt
$ mkdir docs && echo "# Notes" > docs/readme.md
$ vcs add todo.txt docs
staged 2 file(s), 0 removal(s)
$ vcs commit -m "add todo and docs"
[root/main ee48619edff5] add todo and docs
```

## Status and diff

`vcs status` shows staged, unstaged and untracked changes. `vcs diff` shows unstaged changes.
`vcs diff --staged` shows what the next commit will contain. `vcs diff --stat` gives the counts only.

```text
$ sed -i "s/line two/line 2 (edited)/" todo.txt
$ vcs status
zone root: branch main @ ee48619edff5
  not staged:
    modified:  todo.txt
$ vcs diff
diff --vcs a/todo.txt b/todo.txt
--- a/todo.txt
+++ b/todo.txt
@@ -1,3 +1,3 @@
 line one
-line two
+line 2 (edited)
 line three
$ vcs diff --stat
 todo.txt | +1 -1
 1 file(s) changed, 1 insertion(s), 1 deletion(s)
$ vcs add todo.txt
staged 1 file(s), 0 removal(s)
$ vcs diff
$ vcs diff --staged
diff --vcs a/todo.txt b/todo.txt
--- a/todo.txt
+++ b/todo.txt
@@ -1,3 +1,3 @@
 line one
-line two
+line 2 (edited)
 line three
$ vcs commit -m "edit line two"
[root/main d3910bf81206] edit line two
```

## Log, show, blame

`vcs log` lists commits of the checked-out branch. Useful flags: `--oneline`, `--graph`, `-n`, `--author`,
and paths after `--`. `vcs show` prints one commit with its diff. `vcs blame` names the commit behind each line.
The date in the full log is Unix seconds.

```text
$ echo "line four" >> todo.txt && vcs add todo.txt && vcs commit -m "add line four"
staged 1 file(s), 0 removal(s)
[root/main c8d04212111f] add line four
$ vcs log --oneline
c8d04212111f add line four
d3910bf81206 edit line two
ee48619edff5 add todo and docs
$ vcs log --oneline -- docs
ee48619edff5 add todo and docs
$ vcs log -n 1 --author ada
commit c8d04212111fc831d23754f73159df1e527c42416df9e2d78f11c122401ac159
change 895c7872b85028186c216f7db805b221   zone root
Author: Ada Lovelace <ada@example.com>
Date:   1793500017
    add line four

$ vcs show
commit c8d04212111fc831d23754f73159df1e527c42416df9e2d78f11c122401ac159
change 895c7872b85028186c216f7db805b221   zone root
parent d3910bf812068864d3e5a31b56e846d8834ffcbfe701324c79b8f22bc6b0e172
Author: Ada Lovelace <ada@example.com>
Date:   1793500017
    add line four

diff --vcs a/todo.txt b/todo.txt
--- a/todo.txt
+++ b/todo.txt
@@ -1,3 +1,4 @@
 line one
 line 2 (edited)
 line three
+line four
$ vcs blame todo.txt
ee48619edff5 (1) line one
d3910bf81206 (2) line 2 (edited)
ee48619edff5 (3) line three
c8d04212111f (4) line four
```

## Undo small mistakes

`vcs restore <path>` throws away working-tree changes and brings the file back from the index.
`vcs restore --staged <path>` unstages it. Neither touches commits.

```text
$ echo "oops" >> todo.txt
$ vcs status
zone root: branch main @ c8d04212111f
  not staged:
    modified:  todo.txt
$ vcs restore todo.txt
restored 1 file(s)
$ cat todo.txt
line one
line 2 (edited)
line three
line four
$ echo "staged by mistake" >> todo.txt && vcs add todo.txt
staged 1 file(s), 0 removal(s)
$ vcs restore --staged todo.txt
restored 1 index entr(ies)
$ vcs status
zone root: branch main @ c8d04212111f
  not staged:
    modified:  todo.txt
$ vcs restore todo.txt && vcs status
restored 1 file(s)
zone root: branch main @ c8d04212111f
clean
```

## Rename and remove

`vcs mv` and `vcs rm` change the working tree and the index in one step.
A rename shows up as one new file and one deleted file.

```text
$ vcs mv docs/readme.md docs/index.md
docs/readme.md -> docs/index.md
$ vcs rm todo.txt
rm todo.txt
$ vcs status
zone root: branch main @ c8d04212111f
  staged:
    new file:  docs/index.md
    deleted:   docs/readme.md
    deleted:   todo.txt
$ vcs commit -m "rename readme, drop todo"
[root/main da0f62c5e783] rename readme, drop todo
```

## Stash

`vcs stash push` sets tracked changes aside. Untracked files stay where they are.
`vcs stash pop` brings the stash back. It comes back staged.
There is also `vcs stash list` and `vcs stash drop`.

```text
$ echo "half done" > draft.txt && echo "# Notes (wip)" > docs/index.md
$ vcs stash push
stashed changes in zone(s): root
$ vcs status
zone root: branch main @ da0f62c5e783
untracked:
    draft.txt
$ vcs stash list
stash@{0}: WIP on root/main
$ vcs stash pop
applied and dropped stash entry
$ vcs status
zone root: branch main @ da0f62c5e783
  staged:
    modified:  docs/index.md
untracked:
    draft.txt
$ vcs restore --staged docs/index.md && vcs restore docs/index.md && vcs stash list
restored 1 index entr(ies)
restored 1 file(s)
```

## Tags

`vcs tag NAME` makes a lightweight tag at `HEAD`. Add `-m` to make an annotated tag.
`vcs tag -d NAME` deletes one. Any command that takes a revision accepts a tag, a branch,
a commit id prefix, and `~N` or `^`.

```text
$ vcs tag v0.1
tagged v0.1 at da0f62c5e783
$ vcs tag v0.2 -m "second release"
tagged v0.2 at da0f62c5e783
$ vcs tag
v0.1                     da0f62c5e783
v0.2                     da0f62c5e783  annotated: second release
$ vcs log --oneline
da0f62c5e783 rename readme, drop todo
c8d04212111f add line four
d3910bf81206 edit line two
ee48619edff5 add todo and docs
$ vcs show v0.1
commit da0f62c5e783a6c59ebda5b8cb5ef61f21cf137cfcddc2c7b55c4f25f383a84a
change 607bf5aca633c7141e1b477b2b11fc3f   zone root
parent c8d04212111fc831d23754f73159df1e527c42416df9e2d78f11c122401ac159
Author: Ada Lovelace <ada@example.com>
Date:   1793500018
    rename readme, drop todo

diff --vcs a/docs/index.md b/docs/index.md
new file
--- /dev/null
+++ b/docs/index.md
@@ -0,0 +1,1 @@
+# Notes
diff --vcs a/docs/readme.md b/docs/readme.md
deleted file
--- a/docs/readme.md
+++ /dev/null
@@ -1,1 +0,0 @@
-# Notes
diff --vcs a/todo.txt b/todo.txt
deleted file
--- a/todo.txt
+++ /dev/null
@@ -1,4 +0,0 @@
-line one
-line 2 (edited)
-line three
-line four
$ vcs tag -d v0.1
deleted tag v0.1 (was da0f62c5e783)
$ vcs tag
v0.2                     da0f62c5e783  annotated: second release
```

## Machine-readable output

Most read commands take `--json`. You get one JSON object on one line.
The schemas are in [../cli.md](../cli.md).

```text
$ vcs status --json
{"version":1,"command":"status","zones":[{"zone":"root","zone_id":"root","path":"","branch":"main","head":"da0f62c5e783a6c59ebda5b8cb5ef61f21cf137cfcddc2c7b55c4f25f383a84a","staged":[],"unstaged":[]}],"untracked":["draft.txt"],"unsubscribed":[],"unsupported":[],"merging":null,"rebasing":false,"clean":false}
```

Next: [chapter 3, branches, merging and undo](03-branches-merging-undo.md).
