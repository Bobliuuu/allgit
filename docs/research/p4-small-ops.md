# Perforce-mapped small ops on a 100k-file repo: where the time goes

Setup: `datagen flat 100000`, one commit, watcher running (auto-started), macOS arm64 dev laptop under load
average 20-40 (other jobs), so absolute numbers are noisy and a bare process start costs about 11 ms here
(`vcs p4 info`). The recorded p4 numbers (3.4-4.9 ms wall, command-scoreboard.md) come from a Linux box and
cannot be compared in absolute terms from this host; the goal was to remove every cost that scales with the
index. Min / median of 30 runs, alternating base (origin/main) and new binaries, ms:

| op (vcs spelling) | before min / med | after min / med |
|---|---|---|
| add new1.txt (`p4 add`) | 35.8 / 37.3 | 23.5 / 24.6 |
| restore FILE (`p4 revert`) | 58.9 / 65.4 | 44.2 / 57.0 |
| rm FILE (`p4 delete`) | 52.7 / 66.7 | 44.1 / 52.6 |
| status (`p4 opened`) | 31.9 / 40.3 | 33.2 / 34.8 |
| status, clean | 27.5 / 37.0 | 25.5 / 32.0 |

## Findings (phase timers around `add` of one new root file, before the fix)

* `find_new_path_collision` read and decoded every entry of the index (all 100k) for a new file at the root:
  about 13 ms. It now steps over whole directories (seek to the key after the last descendant), decoding about
  one entry per directory.
* Every index patch (`add`, `restore`, `rm`, `restore --staged`) paid, on the 7 MB index: a blake3 of the old
  index (about 5 ms), a blake3 of the new one after the write (about 5 ms), a `memmem` over all of it for the
  attribute-file marker (macOS libc memmem is slow: 18 ms), then the write. Now the old digest comes from a
  per-file-identity cache (`.vcs/fsmon/ixdigest`), the new digest is computed on a thread while the file is
  written and is remembered, and the attribute marker looks only at the patched paths.
* `status` with a watcher: `mine` (a 100k-element reference list) was built even when the monitor had answered;
  now built only for a real scan. The fingerprint hash is served from the digest cache.

Not fixed: the index is still rewritten whole on each mutation (about 25-40 ms of write on this loaded disk) and
`status` still decodes the whole index (about 14 ms). Both need an index layout that can be patched in place or
a lazy status over `IndexFile`; that is the next step.
