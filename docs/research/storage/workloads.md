# Storage benchmark workloads (issue #182, epic #190)

Generators live in `crates/vcs-bench/src/workloads.rs`. Every dataset is a pure function of
`(workload, scale, seed)`: SplitMix64 only, no clock, no OS randomness, no external data. Unit tests assert that two
generations are byte-identical and that edits are reproducible. `storebench list` prints the live parameters;
`storebench gen <workload> <dir> --scale S --seed N` materialises one (plus `..storebench-manifest.json`).

Scale: `smoke` = 1, `small` = 8, `medium` = 64, or any integer. Scale multiplies **file counts only**; file sizes, zone
counts, history length and branch counts are fixed per workload (except where noted). Default seed 42.

Terms: the *history zone* is the zone (or whole repo, if there are no zones) that history edits and per-trial edits touch.
Edit kinds: `lines(n)` replaces n lines; `region(len,count)` overwrites `count` regions of `len` bytes in place;
`rewrite` regenerates the whole file; `perturb(1/d)` rewrites the low mantissa byte of every d-th float.
All numbers below are at scale 1. File sizes in parentheses are drawn uniformly per file from the planned range.

| # | Workload | Files, content | Zones | History | Purpose / what it stresses |
|---|---|---|---|---|---|
| 1 | `small_source` | 1000 source-like text files (200 B to 4 KiB), 3-level dirs | none | 10 revs x 10 files, lines(3) | per-object overhead, loose vs packed small objects |
| 2 | `medium_app` | 200 source files (1 to 16 KiB) + 40 texture-like binaries (16 to 128 KiB) | `src`, `assets` | 10 x 4, region(512 B x2), zone `src` | mixed text/binary, two zones |
| 3 | `large_monorepo` | 8 services x 500 small files (0.2 to 2 KiB), depth 4 | `svc0`..`svc7` | 10 x 20, lines(3), zone `svc0` | tree sizes, many zones, sparse subscription |
| 4 | `binary_heavy` | 24 incompressible 256 KiB files | none | 5 x 6, region(4 KiB x4) | chunk-level dedup of partial edits |
| 5 | `game_assets` | 16 textures (0.5 to 2 MiB, gradient+noise), 8 audio-like random files (0.25 to 1 MiB), 50 code files | `assets`,`audio`,`code` | 5 x 2, rewrite, zone `assets` | large media, whole-asset replacement |
| 6 | `ml_checkpoints` | 3 x 4 MiB + one 8 MiB float32 tensors (random mantissa, exponent in a narrow range) | `ckpt` | 5 x 3, perturb(1/20), zone `ckpt` | large near-incompressible files where each step changes small scattered bytes (hard for CDC dedup) |
| 7 | `many_similar` | 2000 x 1 KiB files, one shared template with 4 x 16 varying bytes | none | 5 x 20, region(16 B x2) | near-duplicate objects (delta/similarity), very small files |
| 8 | `highly_compressible` | 16 x 256 KiB of a 64-byte pattern with zero runs | none | 5 x 4, region(4 KiB x2) | compression ratio upper bound, zstd/zlib CPU vs size |
| 9 | `incompressible` | 16 x 256 KiB random | none | 5 x 2, rewrite | compression must not cost time; expected ratio 1.0 |
| 10 | `repeated_across_zones` | 4 zones x (200 shared + 20 unique) 2 KiB text files; the 200 shared files are byte-identical across zones | `z0`..`z3` | 10 x 8, lines(3), zone `z0` | cross-zone dedup (content-addressed identity ignores path) |
| 11 | `frequent_small_edits` | 300 source files (1 to 4 KiB) | none | 100 revs x 3 files, lines(1) | many tiny commits, small deltas, history-heavy log/blame |
| 12 | `frequent_large_rewrites` | 20 x 256 KiB text | none | 20 revs x 5 files, full rewrite | write amplification, garbage |
| 13 | `deep_history` | 200 source files (0.5 to 3 KiB) | none | 300 revs x 2 files, lines(2) | commit-graph walk, historical lookup, blame depth (scale only widens the tree, not the history) |
| 14 | `many_branches` | 300 source files | none | 3 revs + 50 branches, each one single-file commit | ref count, branch switch, fetch of many refs |
| 15 | `concurrent_publishing` | 4 zones x 200 source files | `p0`..`p3` | 5 x 10, lines(3), zone `p0` | the `publish_concurrent` op: 4 simultaneous publishers, one zone each |
| 16 | `zone_subscription` | 100 zones x 20 source files | `z000`..`z099` | 3 x 10, zone `z000` | sparse clone of 1/5/10/50 percent of zones (`sparse_clone_{1,5,10,50}pct`) |

## Notes and honest limits

* Content families are *models*, not real data: "source" is synthetic lines from a 32-word vocabulary, so compression
  ratios are optimistic compared with real code; "texture" is a gradient plus 3-bit noise; "tensor" is
  random-mantissa float32. Treat ratios as properties of these generators, not of real repositories. [HYPOTHESIS that
  the ranking of designs transfers to real data; real-repo corpora are out of scope here.]
* Sparse subscription picks the first `round(Z * pct / 100)` zones; the git side uses `--filter=blob:none --sparse` plus
  `sparse-checkout set <zone dirs>` (cone mode, which also includes top-level files).
* Every fixture history is replayed with fixed commit timestamps (`VCS_TIMESTAMP`, `GIT_*_DATE`) so commit ids are
  reproducible for a given tool version.
* Workloads with no zones (`zones=0`) skip the sparse clone and concurrent publish operations ("skipped" entries in the
  result file name the reason).
