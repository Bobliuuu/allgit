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

## `vcs-server` memory under concurrent whole-pack reads

Found by the monorepo stress run ("What broke" item 5 in [research/monorepo-stress.md](research/monorepo-stress.md): RSS climbing
to 6 to 13 GiB with 64 to 256 clients). Two causes, no leak and no unbounded cache (the read memo is capped, idle RSS is flat):

1. An un-ranged `GET /v1/o/<key>` reads the whole object into memory (`Backend::get`) and holds it until the client has taken it,
   so server memory is the sum of the objects being downloaded at that moment. A full clone fetches whole packs; slice clones and
   fetches use ranged reads (`offset`/`len`) and are bounded. The request-body side is bounded (`--max-inflight-body`, default
   2 GiB). **Fixed for the directory backend**: `Backend::get_stream` returns the length and tag up front and the server sends the
   body in 256 KiB windows (`Config::stream_min_bytes`, default 8 MiB; smaller objects are still read whole). The directory
   backend's tag is the content hash, so a streamed GET costs one extra bounded-memory read pass over the file (page cache).
   **Still open for S3**: the default `get_stream` returns `None`, so an S3-backed server still loads the whole object. Streaming
   there needs a HEAD (length, ETag) plus a streamed GET body piped through; the S3 ETag is the tag, so no hash pass is needed.
2. glibc malloc retention: one arena per thread and a mmap threshold that rises to 32 MiB keep freed pack buffers resident.
   **Mitigated**: `vcs-server` now sets `M_ARENA_MAX` 4 and `M_MMAP_THRESHOLD` 1 MiB at startup (Linux/glibc; an explicit
   `MALLOC_ARENA_MAX` / `MALLOC_MMAP_THRESHOLD_` wins).

Measured in a small setup (release build, Linux arm64 in Docker, directory backend, 120 files x 256 KiB base, 24 concurrent
clients per round that each clone, add 5 files, commit and publish; the 80 MB packs are fetched whole by the clones; one run
per cell, so read the ratios, not the digits): peak RSS with the glibc defaults 2.6 and 2.3 GiB (174 to 177 threads); with
`MALLOC_ARENA_MAX=2` 0.88 GiB; with `MALLOC_ARENA_MAX=4 MALLOC_MMAP_THRESHOLD_=1048576` 1.17 GiB; with the built-in tuning (no environment) 1.10 GiB peak and 144 MB after the first round. Idle RSS after a round stayed flat
(355 MB with the defaults, 196 MB with two arenas, over 20 minutes), so a long-running server shows a staircase to the high-water
mark, not growth at idle. Fewer arenas can cost allocation throughput under many cores; that was not measured here.

Until large reads are streamed: bound simultaneous clones with `--max-connections`, give the server memory for connections x
largest pack, and prefer slice or `--auto` clones, which read ranges.

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

## Merge family

- **File/directory conflicts park the file as `path~ours` / `path~theirs`.**
  They are representable and resolvable in `merge`, `cherry-pick`, `rebase`
  and `stash pop` ([merge.md](merge.md)). Not done: a `stash pop` of such a
  conflict has no resolve state (settle it by hand, then drop the entry), and
  the parking name must be free (the command refuses before changing anything
  otherwise).
- **`cherry-pick -m N`, `A...B` in `cherry-pick` and `format-patch` work.**
  Still missing: `revert -m` (a merge commit cannot be reverted) and
  `rebase --rebase-merges`.
- **Symmetric ranges order commits by author time** (the `log` order,
  reversed), not by topology, so two commits with the same timestamp are
  picked in id order.

## Other

- Merge assist is verified on 77 real conflicts with parse checks only, not
  the projects' own tests; the HTTP API path is mock-tested only
  ([assist.md](assist.md)).
- No resident daemon: it was built and measured, and did not beat the
  in-process path ([research/resident-daemon.md](research/resident-daemon.md)).
- Remote GC on S3 Express One Zone is not available (hybrid layout not built).
