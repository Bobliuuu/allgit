# Interleaved git vs vcs comparison (before / after the optimizations)

Question: do the recent optimizations actually speed up what was slower than git (add, status flat/binary)?

Harness: `scripts/bench_compare.sh` (driver) and `scripts/bench_compare.py` (helper). Raw per-run data (wall, CPU,
exit code, load, iowait, run order) is in `docs/research/interleaved-data/*.json`; re-render any file with
`python3 -I scripts/bench_compare.py --report FILE.json`.

## How the harness is fair

- **Interleaved.** Each round runs every tool once, in random order (seeded), so all tools see the same machine load.
  The old `bench_scale.sh` ran git and vcs in separate time windows.
- **Wall and CPU.** CPU is user+sys of the child from `wait4` rusage (children it waited for included). CPU is much less
  load-sensitive than wall; in this run CPU max/min per cell stayed within 2x in almost every cell while wall swung by 5x.
- **Paired ratio.** For each run the ratio tool/git is taken, the median reported with a bootstrap 95% CI (10,000
  resamples of the run pairs). A ratio below 1 means faster than git.
- **Noisy rule.** If CPU max/min of a cell (vcs or its git partner) exceeds 2x, no ratio is reported ("noisy").
- Load average before/after each round, iowait% (from `/proc/stat`) and a "wait up to 90 s for the machine to be
  <8% busy" check before each round are recorded in the JSON. The check gives up after 90 s and runs anyway (see the
  caveat below: it gave up on most rounds).
- `--taskset CPUS` / `--nice N` prefix every benchmarked command with `taskset -c` / `nice -n` (util-linux `taskset`,
  present on both boxes). Pinning only helps if the chosen cores are otherwise idle; it was not used in the runs below.
- Two vcs builds plus git in one run: `VCS_BIN_BASE` and `VCS_BIN_NEW`; `VCS_BIN_WATCH` (a `--features watcher` build)
  adds a fourth tool `neww` for the status cells only, with `vcs watch` running and one untimed warm `status`
  before timing. CPU of the watch daemon is not counted in `neww`.
- Disk guard as in `bench_scale.sh` (`MIN_FREE_GB`, default 4). Datagen is `datagen <layout> N` (seed 42).
- Same workflows as `bench_scale.sh`: add (fresh index), commit (all staged), status clean, status 1% modified,
  log -n 50 (49 commits of history), checkout (switch with 1% of files differing).
- The vcs commands need a user identity in the new build, so the harness sets `user.name`/`user.email` in each repo
  after `init` (`bench_scale.sh` does not; its commit cell fails on current main).

Re-run everything in one command once the in-flight branches (perf/fresh-add, perf/status-and-watcher) land:

```
VCS_BIN_BASE=/path/vcs-c47fcda VCS_BIN_NEW=target/release/vcs VCS_BIN_WATCH=/path/vcs-watcher-build \
  scripts/bench_compare.sh --runs 7 --out results/
```

Build the base with `git worktree add <dir> c47fcda && cargo build --release -p vcs-cli`.

## Setup actually used

- Machine: AWS-lab instance b (8 vCPU, ext4, warm cache; the GPU was not touched), git 2.43.0, Linux 7.0.0-1014-aws.
  The 30-core Lambda box was abandoned for the final runs (load 40-58 there; an aborted partial run is not used).
- **The box was not quiet.** `uptime` at start: load 4.6 with other people's `deltagate` and `git add .` running;
  later cells saw load 10-46 (15-17 logged-in users, other jobs sharing the disk, iowait up to 75%). So wall times
  in the later cells (binary 10k status/log/checkout, all of 100k) are contaminated; use the CPU columns for judgement.
  Another user's job also corrupted cargo's registry cache in `~/.cargo` on this box (files containing the text
  "modified ..."), so builds used a private `CARGO_HOME`.
- Builds: base = commit c47fcda; new = `cc1fffb` (origin/main when built, includes the fresh-add and status/watcher
  work, e.g. `AGI3` index, parallel `add`). The watcher build is the same tree with `--features watcher`.
