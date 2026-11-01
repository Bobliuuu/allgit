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
[root/main d01d203f1e77] add todo and docs
```

## Status and diff

`vcs status` shows staged, unstaged and untracked changes. `vcs diff` shows unstaged changes.
`vcs diff --staged` shows what the next commit will contain. `vcs diff --stat` gives the counts only.

```text
$ sed -i "s/line two/line 2 (edited)/" todo.txt
$ vcs status
zone root: branch main @ d01d203f1e77
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
[root/main 3a4f7f02c9bb] edit line two
```

## Log, show, blame

`vcs log` lists commits of the checked-out branch. Useful flags: `--oneline`, `--graph`, `-n`, `--author`,
and paths after `--`. `vcs show` prints one commit with its diff. `vcs blame` names the commit behind each line.
The date in the full log is Unix seconds.

```text
$ echo "line four" >> todo.txt && vcs add todo.txt && vcs commit -m "add line four"
staged 1 file(s), 0 removal(s)
[root/main c51a0b480c04] add line four
$ vcs log --oneline
c51a0b480c04 add line four
3a4f7f02c9bb edit line two
d01d203f1e77 add todo and docs
$ vcs log --oneline -- docs
d01d203f1e77 add todo and docs
$ vcs log -n 1 --author ada
commit c51a0b480c0427ba8c570ee739402673985d75bf423126a5b45ddeef18bfcb5f
change 0d0a2f285db3c9791eb28a3cf35e3b7e   zone root
Author: Ada Lovelace <ada@example.com>
Date:   1793502249
    add line four

$ vcs show
commit c51a0b480c0427ba8c570ee739402673985d75bf423126a5b45ddeef18bfcb5f
change 0d0a2f285db3c9791eb28a3cf35e3b7e   zone root
parent 3a4f7f02c9bbfab7608a6c52975e1ff8db14bfb75d50a201c3f478e056b30f7b
Author: Ada Lovelace <ada@example.com>
Date:   1793502249
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
d01d203f1e77 (1) line one
3a4f7f02c9bb (2) line 2 (edited)
d01d203f1e77 (3) line three
c51a0b480c04 (4) line four
```

## Undo small mistakes

`vcs restore <path>` throws away working-tree changes and brings the file back from the index.
`vcs restore --staged <path>` unstages it. Neither touches commits.

```text
$ echo "oops" >> todo.txt
$ vcs status
zone root: branch main @ c51a0b480c04
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
zone root: branch main @ c51a0b480c04
  not staged:
    modified:  todo.txt
$ vcs restore todo.txt && vcs status
restored 1 file(s)
zone root: branch main @ c51a0b480c04
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
zone root: branch main @ c51a0b480c04
  staged:
    new file:  docs/index.md
    deleted:   docs/readme.md
    deleted:   todo.txt
$ vcs commit -m "rename readme, drop todo"
[root/main 47b3d5b5e97c] rename readme, drop todo
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
zone root: branch main @ 47b3d5b5e97c
untracked:
    draft.txt
$ vcs stash list
stash@{0}: WIP on root/main
$ vcs stash pop
applied and dropped stash entry
$ vcs status
zone root: branch main @ 47b3d5b5e97c
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
tagged v0.1 at 47b3d5b5e97c
$ vcs tag v0.2 -m "second release"
tagged v0.2 at 47b3d5b5e97c
$ vcs tag
v0.1                     47b3d5b5e97c
v0.2                     47b3d5b5e97c  annotated: second release
$ vcs log --oneline
47b3d5b5e97c rename readme, drop todo
c51a0b480c04 add line four
3a4f7f02c9bb edit line two
d01d203f1e77 add todo and docs
$ vcs show v0.1
commit 47b3d5b5e97c49708dcfc5afed479830cc4ec3776dc617acbfa0885a2ae5a7ae
change 5b1bea881a6625fc5ecc275f35e40bfc   zone root
parent c51a0b480c0427ba8c570ee739402673985d75bf423126a5b45ddeef18bfcb5f
Author: Ada Lovelace <ada@example.com>
Date:   1793502250
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
deleted tag v0.1 (was 47b3d5b5e97c)
$ vcs tag
v0.2                     47b3d5b5e97c  annotated: second release
```

## Search, name and export

`vcs grep PATTERN [REV] [-- PATHS]` searches tracked files, in the working tree or in a revision.
Flags: `-n`, `-i`, `-w`, `-l`, `-c`, `-F` (literal).

```text
$ vcs grep -n hello -i
docs/a.md:1:Hello docs
$ vcs grep -c line
docs/a.md:5
$ vcs grep -l ok HEAD~2 -- src
src/main.rs
```

`vcs describe` names a commit after the nearest annotated tag (`--tags` also uses lightweight ones).
The form is `tag-N-g<id>`: N commits after the tag.

```text
$ vcs describe
v1-5-g67faedb41176
$ vcs describe --always HEAD~4
v1-1-gcfa3bceb14fa
```

`vcs archive` writes the files of a revision as `.tar` or `.zip`. The format follows the extension.

```text
$ vcs archive -o ../p.tar --prefix p-1/ && tar tf ../p.tar
p-1/bug
p-1/docs/a.md
p-1/src/main.rs
$ vcs archive -o ../p.zip HEAD~1 && ls -l ../p.zip | cut -d" " -f5
367
```

## Find the commit that broke something

`vcs bisect` does a binary search through history. Mark one bad and one good commit, then test each step by hand
(`vcs bisect good` / `bad` / `skip`) or let `vcs bisect run <command>` do it: exit 0 is good, 125 skip, 1 to 127 bad.
`vcs bisect reset` returns you to your branch.

```text
$ vcs bisect start
status: waiting for a bad commit (`vcs bisect bad [rev]`)
$ vcs bisect bad
status: waiting for a good commit (`vcs bisect good [rev]`)
$ vcs bisect good v1
Bisecting: 4 revision(s) left to test (roughly 2 step(s)); now at 306901e3db76 c4
$ vcs bisect run sh -c "test \$(cat bug) = 0"
Bisecting: 4 revision(s) left to test (roughly 2 step(s)); now at 306901e3db76 c4
  -> 306901e3db76 is bad
Bisecting: 2 revision(s) left to test (roughly 1 step(s)); now at 05408f608454 c3
  -> 05408f608454 is good
306901e3db76768c2433a696cf655f119aaa2a22531319c329595db795c32619 is the first bad commit
    c4
$ vcs bisect reset
bisect ended; back on root/main
```

## Machine-readable output

Most read commands take `--json`. You get one JSON object on one line.
The schemas are in [../cli.md](../cli.md).

```text
$ vcs status --json
{"version":1,"command":"status","zones":[{"zone":"root","zone_id":"root","path":"","branch":"main","head":"47b3d5b5e97c49708dcfc5afed479830cc4ec3776dc617acbfa0885a2ae5a7ae","staged":[],"unstaged":[]}],"untracked":["draft.txt"],"unsubscribed":[],"unsupported":[],"merging":null,"rebasing":false,"clean":false}
```

Next: [chapter 3, branches, merging and undo](03-branches-merging-undo.md).
