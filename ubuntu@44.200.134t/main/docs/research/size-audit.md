# Size audit: where vcs bytes go, and which cuts are free

Owner rule: size must never cost speed; speed wins every tie. Result in one paragraph: the 100k client store is
larger than git's because per-object v2 entries compress worse than git's delta-packed objects, and every
cold-recompression option that exists today (pack v3 blocks, pack v4 deltas) costs checkout CPU or wall time on at
least one measured cell. **No size cut was shipped.** This PR adds the measurement tools and the evidence.

## Environment

Box c (c6i.4xlarge, 16 vCPU, Linux 7.0.0-1014-aws, git 2.43.0), `flock ~/bench.lock` for all timing, load1 3.1 at
start of the timed run. vcs = origin/main dacc807 release build. Datasets `datagen <layout> 100000 <dir> 42`, one
initial commit plus 50 one-line edit commits (as `scripts/bench_space.py`). Component runs are size-only (no timing)
and were not under the lock. Apparent bytes (`st_size`), MiB.
Raw outputs: `docs/research/data/size-audit/`. Tools: `scripts/space_components.py` (component classifier),
`scripts/bench_size_speed.py` (interleaved speed check across store layouts, wall and user+sys CPU via wait4).

## Per component, 100k files

| layout / cell | pack | idx | index | loose (files) | total |
|---|---|---|---|---|---|
| flat vcs client, 51 commits | 62.18 | 4.24 | 6.80 | 2.12 (302) | 75.34 |
| flat git client, 51 commits | 39.29 | 3.08 | 7.66 | 6.01 (11958) | 56.09 |
| flat vcs after repack+gc | 64.04 | 4.25 | 6.80 | 0 | 75.10 |
| flat git after gc | 39.31 | 3.09 | 7.66 | 0 | 50.10 |
| flat vcs remote after publish | 43.76 | 4.25 | (manifest ~0) | - | 48.01 |
| flat git bare after push | 39.31 | 3.09 | - | - | 42.42 |
| flat vcs fresh clone | 64.26 | 8.49 (3 files) | 6.77 | 0 | 79.52 |
| flat git fresh clone | 39.31 | 3.09 | 7.66 | - | 50.08 |
| flat vcs client after `repack --format v3` | 42.76 | 4.25 | 6.80 | - | 53.82 |
| flat vcs client after `repack --format v4` | 42.19 | 4.25 | 6.80 | - | 53.25 |
| deep vcs client / git client (post-51) | 65.80 / 43.44 | 7.87 / 5.72 | 8.74 / 12.17 | 0.10 / 9.14 | 82.51 / 70.50 |
| deep vcs remote / git bare | 46.62 / 43.48 | 7.89 / 5.74 | - | - | 54.50 / 49.24 |
| deep vcs fresh clone / git clone | 65.87 / 43.48 | 15.77 / 5.74 | 8.49 / 12.17 | - | 90.12 / 61.42 |
| binary vcs client / git client (post-51) | 395.63 / 123.5 pack + 396.5 loose | 4.24 | 6.81 / 7.66 | 2.29 / 396.5 | 408.96 / 527.68 |
| binary vcs remote / git bare | 396.65 / 394.49 | 4.25 / 3.09 | - | - | 400.90 / 397.60 |
| binary vcs fresh clone / git clone | 397.71 / 394.49 | 8.49 / 3.09 | 6.77 / 7.66 | - | 412.98 / 405.26 |

There is no commit-graph, bloom or manifest file in the vcs client store (nothing to cut there); the remote has one
manifest of a few bytes and a `tx/` record. Findings:

1. **The 78.2 vs 51.0 MiB client gap is the pack body.** vcs v2 pack 62 to 64 MiB vs git 39 MiB on flat (the same
   data as v3 blocks is 42.8 MiB and as v4 deltas 42.2 MiB). Per-object zstd-3 on ~570 byte files cannot share
   redundancy between files; git's delta chains and v3/v4 can. Deep: 65.8 vs 43.4. Binary is incompressible, so
   there is no gap (395.6 vs 394.5).
