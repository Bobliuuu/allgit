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
- `vcs unlock --break` removes anyone's lock. There is no admin role check yet.
- The lock holder is `VCS_PRINCIPAL`, or your author email.
- Commits are local-first. You can still commit a locked file. The lock is checked when you publish.

## Take a lock

Ada locks the hero painting. Bob can see it. Bob edits it anyway and tries to publish.

```text
$ vcs lock art/hero.psd --lease-secs 3600 --reason "repainting the hero"
locked hero.psd (zone art) token 0
$ vcs locks
hero.psd (zone art) held by ada@example.com until 1793503951985 (unix ms) token 0  repainting the hero
$ cd ../bob
$ export VCS_AUTHOR_NAME=Bob VCS_AUTHOR_EMAIL=bob@example.com
$ vcs locks
hero.psd (zone art) held by ada@example.com until 1793503951985 (unix ms) token 0  repainting the hero
$ head -c 1000 /dev/urandom >> art/hero.psd && vcs add . && vcs commit -m "bob edits the hero anyway"
staged 2 file(s), 0 removal(s)
[art/main 6bdfdcd79f1c] bob edits the hero anyway
$ vcs publish
error: Cannot publish: hero.psd (zone art) is locked by ada@example.com until 1793503951985 (unix ms).

Your local commits are kept. Options:
  ask ada@example.com to `vcs unlock hero.psd`, or wait for the lease to expire
  drop your change to that path and publish the rest
[exit 1]
$ vcs lock art/hero.psd --lease-secs 60
error: Cannot publish: hero.psd (zone art) is locked by ada@example.com until 1793503951985 (unix ms).

Your local commits are kept. Options:
  ask ada@example.com to `vcs unlock hero.psd`, or wait for the lease to expire
  drop your change to that path and publish the rest
[exit 1]
$ vcs revert HEAD --zone art
art: revert done, new commit da8d9f1904e2
$ vcs publish
art/main d693e36c7c2e -> da8d9f1904e2
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
[art/main c0175cd445e0] new hero paint
$ vcs publish
error: Cannot publish art/main:

Remote branch advanced to da8d9f1904e2.

Your local commit: c0175cd445e0
Remote commit:     da8d9f1904e2

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

Next: [chapter 9, structural and AI-assisted merge](09-structural-and-assisted-merge.md).