- Runs: 7 per cell at 10k (3 layouts), **3 per cell at 100k** (CI from 3 pairs is wide and often degenerate).
- **Not run:** 100k deep and 100k binary. They were started but each cell takes over an hour on the shared box
  (binary add alone took about 70 minutes); stopped. The only 100k binary number measured was the add cell,
  CPU medians of 3 runs: git 21,949 ms, base 25,368 ms, new 9,137 ms (no CI, JSON not written because the run was
  stopped). No 100k deep number was measured.
- No numbers on a quiet machine were obtained; every cell here ran with other jobs present.

## Results

Columns: milliseconds; `ratio vs git` is the paired median tool/git with [95% CI]; below 1 means faster than git.
Tools: `git`, `base` = vcs c47fcda, `new` = vcs cc1fffb, `neww` = new with `vcs watch` running (status only).

### flat files=10000 runs=7 prefix=- load1 start/end 4.64/2.25

| cell | tool | wall med | wall min | CPU med | CPU min | CPU max/min | ratio vs git (CPU) | ratio vs git (wall) |
|---|---|---|---|---|---|---|---|---|
| add (fresh index) | git | 1497 | 1352 | 1457 | 1352 | 1.11 | 1 | 1 |
| add (fresh index) | base | 6816 | 6372 | 3385 | 2870 | 1.21 | 2.35 [2.09, 2.50] | 4.26 [2.23, 4.63] |
| add (fresh index) | new | 117 | 110 | 224 | 223 | 1.14 | 0.15 [0.15, 0.17] | 0.07 [0.04, 0.10] |
| commit (all staged) | git | 106 | 66 | 84 | 83 | 1.06 | 1 | 1 |
| commit (all staged) | base | 128 | 110 | 50 | 50 | 1.05 | 0.60 [0.58, 0.60] | 1.28 [0.94, 1.71] |
| commit (all staged) | new | 89 | 69 | 51 | 51 | 1.07 | 0.61 [0.61, 0.62] | 1.05 [0.86, 1.27] |
| status (clean) | git | 15 | 14 | 31 | 31 | 1.04 | 1 | 1 |
| status (clean) | base | 57 | 57 | 83 | 81 | 1.07 | 2.69 [2.61, 2.73] | 4.02 [3.82, 4.11] |
| status (clean) | new | 28 | 26 | 62 | 61 | 1.35 | 1.98 [1.96, 2.04] | 1.91 [1.78, 2.10] |
| status (clean) | neww | 15 | 15 | 14 | 14 | 1.05 | 0.46 [0.45, 0.46] | 1.07 [0.98, 1.08] |
| status (1% modified) | git | 15 | 15 | 32 | 30 | 1.07 | 1 | 1 |
| status (1% modified) | base | 57 | 57 | 83 | 81 | 1.03 | 2.60 [2.56, 2.68] | 3.78 [3.45, 3.92] |
| status (1% modified) | new | 27 | 26 | 63 | 62 | 1.03 | 1.98 [1.96, 2.08] | 1.75 [1.62, 1.93] |
| status (1% modified) | neww | 15 | 14 | 14 | 14 | 1.03 | 0.45 [0.44, 0.46] | 0.98 [0.95, 1.01] |
| log (50 commits) | git | 4 | 4 | 4 | 4 | 1.09 | 1 | 1 |
| log (50 commits) | base | 2 | 2 | 2 | 2 | 1.28 | 0.56 [0.55, 0.60] | 0.57 [0.56, 0.62] |
| log (50 commits) | new | 4 | 3 | 3 | 3 | 1.12 | 0.94 [0.92, 0.96] | 0.94 [0.92, 0.95] |
| checkout (1% differ) | git | 41 | 41 | 59 | 58 | 1.02 | 1 | 1 |
| checkout (1% differ) | base | 122 | 122 | 128 | 127 | 1.06 | 2.19 [2.16, 2.25] | 2.99 [2.95, 3.34] |
| checkout (1% differ) | new | 76 | 75 | 95 | 95 | 1.02 | 1.63 [1.61, 1.64] | 1.84 [1.83, 1.88] |


### deep files=10000 runs=7 prefix=- load1 start/end 2.25/1.99

