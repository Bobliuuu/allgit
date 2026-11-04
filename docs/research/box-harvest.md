# Box harvest: results recovered after the laptop crash (2026-11-02 to 2026-11-03)

After a laptop crash, the files left on the AWS timing boxes (c, d, e, f, g, h) were pulled and checked against `docs/`.
This page lists what was recovered, what was already committed, what is new, and which runs are incomplete. Raw small
files are in `docs/research/box-harvest-data/` (nothing over 1 MB; a credential grep for AKIA, ASIA, secret, token and
password found nothing in the copied files). All cells are medians, vcs divided by the reference, **below 1 means vcs
used less**. Verdict rule is the scoreboard's ([command-scoreboard.md](command-scoreboard.md)): WIN = both wall and CPU
ratio <= 0.90, LOSS = both >= 1.10, TIE = everything else and any cell with a wall difference under 2 ms.
`TIE (mixed)` means one of the two ratios is outside the band and the other is not.

## What was already in `docs/` (nothing to add)

| box result | where it is committed |
|---|---|
| box D walgit/deep/binary 100k (`walgitd-env/out`, `out2`) | `data/walgit-full-boxd`, `data/walgit-full-boxd-428` (byte-identical) |
| box D high-load matrix (`d/hl/mx-*.json`, `gfs.json`) | `highload-matrix.md`, `highload-data/` (byte-identical to `mx-gc`, `mx-same`, `mx-seq`, `gfs`) |
| box E command scoreboard (`scoreboard-out`, `sb5-out`) | `command-scoreboard.md`, `data/command-scoreboard` and `-prev` |
| peering, CI benchmark, zone profile | merged PRs, `peering.md`, `ci-benchmark.md`, `zone-profile-bench.md` |

Box c (`~/boxharvest/c`) is empty. Dozens of `rt/*` directories on d, e, f, g and h (`pr377`, `vfy-*`, `idxinc`, `agentbench*`,
`autosync`, `hotpath`, `soakchaos`, ...) are only checkouts of the repository plus `target/`, with no results in them.

## New 1: soak and chaos, kill -9 of the server (box G)

`data`: `box-harvest-data/soak-chaos/`. vcs-server on local disk, writers pushing while the server is killed with
`kill -9` and restarted; afterwards every acknowledged file is looked up in a fresh clone.

| run | duration | kill -9 count | acked files | missing | wrong content | fsck of fresh clone | gc dry run |
|---|---|---|---|---|---|---|---|
| chaos-kill9 | about 14 min (852 s of kill log, 860 s of samples) | 157 | 245 | **0** | **0** | ok (1026 objects) | nothing deleted; 175 packs |
| smoke (no kills, vcs) | about 100 s | 0 | 468 | 0 | 0 | ok (1448 objects) | 0 would be deleted |
| gsmoke (git, verify only) | n/a | n/a | 270 | 0 | 0 | ok | n/a (server_alive_at_end false) |

Also checked in chaos-kill9: "last wins" on 19 contended files (0 mismatches), a clone of a directory copy of the store
is fsck-clean, the server was alive at the end.

Honest limits: this is a 14-minute run on one box, not an hours-long soak. The 157 kills are about one every 5.4 s, so
most of them land during idle or short writes. Server RSS, CPU and fd samples are `0` or `-1` in the chaos run (the
sampler followed a pid that was killed), so the chaos run shows **no memory or fd trend**; only the non-chaos smoke
run has those (RSS 5.0 to 9.1 MB over 100 s). The git "gsmoke" row has no `last_wins` data and no kill step, so it is
not a chaos comparison against git. No result for a kill during `gc` (only a dry run was done).

## New 2: re-measure of the LOSS rows against git (box E, 100k flat, 2026-11-03)

Data: `box-harvest-data/git-losses-rerun/` (`gl2-*.json`), git 2.43.0. These re-time rows that were LOSS in the
scoreboard. `base` is the build before the fixes; `scan1`, `g1`, `z1` to `z6` are after fixes, on a build whose revision was
not recorded in the JSON.

