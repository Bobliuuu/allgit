# 6. Large files, chunking, packs and gc

## Chunking

Files of 1 MiB or more are cut into chunks by content (a rolling hash, 8 KiB on average, at most 64 KiB).
Each chunk is stored once. If you change a small part of a big file, only the chunks around the edit are new.

Here a random 8 MiB file is committed, then 20 bytes in its middle are changed and it is committed again.
`du` shows the object store in KiB.

```text
$ head -c 8388608 /dev/urandom > scene.bin
$ vcs add scene.bin && vcs commit -m "add 8 MiB scene"
staged 1 file(s), 0 removal(s)
[root/main 5703e8041108] add 8 MiB scene
$ du -sk .vcs/objects | cut -f1
10012
$ printf "EDITED-IN-THE-MIDDLE" | dd of=scene.bin bs=1 seek=4000000 conv=notrunc 2>/dev/null
$ vcs status
zone root: branch main @ 5703e8041108
  not staged:
    modified:  scene.bin
$ vcs diff --stat
 scene.bin | binary
 1 file(s) changed, 0 insertion(s), 0 deletion(s)
$ vcs add scene.bin && vcs commit -m "tweak the middle of the scene"
staged 1 file(s), 0 removal(s)
[root/main 5ef4b882d8f5] tweak the middle of the scene
$ du -sk .vcs/objects | cut -f1
10072
$ vcs fsck
checked 436 objects
ok
```

The store grew by 60 KiB for the second version, not by another 8 MiB.
(The 10012 KiB includes about 430 small chunk files, each rounded up to a disk block.)
A random file has no repeats of its own, so this saving comes only from the edit being small.
Small files are stored whole:

```text
$ echo "tiny" > small.txt && vcs add small.txt && vcs commit -m "small file"
staged 1 file(s), 0 removal(s)
[root/main e3577f7e739c] small file
$ find .vcs/objects -type f | wc -l
441
```

Limits: a large file is held in memory when it is read back or checked out. There is no streaming and no
partial read yet. See [../object-model.md](../object-model.md).

## Packs and repack

Every new object starts as a loose file. That is many small files.
`vcs repack` moves them into one pack and one index under `.vcs/objects/pack`.

```text
$ vcs repack
packed 441 object(s)
$ find .vcs/objects -type f | sed "s/[0-9a-f]\{8,\}/<id>/" | sort | uniq -c
      1 .vcs/objects/pack/pack-<id>.idx
      1 .vcs/objects/pack/pack-<id>.pack
$ ls .vcs/objects/pack
pack-991819615bf3ed2ac463c85d324b7164f9144f837f8ff43143cb59ab0be8318b.idx
pack-991819615bf3ed2ac463c85d324b7164f9144f837f8ff43143cb59ab0be8318b.pack
$ du -sk .vcs/objects | cut -f1
9180
$ vcs fsck
checked 441 objects
ok
```

Repacking is safe to repeat. `vcs fsck` still passes after it.
Packs do not use deltas or compression yet, so repack saves file overhead, not content.
The sizes above went from 10072 KiB to 9180 KiB. That is block rounding, not smaller data.

## Garbage collection

`vcs gc` removes objects that no branch, tag, snapshot or operation points at.
**It is destructive.** It only deletes objects older than a grace period (14 days by default).
It refuses a grace below 3.5 days, because a short one is not safe against another writer.
Use `--dry-run` first.

In the run below a branch with new work was deleted. Nothing is collected,
because the operation log still names those commits (that is what makes `vcs op undo` work).

```text
$ vcs branch scratch && vcs switch scratch && head -c 3000000 /dev/urandom > scratch.bin && vcs add scratch.bin && vcs commit -m "scratch work" && vcs switch main
created root/scratch at e3577f7e739c
zone root: switched to scratch
staged 1 file(s), 0 removal(s)
[root/scratch 29e710fd1842] scratch work
zone root: switched to main
$ vcs branch -d scratch --force
deleted root/scratch (was 29e710fd1842)
$ vcs gc --dry-run
605 objects: 605 reachable
0 unreachable but younger than the 1209600s grace period (kept)
0 unreachable and old enough: would delete
dry run: nothing was deleted
$ vcs gc
DESTRUCTIVE: deleting unreachable local objects older than 1209600s (holding the repository lock)
deleted 0 object(s) (0 loose files, 0 dropped from packs; 0 pack(s) rewritten, 0 removed)
0 unreachable object(s) younger than the grace period were kept
$ vcs gc --grace-secs 60
DESTRUCTIVE: deleting unreachable local objects older than 60s (holding the repository lock)
error: refusing to delete with a grace period of 60s: it must exceed the touch-on-dedup threshold (302400s), otherwise a concurrent writer can re-adopt an object that this sweep then deletes (docs/gc.md)
[exit 1]
$ vcs gc --dry-run --grace-secs 0
605 objects: 605 reachable
0 unreachable but younger than the 0s grace period (kept)
0 unreachable and old enough: would delete
dry run: nothing was deleted
```

So on a young repository `gc` finds nothing. *Not run: a sweep that actually deletes objects,
because that needs objects older than 3.5 days. The sweep is covered by tests in `crates/vcs-repo/tests/gc.rs`.*

`vcs gc --remote --yes` collects a **directory remote**. It is not available for S3 or vcs-server remotes.
Read [../gc.md](../gc.md) before using it.

Next: [chapter 7, working with Git and Perforce](07-git-and-perforce.md).
