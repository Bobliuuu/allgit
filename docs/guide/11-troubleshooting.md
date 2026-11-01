# 11. Troubleshooting

## Check the repository: fsck

`vcs fsck` reads every object, checks its hash, and checks the links between commits, trees, zones and the operation log.
It changes nothing. A clean run ends with `ok`. Problems are listed and the exit code is 1.

Here one object file is damaged by hand. In a repository with a remote the damaged copy can simply be dropped
and fetched again:

```text
$ vcs fsck
checked 7 objects
ok
$ f=$(ls -S $(find .vcs/objects -type f) | tail -1); echo "$f" | sed "s/[0-9a-f]\{62\}$/<rest>/"; echo garbage >> "$f"
.vcs/objects/c3/<rest>
$ vcs fsck
checked 7 objects
PROBLEM: c34d947fc4de51f01072acfa2887791420c8d15c5dd5e67fbe05cda6454280f8: object c34d947fc4de51f01072acfa2887791420c8d15c5dd5e67fbe05cda6454280f8 is corrupt: hash mismatch (content hashes to 3b5654cf30bff327d6916aca01afbb2608fa54853381d838511d88ac75f0e148)
error: 1 problem(s) found
[exit 1]
$ rm "$f"; vcs fsck
checked 6 objects
1 object(s) not cached locally; they are on the remote and will be refetched on demand
ok
$ vcs fetch; vcs fsck
fetched root/main @ 2c03b463c6c5
checked 6 objects
1 object(s) not cached locally; they are on the remote and will be refetched on demand
ok
```

Note that after `rm`, `fsck` says the object is "not cached locally" and still passes. The remote holds a copy,
and `vcs` fetches it when something needs it. *The object here was not needed again in this run,
so the refetch itself was not shown.*

If you have no remote, a damaged object cannot be rebuilt. Restore it from a clone or a backup.
Operation-log objects are local only. A damaged one cannot come back from a remote. `fsck` reports it, and your branches and files are not affected.

## A damaged pack

`fsck` also checks pack checksums:

```text
$ cd .. && vcs clone remote healthy >/dev/null && cd healthy
$ vcs repack && vcs fsck
packed 6 object(s)
checked 6 objects
ok
$ p=$(ls .vcs/objects/pack/*.pack); head -c 100 /dev/urandom | dd of=$p bs=1 seek=40 conv=notrunc 2>/dev/null; vcs fsck
checked 6 objects
PROBLEM: 2c03b463c6c59f426b75b5048cb3711a29f6fcc711fbc20396228db0ffcb4c52: object 2c03b463c6c59f426b75b5048cb3711a29f6fcc711fbc20396228db0ffcb4c52 is corrupt: hash mismatch (content hashes to 26e5b286e20e7120210ec7f5eae7fa33a37f1f494b8bcec9e8341b1cbc344439)
PROBLEM: 52c0aeedfb9f472cf7e17f8e0fe69eb448b3b00d10240ab4920a02095f40c86b: object 52c0aeedfb9f472cf7e17f8e0fe69eb448b3b00d10240ab4920a02095f40c86b is corrupt: hash mismatch (content hashes to 433ff7eb32bccaf2415d138a432b941b7246f96a90b5b04b88e5d4c782fef153)
PROBLEM: ref root/main: points to missing/non-commit 2c03b463c6c59f426b75b5048cb3711a29f6fcc711fbc20396228db0ffcb4c52
PROBLEM: operation 923289641068b6761ef3aaaa326e45b0c6318a560637580529de45afa9e01445: view root/main -> 2c03b463c6c59f426b75b5048cb3711a29f6fcc711fbc20396228db0ffcb4c52 is missing or not a commit of that zone
PROBLEM: /home/ubuntu/guide-run/ch11/healthy/.vcs/objects/pack/pack-0ff3f29940658684989b597c9cdeb14a605a61c324d4e53fdb44250334feb71e.pack: pack checksum mismatch
PROBLEM: /home/ubuntu/guide-run/ch11/healthy/.vcs/objects/pack/pack-0ff3f29940658684989b597c9cdeb14a605a61c324d4e53fdb44250334feb71e.pack: object 2c03b463c6c59f426b75b5048cb3711a29f6fcc711fbc20396228db0ffcb4c52 hash mismatch
PROBLEM: /home/ubuntu/guide-run/ch11/healthy/.vcs/objects/pack/pack-0ff3f29940658684989b597c9cdeb14a605a61c324d4e53fdb44250334feb71e.pack: bad framing for 52c0aeedfb9f472cf7e17f8e0fe69eb448b3b00d10240ab4920a02095f40c86b
error: 7 problem(s) found
[exit 1]
```