| cell | git wall / CPU ms | vcs wall / CPU ms | wall | CPU | n | verdict | file, load1 at start |
|---|---|---|---|---|---|---|---|
| `ls-files` before | 14.0 / 13.9 | 17.7 / 31.5 | 1.27 | 2.27 | 9 | LOSS | gl2-base, 5.1 |
| `ls-files` after | 14.2 / 14.0 | 8.6 / 19.5 | 0.61 | 1.39 | 9 | TIE (mixed) | gl2-scan1, 3.9 (rose to 19 during the group) |
| `ls-files -s` after | 27.9 / 27.8 | 11.4 / 29.7 | 0.41 | 1.07 | 9 | TIE (mixed) | gl2-scan1 |
| `grep` whole tree (new row) | 728 / 2,490 | 91 / 1,019 | 0.13 | 0.41 | 9 | WIN | gl2-scan1 |
| `grep -- path` before | 11.0 / 13.8 | 18.7 / 34.0 | 1.69 | 2.46 | 9 | LOSS | gl2-base |
| `grep -- path` after | 11.1 / 13.8 | 11.2 / 26.2 | 1.01 | 1.90 | 9 | TIE | gl2-scan1 |
| `log --stat` | 3.4 / 3.3 | 5.6 / 5.5 | 1.65 | 1.67 | 9 | LOSS | gl2-base (no after run recovered) |
| `bundle create` before | 363 / 670 | 560 / 1,728 | 1.54 | 2.58 | 3 | LOSS | gl2-base |
| `bundle create` after, 5 runs | 337 to 540 / 612 to 650 | 331 to 1,122 / 796 to 1,377 | 0.96 to 2.65 | 1.23 to 2.25 | 3 each | LOSS in 3 of 5, TIE in 2 | gl2-z1, z3 to z6; **load1 9 to 23, not a clean measurement** |
| `gc` before | 547 / 796 | 1,067 / 1,091 | 1.95 | 1.37 | 3 | LOSS | gl2-base-gc |
| `gc` after | 527 / 825 | 272 / 1,920 | 0.51 | 2.33 | 3 | TIE (mixed): half the wall time, 2.3x the CPU | gl2-g1 |
| `archive` (new row) | 1,325 / 784 | 732 / 420 | 0.55 | 0.54 | 3 | WIN | gl2-z1, load1 23 |

Reading: the `ls-files` and `grep -- path` losses are gone in wall time but **CPU is still 1.4x to 1.9x git** (vcs spends
more threads for less wall time). `bundle create` is still slower than git and its 5 repeat runs are too noisy to
claim any change. `gc` trades CPU for wall time. `log --stat` has no after-fix run.

## New 3: hot path baseline, status and diff (box H, 100k flat, 2026-11-03, quiet box)

Data: `box-harvest-data/hotpath-baseline/` (`base-clean`, `base-dirty`, `base-small`). load1 0.13 to 0.32, 9 runs per cell,
git 2.43.0. Only the **baseline** (before) files survived; there is no after-run, so no improvement is claimed.

| cell | git wall / CPU ms | vcs wall / CPU ms | wall | CPU | verdict |
|---|---|---|---|---|---|
| `status` clean | 62.9 / 175.1 | 29.5 / 43.4 | 0.47 | 0.25 | WIN |
| `status -s` clean | 64.5 / 175.3 | 28.6 / 42.4 | 0.44 | 0.24 | WIN |
| `status` dirty | 69.1 / 181.2 | 30.7 / 44.5 | 0.44 | 0.25 | WIN |
| `diff` clean | 21.5 / 132.5 | 7.0 / 6.5 | 0.33 | 0.05 | WIN |
| `diff -- path` clean | 9.7 / 12.7 | 5.5 / 5.4 | 0.57 | 0.43 | WIN |
| `diff` dirty | 64.0 / 173.3 | 58.6 / 552.1 | 0.92 | **3.19** | TIE (mixed): CPU is a loss |
| `diff --name-only` dirty | 29.9 / 139.6 | 56.2 / 549.9 | **1.88** | **3.94** | **LOSS** |
| `add` 1 file | 49.7 / 49.8 | 42.5 / 49.8 | 0.86 | 1.00 | TIE (mixed) |
| `commit` 1 file | 64.5 / 175.0 | 64.8 / 58.8 | 1.00 | 0.34 | TIE |
| `restore` 1 file | 45.1 / 45.1 | 44.9 / 49.9 | 1.00 | 1.11 | TIE |
| `log -n 5`, `show`, `branch` list | 1.0 to 1.9 | 0.9 to 1.7 | 0.7 to 1.2 | 0.7 to 1.2 | TIE (under 2 ms) |

Discrepancy to know about: the box E scoreboard (README table) has clean `status` at 55 ms against git's 65 ms (0.88 wall,
1.04 CPU), while this baseline has 29.5 ms (0.47, 0.25). Different box, different (unrecorded) build, and the watcher
is on by default for big repos in later builds. The README table keeps the box E number; the two are not the same
measurement. The p4 re-run below (box E, 2026-11-03) shows the same 30 ms `status`, so the newer figure is probably
real for current builds, but it needs a scoreboard re-run to be quoted in the table.

## New 4: Perforce comparison re-runs (box E, 100k, p4d on loopback)

Data: `box-harvest-data/p4-rerun/` (`base`, `aw`, `aw2`, 3 runs per cell, 2026-11-03; `earlier/` holds two 15-run files from
2026-11-02 with fewer cells). Builds not recorded. `base` is the start of the session, `aw2` is the last run. p4 CPU is a lower
bound (p4d read in 10 ms ticks), so ignore p4 CPU ratios for cells under 10 ms and judge those by wall time.

