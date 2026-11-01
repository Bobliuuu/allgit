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
[root/main 727d880b946a] add 8 MiB scene
$ du -sk .vcs/objects | cut -f1
10040
$ printf "EDITED-IN-THE-MIDDLE" | dd of=scene.bin bs=1 seek=4000000 conv=notrunc 2>/dev/null
$ vcs status
zone root: branch main @ 727d880b946a
  not staged:
    modified:  scene.bin
$ vcs diff --stat
 scene.bin | binary
 1 file(s) changed, 0 insertion(s), 0 deletion(s)
$ vcs add scene.bin && vcs commit -m "tweak the middle of the scene"
staged 1 file(s), 0 removal(s)
[root/main 76af65942293] tweak the middle of the scene
$ du -sk .vcs/objects | cut -f1
10108
$ vcs fsck
checked 447 objects
ok
```

The store grew by 44 KiB for the second version, not by another 8 MiB.
(The 10076 KiB includes about 450 small chunk files, each rounded up to a disk block.)
A random file has no repeats of its own, so this saving comes only from the edit being small.
Small files are stored whole:

```text
$ echo "tiny" > small.txt && vcs add small.txt && vcs commit -m "small file"
staged 1 file(s), 0 removal(s)
[root/main 837c3def452c] small file
$ find .vcs/objects -type f | wc -l
452
```

Limits: a large file is held in memory when it is read back or checked out. There is no streaming and no
partial read yet. See [../object-model.md](../object-model.md).

## Packs and repack

Every new object starts as a loose file. That is many small files.
`vcs repack` moves them into one pack and one index under `.vcs/objects/pack`.

```text
$ vcs repack
packed 452 object(s)
$ find .vcs/objects -type f | sed "s/[0-9a-f]\{8,\}/<id>/" | sort | uniq -c
      1 .vcs/objects/pack/pack-<id>.idx
      1 .vcs/objects/pack/pack-<id>.pack
$ ls .vcs/objects/pack
pack-1feb46c78f41b48582859b6aa00efc95926eb370861b943ef3a023343290faff.idx
pack-1feb46c78f41b48582859b6aa00efc95926eb370861b943ef3a023343290faff.pack
$ du -sk .vcs/objects | cut -f1
9188
$ vcs fsck
checked 452 objects
ok
```

Repacking is safe to repeat. `vcs fsck` still passes after it.
Packs do not use deltas or compression yet, so repack saves file overhead, not content.
The sizes above went from 10120 KiB to 9208 KiB. That is block rounding, not smaller data.

## Garbage collection

`vcs gc` removes objects that no branch, tag, snapshot or operation points at.
**It is destructive.** It only deletes objects older than a grace period (14 days by default).
It refuses a grace below 3.5 days, because a short one is not safe against another writer.
Use `--dry-run` first.

In the run below a branch with new work was deleted. Nothing is collected,
because the operation log still names those commits (that is what makes `vcs op undo` work).

```text
$ vcs branch scratch && vcs switch scratch && head -c 3000000 /dev/urandom > scratch.bin && vcs add scratch.bin && vcs commit -m "scratch work" && vcs switch main
created root/scratch at 837c3def452c
zone root: switched to scratch
staged 1 file(s), 0 removal(s)
[root/scratch f7ca8548d5c6] scratch work
zone root: switched to main
$ vcs branch -d scratch --force
deleted root/scratch (was f7ca8548d5c6)
$ vcs gc --dry-run
608 objects: 608 reachable
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
608 objects: 608 reachable
0 unreachable but younger than the 0s grace period (kept)
0 unreachable and old enough: would delete
dry run: nothing was deleted
```

So on a young repository `gc` finds nothing. *Not run: a sweep that actually deletes objects,
because that needs objects older than 3.5 days. The sweep is covered by tests in `crates/vcs-repo/tests/gc.rs`.*

`vcs gc --remote --yes` collects a **directory remote**. It is not available for S3 or vcs-server remotes.
Read [../gc.md](../gc.md) before using it.

Next: [chapter 7, working with Git and Perforce](07-git-and-perforce.md).
