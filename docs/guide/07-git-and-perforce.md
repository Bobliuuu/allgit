# 7. Working with Git and Perforce

`vcs` never runs `git` and never links libgit2. It talks to Git through Git's own stream formats.
This chapter used a real `git` 2.34 on the box to produce and read the streams.

Three tools:

| Tool | Direction | Notes |
| --- | --- | --- |
| `vcs import-git` | Git to vcs | reads a `git fast-export` stream into the root zone |
| `vcs export-git` | vcs to Git | writes a `git fast-import` stream for one zone branch |
| `git-remote-allgit` | both, live | lets `git clone/push/pull` talk to a vcs repository |

## Import a Git repository

Make the stream with Git, then import it. Pass `--show-original-ids` so the original SHAs are recorded.

```text
$ cd oldproject && git log --oneline && cd ..
a64cae8 second
32ee4d3 first
$ git -C oldproject fast-export --all --show-original-ids > old.stream
$ vcs init imported && cd imported
Initialized empty vcs repository in /home/ubuntu/guide-run/ch07/imported/.vcs
$ vcs import-git ../old.stream
imported 2 commit(s), 0 already present; 3 blob(s); 5 new object(s)
  branch main -> 0c3ea688807b
0 symlink entr(ies), 1 tag(s) imported
$ vcs log --oneline
0c3ea688807b second
103c4dffecbf first
$ ls; cat app.txt
app.txt
lib
v2
$ head -3 .vcs/git-import | cut -c1-60
103c4dffecbfae32b2917a49be9203053fb87e714cae265a07cbd1a6e39e
5f4910ef193314d61f35c75f5301719d5850fadd23e05aa164df1036df7e
c45a23cdca2810453aafa3fbe3e9ac6668535f6df9b956cf4b98a96d9580
$ vcs fsck
checked 11 objects
ok
```

The table `.vcs/git-import` maps vcs commits to Git commits. Importing again skips what is already there.
Commit ids differ from Git's, because they are hashed another way. Author, time, message, file contents,
paths and the executable bit survive. Committer identity and time zones do not. Tags are imported.

## Export to Git

Export writes a stream. Feed it to `git fast-import`. Paths are relative to the zone, so one zone becomes
one standalone Git repository.

```text
$ echo "v3" > app.txt && vcs add . && vcs commit -m "third, made in allgit"
staged 2 file(s), 0 removal(s)
[root/main da67dd155839] third, made in allgit
$ vcs export-git main -o ../out.stream
exported 3 commit(s) (0 already exported), 4 blob(s), 1 tag(s) to refs/heads/main
$ head -12 ../out.stream
blob
mark :1
data 3
v1

commit refs/heads/main
mark :2
author Gitty <gitty@example.com> 1793500349 +0000
committer Gitty <gitty@example.com> 1793500349 +0000
data 55
first

$ cd .. && git init -q -b main roundtrip && cd roundtrip && git fast-import --quiet < ../out.stream && git log --oneline && git show HEAD:app.txt
7964b5b third, made in allgit
7b23447 second
f7ed064 first
v3
$ cd ../imported
```

The second export only writes new commits. It remembers what it exported in `.vcs/git-export`.
Give the same Git repository `--export-marks` and `--import-marks-if-exists` on the Git side to keep old SHAs stable.
`--full` exports everything again.

```text
$ echo "v4" > app.txt && vcs add . && vcs commit -m "fourth"
staged 2 file(s), 0 removal(s)
[root/main b4e6b31eac39] fourth
$ vcs export-git main -o ../out2.stream
exported 1 commit(s) (1 already exported), 1 blob(s), 1 tag(s) to refs/heads/main
$ grep -c "^commit " ../out2.stream
1
```

Every exported commit gets a `Vcs-Change-Id` trailer so the change id survives a round trip.
More: [../interop.md](../interop.md) and [../import-git.md](../import-git.md).

## Use Git on a vcs repository: git-remote-allgit

`cargo build --release` also builds `git-remote-allgit`. Put it on your `PATH`.
Git then understands URLs that start with `allgit::`.

```text
$ cd .. && git clone -q allgit::$PWD/imported git-view && cd git-view && git log --oneline
1595118 fourth
7964b5b third, made in allgit
7b23447 second
f7ed064 first
$ echo "from git" > git-note.txt && git add . && git commit -q -m "commit made with git" && git push origin HEAD:refs/heads/main
To allgit::/home/ubuntu/guide-run/ch07/imported
   1595118..c3e1785  HEAD -> main
$ vcs -C ../imported log --oneline -n 3
3123125b5edc commit made with git
b4e6b31eac39 fourth
da67dd155839 third, made in allgit
$ git remote -v
origin	allgit::/home/ubuntu/guide-run/ch07/imported (fetch)
origin	allgit::/home/ubuntu/guide-run/ch07/imported (push)
```

What works (tested against a real Git): clone, fetch, pull, fast-forward pushes, tags, executable bits, symlinks.
What does not: only the **root zone** is visible to Git. Only local paths work, not HTTP or S3 remotes.
Force-pushes that rewrite history are rejected. Large files are held in memory and there is no chunking on this path.

## Perforce

`vcs import-p4` reads **recorded** Perforce output from a directory. The importer never runs `p4`
and never contacts a server.

**Status: fixture-tested only.** It was written without a Perforce server. It has never seen real `p4` output.
The run below uses the hand-written fixtures in `crates/vcs-repo/tests/fixtures/p4`. Treat any real use as an experiment
and check the result. *Not run: any real Perforce server or real `p4` output.*

You record the output wherever Perforce is, then import it. The layout and the view file format are in
[../import-p4.md](../import-p4.md). The view maps depot paths to paths in the root zone.

```text
$ cd ~/guide-run/ch07 && cp -r ~/rt/guide/crates/vcs-repo/tests/fixtures/p4 p4fx && ls p4fx p4fx/basic p4fx/basic/describe && cat p4fx/basic.view
p4fx:
basic
basic.view

p4fx/basic:
changes.txt
describe
print

p4fx/basic/describe:
100.marshal
101.txt
102.txt
103.marshal
104.txt
# depot -> zone path; later rules win
//depot/proj/...          app/...
-//depot/proj/secret/...
//depot/tools/build.sh    scripts/build.sh
$ vcs init p4mirror && cd p4mirror && vcs import-p4 ../p4fx/basic --view ../p4fx/basic.view
Initialized empty vcs repository in /home/ubuntu/guide-run/ch07/p4mirror/.vcs
imported 4 changelist(s), 0 already present; 13 new object(s)
not imported (no file in view): change 104
$ vcs log --oneline
0973c19c58e6 Integrate from release branch
21fe5d71b0bc Move util into lib
f08d5629b83e Fix main, add util
1e6170526361 Initial import of proj
$ ls -R . | head -12
.:
app
scripts

./app:
NEW.txt
README.txt
bin
src

./app/bin:
run.sh
```

Each submitted changelist becomes one commit. A changelist with no file in the view is reported and skipped
(change 104 above). Importing again only adds new changelists.

Next: [chapter 8, locks](08-locks.md).