| cell | p4 wall ms | vcs wall base -> aw2 | verdict base -> aw2 |
|---|---|---|---|
| `status` clean | 1,300 -> 1,155 | 134 -> 30 | TIE (mixed) -> WIN |
| `status` 1% changed | 1,179 -> 1,170 | 127 -> 30 | TIE (mixed) -> WIN |
| `opened` | 3.9 | 113 -> 32 | LOSS -> LOSS (8x p4 wall) |
| `diff` one file | 4.3 -> 4.1 | 67 -> 25 | LOSS -> LOSS (6x) |
| `add` one file | 4.7 -> 4.8 | 61 -> 63 | LOSS (13x, unchanged) |
| `revert` one file | 4.3 | 43 -> 24 | LOSS -> LOSS (5.5x) |
| `delete` one file | 4.8 -> 4.6 | 30 -> 26 | LOSS -> LOSS (5.6x) |
| `submit` one file | 5.8 -> 5.9 | 179 -> 112 | LOSS -> LOSS (19x) |
| `sync` 1% changed | 478 -> 315 | 803 -> 369 | LOSS -> LOSS (1.17x wall, 1.44x CPU) |
| `grep` | 57 | 100 -> 85 | LOSS (1.5x wall, CPU 380x p4's tick-limited figure) |
| `add` 100k files | 3,080 -> 3,015 | 580 -> 536 | WIN -> TIE (mixed): wall 0.18, CPU 0.92 |
| `submit` 100k files | 19,080 -> 18,299 | 677 -> 646 | WIN -> WIN (about 28x faster wall) |
| `files`, `have`, `changes`, `sync` no-op | 44 to 130 | 2 to 28 | WIN, both runs |
| `filelog`, `annotate`, `fstat` | 3.5 to 3.9 | 5.2 to 5.8 | TIE (under 2 ms difference) |
| `clone`/full sync (n=2) | 1,531 -> 1,525 | 1,153 -> 1,169 | TIE (mixed): wall 0.77, CPU 2.0 |

`verify` (vcs 660 ms vs p4 1,440 ms) is a TIE (mixed) because the p4 CPU is not captured. Small n=3 samples throughout.

## New 5: bundle verify and ls-tree fix probes (box E)

Data: `box-harvest-data/slowfix/`. `bundle verify`: git 1.3 ms, vcs 1.9 ms (n=15, 1.50x wall; under 2 ms difference so TIE);
`ls-tree` flat: git 1.7 ms, vcs 2.2 ms (n=15, TIE). Both are down from the 467 ms and 34 ms in the scoreboard, but the
recovered runs were taken at load1 3.2 to 3.9 and do not say which build they ran, so they are not used in the README table.

## Incomplete or unusable runs, rerun needed

| run | what is wrong | rerun |
|---|---|---|
| `f/data/s1_nested.log` (hugescale, box F) | The vcs binary was missing or the repo was never initialised: every vcs row is `error: not inside a vcs repository`, rc 1. Only git rows are valid (add 231.9 s, clone 543.2 s at an unrecorded size). Nothing about vcs at huge scale exists. Kept in `hugescale-incomplete/` for the git baseline only. | rerun `hugescale` on a box with the build present |
| autosync (box H) | Directory holds a repository checkout and no result files. | run it |
| hot path after-fix (box H) | Only the `base-*` files exist. | rerun after the fix |
| `d/rt/bigdata` | `bigbench.sh` failed with a shell syntax error and `cd clone/c: No such file`; logs are empty. | fix the script and rerun |
| `d/rt/remotedone-s3` | Log shows RustFS `AccessDenied` on `PutObject` and `CreateMultipartUpload`; no result. | recheck bucket policy |
| `d/hl/run1.json` (= `run1-partial.json`) | `branches n=64 vcs_s3` never finished (the log stops there); `vcs_s3` at n=32 took 274 s and 2.3 pushes/s, a pre-fix figure. Superseded by `mx-*`. | covered by `highload-matrix.md` |
| `e/rt/gl2-z*` `bundle create` | load1 9 to 23 when taken. | rerun on a quiet box |
| soak | 14 minutes only, no RSS/fd data under kill, no kill during gc. | long soak with the sampler following the restarted pid |
| `d/rt/dc/*.out` | Unlabelled tuning scratch (base/new1/new8 wall times, `git maintenance` error `unknown option detach`); no usable conclusion. | not used |
| `e/rt/out-*.json` (16 files, 2026-11-02 07:16 to 08:06) | Tuning iterations for `rm`, `reset`, `restore`, `clean`; load1 4 to 28, before/after builds not recorded. Not copied. They do show the direction (for example `reset --hard` 554 ms before, 69 ms after, git 172 ms, but CPU 1.2x git). | rerun in the scoreboard on a quiet box |