2. **A fresh clone is 79.5 MiB (flat) although the remote it came from is 48.0.** The clone stores the objects as
   v2 entries (64.3 MiB) instead of keeping the remote's blocks (43.8 MiB), and holds `remote-idx/` (4.25 MiB, a
   byte copy of the remote index kept so later fetches do not re-read it) next to its own idx (4.24 MiB).
3. The vcs idx is larger than git's (4.25 vs 3.09 flat, 7.9 vs 5.7 deep): 32 byte ids against 20.
4. Loose objects are not a vcs problem (2 MiB / 302 files before maintenance); git pre-gc carries 6 to 9 MiB loose.

## Speed check of the cold-recompression options (flat 100k, 5 runs, interleaved, medians)

Same binary and identical repos; only `vcs repack --format X` differs. `v2` and `v2new` are both the default layout
(two copies, built by origin/main and by the experimental binary), so their difference is the noise floor. Cells are
wall ms / user+sys CPU ms. `v3lru` is pack v3 with 4 MiB blocks and an experimental 8-block read cache (not
shipped); `v3k256` / `v3k64` are v3 with 256 / 64 KiB blocks (experimental `VCS_BLOCK_KIB`, not shipped).

| cell | v2 | v2new | v3lru (4 MiB) | v3k256 | v3k64 |
|---|---|---|---|---|---|
| store bytes (du -sb .vcs) | 78.7M | 78.7M | 56.3M | 57.6M | 59.0M |
| status | 126/949 | 121/1041 | 115/938 | 128/901 | 119/1001 |
| add 1 file | 56/57 | 54/57 | 56/57 | 63/57 | 56/57 |
| commit 1 file | 66/60 | 63/60 | 67/65 | 64/62 | 65/60 |
| log -n 50 | 3/3 | 2/2 | 3/3 | 3/3 | 3/3 |
| switch to old commit | 268/829 | 163/843 | 351/946 | 187/848 | 165/851 |
| restore 10% of files | 1018/1027 | 846/1014 | 3748/23607 | 1301/3415 | 1128/1703 |
| restore all files | 1005/1876 | 905/1912 | 25531/194371 | 2992/21243 | 1337/7380 |

fsck passed on every variant (101366 objects). Earlier partial run (7 runs, same box, v2 / v3 without the cache / v4;
killed during restore-all to free the lock; `speed-v2-v3-v4-flat100k-partial.log`): switch 143/950 vs v3 360/1213 vs v4
137/900; restore 10%: v2 273/1051, **v3 23969/30539**, **v4 493/1085** (wall +80%).

Verdict (rule: no regression on any cell, CPU or wall):

- **v3 blocks (any size) fail**: restore costs 1.7x (64 KiB) to 100x (4 MiB) the CPU of v2, because reading one
  object decodes its whole block. Even a read cache only moves 24 s to 3.7 s at 4 MiB. Not shippable.
- **v4 deltas fail**: restore 10% is +80% wall (273 vs 493 ms). Not shippable as a default.
- status, add, commit, log are unaffected by pack format in every variant (store reads are not on those paths).
- fetch and clone were not re-timed: they do not read the client's packs, and any change to what clone writes
  would be a hot-path change that I did not make.

## What would be free, not done

- Sharing the remote's pack in a clone (finding 2) would remove ~20 MiB of the 79.5 MiB and skip re-encoding, but a
  clone would then hold v3 blocks and pay the restore costs above. It needs a hybrid first: keep head-tree blobs as
  per-object entries and put only history-only (cold) objects in blocks/deltas. The 100k test histories have 50 edits,
  so the cold set is tiny and this audit cannot measure the benefit; it needs a deep-history dataset.
- Dropping `remote-idx/` (4.25 / 7.9 MiB) would make every later fetch re-read the remote index: it trades size for speed,
  so it was left.
- A shorter id in idx (finding 3) is a format change; not attempted.

## Not run

Deep and binary layouts were measured for size only (components table), not for the speed cells; fetch and clone
timing; walgit was not re-run (walgit figures come from `docs/research/walgit-benchmark.md`).