| cell | tool | wall med | wall min | CPU med | CPU min | CPU max/min | ratio vs git (CPU) | ratio vs git (wall) |
|---|---|---|---|---|---|---|---|---|
| add (fresh index) | git | 1749 | 1531 | 1593 | 1531 | 1.14 | 1 | 1 |
| add (fresh index) | base | 7212 | 6607 | 3778 | 3693 | 1.07 | 2.36 [2.31, 2.45] | 4.29 [2.29, 4.44] |
| add (fresh index) | new | 452 | 431 | 580 | 568 | 1.21 | 0.37 [0.36, 0.37] | 0.28 [0.14, 0.28] |
| commit (all staged) | git | 3530 | 3315 | 3484 | 3385 | 1.13 | 1 | 1 |
| commit (all staged) | base | 18017 | 16662 | 6976 | 6602 | 1.09 | 2.00 [1.97, 2.04] | 4.94 [3.23, 5.56] |
| commit (all staged) | new | 233 | 229 | 215 | 213 | 1.05 | 0.06 [0.06, 0.06] | 0.07 [0.04, 0.07] |
| status (clean) | git | 276 | 274 | 346 | 344 | 1.01 | 1 | 1 |
| status (clean) | base | 207 | 193 | 566 | 525 | 1.10 | 1.64 [1.56, 1.66] | 0.75 [0.74, 0.76] |
| status (clean) | new | 114 | 107 | 414 | 397 | 1.96 | 1.20 [1.16, 1.31] | 0.41 [0.40, 0.45] |
| status (clean) | neww | 29 | 29 | 29 | 28 | 14.11 NOISY | noisy | noisy |
| status (1% modified) | git | 278 | 276 | 349 | 347 | 1.01 | 1 | 1 |
| status (1% modified) | base | 203 | 189 | 550 | 500 | 1.16 | 1.57 [1.54, 1.61] | 0.73 [0.72, 0.75] |
| status (1% modified) | new | 115 | 105 | 427 | 395 | 1.14 | 1.23 [1.14, 1.25] | 0.41 [0.39, 0.42] |
| status (1% modified) | neww | 29 | 29 | 29 | 29 | 15.25 NOISY | noisy | noisy |
| log (50 commits) | git | 4 | 4 | 4 | 3 | 1.10 | 1 | 1 |
| log (50 commits) | base | 2 | 2 | 2 | 2 | 1.08 | 0.55 [0.54, 0.57] | 0.56 [0.55, 0.58] |
| log (50 commits) | new | 5 | 5 | 5 | 5 | 1.06 | 1.32 [1.28, 1.37] | 1.32 [1.27, 1.34] |
| checkout (1% differ) | git | 80 | 79 | 154 | 153 | 1.01 | 1 | 1 |
| checkout (1% differ) | base | 624 | 604 | 956 | 912 | 1.06 | 6.22 [6.16, 6.24] | 7.83 [7.68, 7.89] |
| checkout (1% differ) | new | 100 | 99 | 131 | 130 | 1.01 | 0.85 [0.85, 0.85] | 1.25 [1.24, 1.27] |


### binary files=10000 runs=7 prefix=- load1 start/end 1.99/9.33