Fix: delete the pack and its `.idx` from `.vcs/objects/pack`, then `vcs fetch` (with a remote) or restore from a clone.
*That fix was not run here.*

## A remote that lost data: publish --repair

If a remote lost objects, other people see errors like `object ... missing on remote`.
If you still have the objects locally, `vcs publish --repair` walks everything reachable from your branches,
checks each object on the remote, and uploads the missing ones. A plain `publish` says "nothing to publish".

```text
$ cd .. && vcs init p2 && cd p2 && echo data > f.txt && vcs add . && vcs commit -m first && mkdir ../remote2 && vcs remote set ../remote2 && vcs publish
Initialized empty vcs repository in /home/ubuntu/guide-run/ch11/p2/.vcs
staged 1 file(s), 0 removal(s)
[root/main cfdec50d89d9] first
remote set to /home/ubuntu/guide-run/ch11/remote2
root/main (none) -> cfdec50d89d9
published atomically (remote generation 1)
$ ls ../remote2/packs | sed "s/[0-9a-f]\{16,\}/<id>/"
pack-<id>.idx
pack-<id>.pack
$ rm ../remote2/packs/*
$ vcs clone ../remote2 ../c2
error: remote data invalid: object cfdec50d89d9d76bdf37219900e1848d4ce9d68c75916686583f58b4f1a1f168 missing on remote
[exit 1]
$ vcs publish
nothing to publish
$ vcs publish --repair
repaired 3 object(s) on the remote
nothing to publish
$ vcs clone ../remote2 ../c3 && ls ../c3
Cloned /home/ubuntu/guide-run/ch11/remote2 into ../c3
downloaded zones: root
skipped zones (not subscribed): (none)
f.txt
```

If you do not have the objects either, `--repair` cannot help. It says so and names the object.

## Common errors

```text
$ cd /tmp && vcs status
error: not inside a vcs repository (no .vcs directory found)
[exit 1]
$ cd ~/guide-run/ch11/proj
$ vcs frobnicate
error: unrecognized subcommand 'frobnicate'

  tip: a similar subcommand exists: 'conflict'

Usage: vcs [OPTIONS] <COMMAND>

For more information, try '--help'.
[exit 2]
$ vcs switch nope
error: no such branch root/nope
[exit 1]
$ echo dirty >> a.txt; vcs branch other; vcs switch other
created root/other at 2c03b463c6c5
error: zone "root" has uncommitted changes (0 staged, 1 unstaged); commit or revert them first
[exit 1]
$ vcs commit -m "nothing staged"
error: nothing to commit (no staged changes)
[exit 1]
$ vcs merge nonexistent
error: no such branch root/nonexistent
[exit 1]
$ vcs show deadbeef
error: unknown revision "deadbeef" in zone "root" (expected HEAD, a tag, a branch or a commit id prefix)
[exit 1]
$ vcs add missing.txt
error: pathspec "missing.txt" did not match any files
[exit 1]
```

What they mean:

| Message | Fix |
| --- | --- |
| `not inside a vcs repository` | `cd` into the repository, or use `vcs -C <dir>` |
| `user.name and user.email are not set` | [chapter 1](01-install-and-first-repo.md#tell-vcs-who-you-are) |
| `zone "root" has uncommitted changes` | commit, `vcs stash push`, or `vcs restore` first |
| `nothing to commit (no staged changes)` | run `vcs add` |
| `merge has conflicts` | [chapter 3](03-branches-merging-undo.md#conflicts) |
| `Cannot publish ... Remote branch advanced` | `vcs fetch`, merge `--remote`, publish again |
| `not logged in to http://...` | `vcs login`, or set `VCS_TOKEN` |
| `authentication failed (401)` | the token is wrong, expired or revoked: `vcs login` again |
| `permission denied (403)` | your token's role or zone scope is too small. Ask the admin |
| `missing S3 credentials` | export `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` |
| `Cannot publish: ... is locked by ...` | [chapter 8](08-locks.md) |

## When something went wrong and you want it back

Run `vcs op log`. Then `vcs op undo` or `vcs op restore <id>`
([chapter 3](03-branches-merging-undo.md#undo-with-the-operation-log)). Nothing is deleted by it.

## Reporting a bug

Include `vcs --version`, the commands you ran, the full output, and the output of `vcs fsck`.
Use `--json` output only if you are scripting. Issues go to <https://github.com/Bobliuuu/allgit/issues>.
