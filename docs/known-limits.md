# Known limits

Things we measured, understand, and have not fixed. Each one links to the
open issue that tracks it. Numbers are from single runs on loaded machines
unless noted; see the linked research docs for method.

## Scale (issue #436)

- **First `status` on a 10M-file repository does not finish in a reasonable
  time.** The run was stopped after 30 minutes; it was IO bound, and the
  import alone took 853 s. 1M files and 100k files are measured and fine
  (see [scalability.md](scalability.md)).
- **`gc` and `fsck` memory at 1M commits is the first hard limit.** On a
  1M-commit history `gc` took 40 s and 5.8 GB, and `fsck` used 16 GB.
- **The `refs-delta` layout has no long soak.** It passed a 6 minute
  `kill -9` soak (1,357 acked files, 0 missing), but that soak reported
  `server_alive_at_end: false` and the cause was not investigated. Treat the
  layout as opt-in and not production-hardened until a 2 hour soak and that
  report are explained. The single-manifest default is unaffected.
- **Sharded manifest is opt-in and not a general speedup.** Against an
  in-memory backend with 25 ms requests it is 2.1x faster for 64 disjoint
  writers (58.9 vs 27.6 pushes/s), but through `vcs-server` on a local fsync
  disk it was measured slower than the single manifest at 32 writers (see
  [manifest-scaling.md](manifest-scaling.md) for the per-configuration
  numbers), so the single manifest stays the default. Direct-to-store clients still read every
  zone on each manifest read, and the per-branch ticket queue is not wired
  into the CLI push path.

## `status` at 100k files (issue #293)

- **No path arena.** `Entries` is a public `BTreeMap`, and the AGI5 mmap
  index already removed most of the decode cost, so the expected gain is
  small. A quiet `status` costs about 9 ms CPU flat and 7 ms deep at 100k
  files (git: 13 ms and 721 ms).
- **No quiet-machine re-measure.** Those numbers were taken at load 80 to 150.
- **Linux directory scans use `fstatat`.** `getattrlistbulk` is used on macOS
  only (22% less CPU for `status`, 43% for `diff`); `statx` has no
  per-directory form and io_uring batching was not built or timed.

## Command-level gaps vs other tools

From the final scoreboard ([research/command-scoreboard.md](research/command-scoreboard.md)):

- `add -u` (1.12x) and `commit -a` (1.29x) are still slower than git: the
  status pass and the add hash files twice.
- `init`, `init proj` and `branch -m` are about 4x git on wall time with equal
  CPU. The gap is fsyncs; removing them would trade crash safety for about
  10 ms on rare commands.
- Warm `log -- path` is about 2x git (about 30 ms vs 16 to 18 ms), bounded by
  process and store-open start-up.
- vs Perforce: one-file `submit`, `opened`, `revert`, `diff`, `delete` and
  `sync path@rev` lose, mostly because p4d is a warm server.
- `bundle verify` is a header-only check by default (like git). A flipped
  byte inside the pack body passes it; `--full`, `fetch` and `clone` hash every
  object.
- `vcs status | head` panics on a broken pipe.

## Other

- Merge assist is verified on 77 real conflicts with parse checks only, not
  the projects' own tests; the HTTP API path is mock-tested only
  ([assist.md](assist.md)).
- No resident daemon: it was built and measured, and did not beat the
  in-process path ([research/resident-daemon.md](research/resident-daemon.md)).
- Remote GC on S3 Express One Zone is not available (hybrid layout not built).