| cell | tool | wall med | wall min | CPU med | CPU min | CPU max/min | ratio vs git (CPU) | ratio vs git (wall) |
|---|---|---|---|---|---|---|---|---|
| add (fresh index) | git | 2389 | 2369 | 2384 | 2368 | 1.02 | 1 | 1 |
| add (fresh index) | base | 6667 | 6273 | 3695 | 3657 | 1.02 | 1.55 [1.53, 1.56] | 2.81 [2.74, 2.90] |
| add (fresh index) | new | 203 | 189 | 350 | 349 | 1.08 | 0.15 [0.15, 0.15] | 0.09 [0.08, 0.09] |
| commit (all staged) | git | 73 | 65 | 90 | 84 | 1.65 | 1 | 1 |
| commit (all staged) | base | 146 | 108 | 52 | 49 | 1.28 | 0.54 [0.49, 0.61] | 1.60 [1.44, 2.10] |
| commit (all staged) | new | 97 | 52 | 55 | 51 | 1.34 | 0.58 [0.54, 0.62] | 0.96 [0.75, 1.49] |
| status (clean) | git | 45 | 18 | 39 | 33 | 1.22 | 1 | 1 |
| status (clean) | base | 94 | 66 | 94 | 83 | 1.19 | 2.47 [2.32, 2.51] | 2.59 [0.93, 3.32] |
| status (clean) | new | 64 | 36 | 59 | 57 | 1.46 | 1.48 [1.44, 2.08] | 1.43 [1.25, 1.92] |
| status (clean) | neww | 23 | 15 | 20 | 14 | 1.54 | 0.50 [0.45, 0.51] | 0.76 [0.51, 0.91] |
| status (1% modified) | git | 61 | 32 | 40 | 38 | 1.08 | 1 | 1 |
| status (1% modified) | base | 85 | 61 | 97 | 86 | 1.24 | 2.37 [2.34, 2.51] | 2.41 [0.47, 2.67] |
| status (1% modified) | new | 56 | 38 | 70 | 61 | 1.22 | 1.82 [1.58, 1.86] | 1.20 [0.33, 1.56] |
| status (1% modified) | neww | 29 | 17 | 20 | 16 | 1.30 | 0.49 [0.46, 0.51] | 0.64 [0.41, 2.34] |
| log (50 commits) | git | 4 | 4 | 4 | 4 | 1.98 | 1 | 1 |
| log (50 commits) | base | 2 | 2 | 2 | 2 | 1.91 | 0.56 [0.56, 0.60] | 0.60 [0.56, 0.65] |
| log (50 commits) | new | 4 | 4 | 4 | 3 | 7.11 NOISY | noisy | noisy |
| checkout (1% differ) | git | 46 | 46 | 65 | 63 | 1.26 | 1 | 1 |
| checkout (1% differ) | base | 324 | 232 | 137 | 134 | 1.22 | 2.11 [2.10, 2.14] | 5.78 [5.09, 10.95] |
| checkout (1% differ) | new | 269 | 198 | 99 | 96 | 1.08 | 1.52 [1.48, 1.56] | 5.31 [4.35, 6.37] |


### flat files=100000 runs=3 prefix=- load1 start/end 8.99/9.78

| cell | tool | wall med | wall min | CPU med | CPU min | CPU max/min | ratio vs git (CPU) | ratio vs git (wall) |
|---|---|---|---|---|---|---|---|---|
| add (fresh index) | git | 17499 | 12400 | 10890 | 9230 | 1.23 | 1 | 1 |
| add (fresh index) | base | 132170 | 117702 | 25053 | 24971 | 1.24 | 2.71 [2.20, 2.84] | 6.73 [3.64, 10.87] |
| add (fresh index) | new | 2505 | 2244 | 2552 | 2499 | 1.18 | 0.27 [0.22, 0.27] | 0.18 [0.07, 0.22] |
| commit (all staged) | git | 2062 | 897 | 794 | 757 | 1.08 | 1 | 1 |
| commit (all staged) | base | 5986 | 5605 | 528 | 508 | 1.06 | 0.67 [0.65, 0.68] | 3.51 [2.29, 6.25] |
| commit (all staged) | new | 1920 | 492 | 338 | 316 | 1.17 | 0.42 [0.41, 0.46] | 0.93 [0.19, 2.49] |
| status (clean) | git | 229 | 220 | 311 | 306 | 1.04 | 1 | 1 |
| status (clean) | base | 607 | 593 | 855 | 845 | 1.05 | 2.76 [2.68, 2.85] | 2.76 [0.94, 5.49] |
| status (clean) | new | 708 | 179 | 673 | 630 | 1.41 | 2.11 [2.06, 2.86] | 3.09 [0.28, 5.02] |
| status (clean) | neww | 125 | 85 | 168 | 166 | 1.02 | 0.54 [0.53, 0.54] | 0.55 [0.37, 0.57] |
| status (1% modified) | git | 882 | 613 | 325 | 318 | 1.06 | 1 | 1 |
| status (1% modified) | base | 673 | 611 | 876 | 860 | 1.05 | 2.64 [2.59, 2.84] | 1.00 [0.63, 1.10] |
| status (1% modified) | new | 267 | 262 | 661 | 644 | 1.05 | 2.03 [1.95, 2.07] | 0.43 [0.25, 0.91] |
| status (1% modified) | neww | 457 | 87 | 167 | 165 | 1.04 | 0.51 [0.51, 0.52] | 0.43 [0.14, 0.91] |
| log (50 commits) | git | 4 | 4 | 4 | 4 | 1.18 | 1 | 1 |
| log (50 commits) | base | 3 | 2 | 2 | 2 | 1.24 | 0.57 [0.52, 0.59] | 0.61 [0.53, 3.77] |
| log (50 commits) | new | 9 | 9 | 9 | 9 | 1.12 | 2.05 [2.01, 2.18] | 2.14 [2.00, 11.69] |
| checkout (1% differ) | git | 5623 | 1208 | 568 | 561 | 1.24 | 1 | 1 |
| checkout (1% differ) | base | 11535 | 4030 | 1450 | 1411 | 1.09 | 2.48 [2.21, 2.59] | 3.34 [1.59, 5.90] |
| checkout (1% differ) | new | 13120 | 1828 | 1036 | 1020 | 1.15 | 1.85 [1.47, 2.06] | 1.81 [1.51, 6.27] |

