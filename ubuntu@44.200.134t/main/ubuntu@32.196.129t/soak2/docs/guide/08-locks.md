# 8. Locks for binary assets

Binary files cannot be merged. If two people edit `hero.psd`, one of them loses work.
A lock says "I am editing this. Do not publish changes to it."

Locks live on the remote. They need a remote that can be written by `publish`, so `vcs lock` fails without one.

**Read this first.** Today the check runs in the *client* that publishes. Anyone who can write the remote's manifest
can bypass it. Locks are therefore **advisory** until a validating server is the only writer.
They stop honest mistakes. They do not stop a hostile client. The design is in [../locks.md](../locks.md).

Other limits you should know:

- A lease length is required: `--lease-secs N`. There is no default.
- Locks cover one file in one zone, on every branch. The lock path is relative to the zone.
- `vcs unlock --break` removes anyone's lock. A directory remote has no roles, so anyone can; `vcs-server` accepts it only
  from an admin token.
- The lock holder is `VCS_PRINCIPAL`, or your author email.
- Commits are local-first. You can still commit a locked file. The lock is checked when you publish.

## Take a lock

Ada locks the hero painting. Bob can see it. Bob edits it anyway and tries to publish.

```text
$ vcs lock art/hero.psd --lease-secs 3600 --reason "repainting the hero"
locked hero.psd (zone art) token 0
$ vcs locks
hero.psd (zone art) held by ada@example.com until 1793505882210 (unix ms) token 0  repainting the hero
$ cd ../bob
$ export VCS_AUTHOR_NAME=Bob VCS_AUTHOR_EMAIL=bob@example.com
$ vcs locks
hero.psd (zone art) held by ada@example.com until 1793505882210 (unix ms) token 0  repainting the hero
$ head -c 1000 /dev/urandom >> art/hero.psd && vcs add . && vcs commit -m "bob edits the hero anyway"
staged 2 file(s), 0 removal(s)
[art/main 359367cdf011] bob edits the hero anyway
$ vcs publish
error: Cannot publish: hero.psd (zone art) is locked by ada@example.com until 1793505882210 (unix ms).

Your local commits are kept. Options:
  ask ada@example.com to `vcs unlock hero.psd`, or wait for the lease to expire
  drop your change to that path and publish the rest
[exit 1]
$ vcs lock art/hero.psd --lease-secs 60
error: Cannot publish: hero.psd (zone art) is locked by ada@example.com until 1793505882210 (unix ms).

Your local commits are kept. Options:
  ask ada@example.com to `vcs unlock hero.psd`, or wait for the lease to expire
  drop your change to that path and publish the rest
[exit 1]
$ vcs revert HEAD --zone art
art: revert done, new commit 1fb0a520b2ca
$ vcs publish
art/main dd1c186756d5 -> 1fb0a520b2ca
published atomically (remote generation 3)
```

Bob's local commit is kept. His options are to ask Ada, wait for the lease to end, or drop his change to that file
and publish the rest. Reverting his commit made the net change empty, so the publish went through.
Only the net change of what you publish is checked.

(Bob's `vcs lock` attempt failed with the publish message. That wording is misleading. The lock was refused because Ada holds it.)

## Release a lock

Ada publishes her work and releases the lock. Her publish presents her lock token automatically.

```text
$ unset VCS_AUTHOR_NAME VCS_AUTHOR_EMAIL; cd ../ada
$ head -c 1000 /dev/urandom >> art/hero.psd && vcs add . && vcs commit -m "new hero paint"
staged 2 file(s), 0 removal(s)
[art/main b14e63a06407] new hero paint
$ vcs publish
error: Cannot publish art/main:

Remote branch advanced to 1fb0a520b2ca.

Your local commit: b14e63a06407
Remote commit:     1fb0a520b2ca

Options:
  vcs sync
[exit 1]
$ vcs unlock art/hero.psd
unlocked 1 path(s)
$ vcs locks
no locks
```

## Break a lock

```text
$ vcs lock art/hero.psd --lease-secs 3600
locked hero.psd (zone art) token 1
$ vcs unlock art/hero.psd --break
unlocked 1 path(s)
$ vcs locks
no locks
```

`--break` works for anyone today. In the run above the same person broke her own lock.
`vcs locks --json` is not supported yet.

## Lockable files: read-only until you lock

You do not have to remember which files need a lock. List them in a tracked `.vcsattributes` file (one per directory;
repo root or a zone root is typical). It uses the same patterns as `.vcsignore`:

```text
# .vcsattributes
*.psd lockable
assets/** lockable merge=binary
*.fbx lockable merge=binary
*.dat merge=binary
```

What `lockable` does:

- Matching files are checked out **read-only** (mode bits cleared; the read-only attribute on Windows, which is
  implemented but not tested). `clone`, `switch`, `sync`, `merge`, `restore`, `commit` and `worktree add` re-apply it.
- `vcs lock <path> --lease-secs N` makes the file writable, in every worktree of the repository.
  `vcs unlock <path>` makes it read-only again.
- `vcs add` and `vcs commit` refuse a change to a tracked lockable file when you do not hold its lock. They say which
  files and how to take the lock. `--force-no-lock` overrides this locally and prints a warning; it is off by default.
  A brand-new file is exempt from this client check (there is nothing to lose yet).
- `vcs status` lists lockable files that are modified without a lock.
- `vcs locks` shows holder, age, time to expiry, and marks expired locks as stale. Anyone can take a stale lock;
  an admin can also `vcs unlock --break` it.
- With `vcs config set lock.releaseOnPublish on`, `vcs publish` releases your locks on files that have no local
  changes left, which makes them read-only again.

Your held locks are recorded in `.vcs/lockable-held`, so the checks work offline. Before it lets you add or commit, the
client asks the remote whether the lock is still yours. If the remote cannot be reached, `lock.offline` decides:
`warn` (default) trusts the local record and prints a warning, `refuse` stops. If your lease ended or an admin broke the
lock, the message says so and the file is read-only again.

The server still has the last word. With a `require-lock` rule in the server policy, a publish that changes a matching
path without a live lock is refused with a 428, even if the client used `--force-no-lock`. New files under such a rule
need a lock too: `vcs lock` the path before the first publish.

### Merging binaries: `merge=binary`

With `merge=binary` a merge, rebase, cherry-pick or revert never text-merges that path, even when both sides look like
text and would merge cleanly. If both sides changed it differently you get a conflict, your side stays in the
working tree untouched (no markers), and you choose: `vcs resolve <path> --ours` or `--theirs`, then `vcs add` and
`vcs commit`.

Limits: removing a `lockable` line does not make already read-only files writable again (run `chmod u+w`). The
attribute files are read from the files in your working tree that are tracked (plus a root file on disk).

Next: [chapter 9, structural and AI-assisted merge](09-structural-and-assisted-merge.md).