(Load/iowait per round is in the JSON files.)

## Reading the results

Judge by CPU ratio first; wall is shown because parallel code spends more CPU than wall. `new` add, status and
checkout use many threads, so their CPU ratio is worse than their wall ratio on an idle machine, and on a loaded
box wall is dominated by other jobs.

**Got faster (base to new, CPU, same-run paired):**
- add (fresh index): flat 10k 3,385 to 224 ms; deep 3,778 to 580; binary 3,695 to 350; flat 100k 25,053 to 2,552.
  Against git it went from 1.55-2.71x slower (CPU) to 0.15-0.37x (faster), CIs do not touch 1.
- commit: deep 10k 6,976 to 215 ms (git 3,484); other layouts were already below git on CPU and are unchanged within noise
  (flat 10k 50 to 51 ms). flat 100k 528 to 338 ms.
- status: all layouts about 1.3-1.4x less CPU (flat 10k 83 to 62 ms; deep 566 to 414; binary 94 to 59; flat 100k 855 to 673).
- checkout: deep 10k 956 to 131 ms (now faster than git, 0.85); flat 10k 128 to 95; binary 137 to 99; flat 100k 1,450 to 1,036.
- status with the watcher (`neww`): flat 10k 14 ms vs git 31 (0.45x); binary 10k 0.49x; flat 100k 168 vs git 311 (0.54x).
  Faster than git in CPU on every layout where it is not noisy; wall is about git-level at 10k (0.98-1.07 flat).

**Still slower than git (CPU ratio, new vs git, CI above 1):**
- status without the watcher: flat 10k 1.98x (clean) / 1.98x (1%); binary 10k 1.48x / 1.82x; flat 100k 2.11x / 2.03x;
  deep 10k 1.20x / 1.23x. So the answer to the question: status flat/binary improved by about 1.3-1.4x but is still
  1.5-2x git's CPU. Its wall time is lower than git only for deep 10k (0.41x) and, under load, flat 100k 1%-modified (0.43x);
  on flat 10k wall is still 1.75-1.9x git.
- checkout: flat 10k 1.63x, binary 10k 1.52x, flat 100k 1.85x CPU (wall 1.8x at flat 10k and 100k).
- log -n 50: flat 100k 2.05x (9 ms vs 4 ms), deep 10k 1.32x (5 vs 4 ms); flat 10k 0.94x. `new` log is slower than
  base on these (base 2 ms): a regression of a few ms worth checking, not a large absolute cost.

**Faster than git already:** add (all layouts and both sizes measured), commit (all measured cells, CPU 0.06-0.61x),
log at flat 10k is on par.

**Noisy (no ratio reported by the rule, CPU max/min > 2x):** deep 10k `neww` status (CPU min 29 ms, max/min 14x: at least one
run fell back to a full scan; not investigated); binary 10k `new` log (3-4 ms, 7x). Wall at 100k: the status clean wall
for `new` (median 708 ms, min 179) and checkout/commit walls swing 3-10x with load (CIs like [0.19, 2.49]); treat
100k wall columns as unreliable, CPU columns as the result.

**Caveats.** 3 runs at 100k; one shared machine at load 5-20 (not the idle box that was intended); base vs new
compared by CPU only partly reflects wall when threads are used; `neww` excludes daemon CPU; deep and binary at
100k are missing; git 2.43.0.
