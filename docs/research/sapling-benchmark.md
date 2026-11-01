# Sapling (Meta `sl`) vs git vs vcs benchmark

Status: **measured, one 8-vCPU AWS box, shared with other jobs (load1 3 to 30 depending on the layout; see below)**.
Sapling was run in its **Git-backed store mode**. Nothing here is estimated; every number comes from the raw output at
the end of this file, produced by `scripts/bench_sapling.sh`.

## What was used

| item | value |
|---|---|
| machine | AWS "swarm-gpu" instance b, AMD EPYC 7R32, **8 vCPU**, 31 GiB RAM, ext4 on NVMe, kernel 7.0.0-1014-aws. CPU only; the GPU was not touched. The box is advertised as quiet but other agents were using it (see load) |
| git | 2.43.0 (Ubuntu), `GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null`, defaults otherwise (so `gc.auto` is on) |
| vcs | `vcs 0.1.0`, release build of `origin/main` at commit `28e2980` built on the same box (`cargo build --release -p vcs-cli`), identity via `VCS_AUTHOR_NAME/EMAIL` |
| Sapling | `Sapling 0.2.20261028-123539+7415fb2f`, the **official prebuilt Linux x64 release** (`sapling-0.2.20261028-123539+7415fb2f-linux-x64.tar.xz`), run from its own directory `~/rt/slbench/dl/x/sl`; no install, no sudo, no building from source needed |
| Sapling mode | `sl init --git` (the plain `sl init` of this release makes a non-Git store). Confirmed per run directory: `.sl/store/requires` contains `git` and `git-store`, `.sl/store/git` is a bare Git repository, commit ids are 40-hex Git SHA-1. Clone used `sl clone --git <bare git repo>` |
| datasets | `datagen flat|deep|binary 100000`, seed 42 default, the same generator and layouts as `scripts/bench_scale.sh` |
| script | `scripts/bench_sapling.sh <layout> 100000 5`: 5 runs per cell, warm cache, prep untimed, tools **interleaved** (run i of git, vcs and sl are executed back to back; the order rotates with i), load1 sampled before the first and the last run of each tool |
| metrics | wall ms (`perf_counter` around the subprocess) and CPU ms (user+sys of the command and every child it waited for, via `getrusage(RUSAGE_CHILDREN)`). Background processes that outlive the command (for example a detached `git gc --auto`) are not in the CPU number |

### Download and verification

* Source: GitHub release `0.2.20261028-123539+7415fb2f` of `facebook/sapling` (the repository moved to `meta/sapling`; the
  release API still serves it). Assets for that tag: linux-arm64, linux-x64, windows-x64 and a macOS bottle. **No
  detached signature or checksum file is published**; the only integrity data is the SHA-256 `digest` that the GitHub API reports for each asset.
* Linux x64 tarball SHA-256, computed on the box with `sha256sum`: `9504b9514ed16dce3b5443811f7625742a363e66a44b316bcaad70a9d013b274`,
  **equal** to the GitHub API digest for that asset (`sha256:9504b951...b274`, 36,709,220 bytes). This proves the file is
  the one GitHub stores; it is not a publisher signature. I did not verify the build provenance beyond that.
* Licence: not checked; the binary was only run, not redistributed or modified.
* The tarball was extracted into its own empty directory (`~/rt/slbench/dl/x`), contents: `sl`, `lib/python3.12`, `isl-dist.tar.xz`.

## Commands compared

| workflow | git | vcs | Sapling (Git store) |
|---|---|---|---|
| init | `git init -q -b main` | `vcs init` | `sl init --git -q` |
| add | `git add .` | `vcs add .` | `sl add -q .` |
| commit | `git commit -q -m c` | `vcs commit -m c` | `sl commit -q -m c` |
| status | `git status --porcelain` | `vcs status` | `sl status` |
| log | `git log --oneline -n 50` | `vcs log -n 50` | `sl log -l 50 -T '{node|short} {desc|firstline}\n'` |
| checkout | `git checkout -q side` then (timed) `git checkout -q main` | `vcs switch side` / `main` | `sl goto -q side` / `main` (bookmarks) |
| clone | `git clone -q file://<bare repo>` (repacked bare repo) | `vcs clone <published dir>` | `sl clone --git -q <bare git repo>` (the bare repo was filled by `sl push --to main`) |

Cell definitions are those of `scripts/bench_scale.sh`: **add** = fresh index, all 100k files; **commit** = after a
fresh `add`, time of the commit only; **status (1% modified)** = every 100th file (sorted) rewritten, 1,000 files (all three tools
reported 1,000 changed paths; vcs prints 2 extra header lines); **log** = a history of 50 commits (the first full commit plus 49 small ones); **checkout (1% differ)** = switch from `side`
(commit 1) to `main` (commit 50 differs in about 1,000 files); **clone** = full local clone of the 50-commit history, destination deleted
between runs. `du -sk` counts 4 KiB blocks per small file.

## Results: median of 5 runs


#### Median wall ms (git / vcs / sl)

| workflow | flat | deep | binary |
|---|---|---|---|
| add (fresh index) | 31,759 / 3,764 / 15,970 | 74,704 / 6,000 / 15,829 | 18,156 / 6,769 / 8,489 |
| commit (all staged) | 2,758 / 1,390 / 153,460 | 110,408 / 4,377 / 245,203 | 558 / 441 / 31,014 |
| status (clean) | 759 / 162 / 562 | 969 / 482 / 833 | 112 / 141 / 268 |
| status (1% modified) | 901 / 172 / 811 | 999 / 493 / 828 | 120 / 141 / 270 |
| log (50 commits) | 6 / 11 / 435 | 6 / 16 / 298 | 5 / 11 / 298 |
| checkout (1% differ) | 1,154 / 4,840 / 4,539 | 484 / 580 / 1,468 | 360 / 503 / 984 |
| clone (50 commits) | 11,244 / 13,359 / 19,790 | 15,866 / 26,766 / 79,264 | 9,856 / 9,138 / 10,412 |

#### Median CPU ms (git / vcs / sl)

| workflow | flat | deep | binary |
|---|---|---|---|
| add (fresh index) | 13,326 / 2,876 / 13,697 | 12,614 / 4,029 / 15,057 | 18,060 / 11,568 / 9,084 |
| commit (all staged) | 930 / 648 / 30,550 | 15,260 / 1,886 / 47,016 | 724 / 606 / 31,724 |
| status (clean) | 303 / 631 / 887 | 1,392 / 2,351 / 3,371 | 280 / 620 / 864 |
| status (1% modified) | 323 / 642 / 890 | 1,424 / 2,329 / 3,361 | 287 / 625 / 864 |
| log (50 commits) | 6 / 11 / 339 | 6 / 16 / 299 | 5 / 11 / 298 |
| checkout (1% differ) | 548 / 1,100 / 1,631 | 906 / 1,507 / 4,088 | 521 / 1,032 / 1,627 |
| clone (50 commits) | 6,016 / 12,551 / 10,557 | 8,401 / 17,241 / 17,865 | 10,077 / 17,244 / 14,547 |

Order in each cell: **git / vcs / Sapling**. Load at the start of each layout run (load1 on 8 vCPU): flat **29.4**, deep **13.3**,
binary **3.0**; at the end 13.2, 5.8, 1.8. Per-cell load ranges are in the raw output. Other jobs also used the disk: `iostat` showed
the NVMe device up to about 97 % busy during parts of the flat and deep runs. Consequences: **wall times of the flat and deep runs are inflated
and noisy** (single runs vary up to 10x, for example Sapling `commit` on flat: 47 s to 505 s); CPU times are much more stable (see the raw runs). The **binary run is the cleanest**
(load 1.8 to 3). Do not read wall differences under about 2x on flat and deep as real.

## Disk (`du -sk`, KiB)

| layout | working tree | store after 1st commit: git `.git` / vcs `.vcs` / sl `.sl` | store after 50 commits: git / vcs / sl | clone total (tree + store): git / vcs / sl | clone source dir: git / vcs / sl |
|---|---|---|---|---|---|
| flat | 404,032 | 51,404 / 74,940 / 418,056 | 62,204 / 84,704 / 458,860 | 456,336 / 484,976 / 451,060 | 44,872 / 52,628 / 44,412 |
| deep | 749,524 | 62,976 / 84,260 / 772,112 | 87,500 / 94,124 / 801,296 | 813,188 / 833,896 / 804,588 | 51,992 / 56,660 / 51,144 |
| binary | 404,032 | 414,964 / 420,924 / 818,152 | 425,764 / 430,672 / 879,604 | 819,888 / 827,412 / 814,560 | 408,432 / 413,816 / 407,968 |

The flat and deep text files compress well, and `git commit` ran `gc --auto` (default), which packed git's 100k loose objects, hence
51 MB. Sapling's local Git store after `sl commit` is **loose, about the size of the working tree on text (418 MB) and about 2x on random binary data
(818 MB)**: I did not run `git gc` / `sl` maintenance on it, so the packed size of a Sapling Git store was not measured locally. A fresh `sl clone` from a packed bare repo comes
out about the size of git's clone (last two columns), so the large figure is the loose-object state after a local commit, not a property of the data.

## What the numbers say

* **add**: on random binary data (quiet box) Sapling is the lowest in CPU (9.1 s vs git 18.1 s vs vcs 11.6 s) and between vcs and git in wall time (git 18.2 s, vcs 6.8 s, sl 8.5 s). On text, vcs
  is the fastest in both CPU and wall (2.9 to 4.0 s CPU vs 12.6 to 13.3 s for git and 13.7 to 15.1 s for Sapling).
* **commit**: this is Sapling's big cost on 100k files: **31 s wall and 32 s CPU on binary, 30 to 47 s CPU (and 2.5 to 4 minutes wall under disk contention) on text**, against 0.4 to 0.6 s (vcs, git, binary). Sapling writes 100,000 loose Git objects at commit time. On
  the deep layout git's own `commit` also spends 15 s CPU (110 s wall under contention) in `gc --auto`.
* **status**: git is cheapest in CPU (0.28 to 1.4 s); Sapling `status` uses 0.86 to 3.4 s CPU, vcs 0.62 to 2.4 s. Wall: binary 112 / 141 / 268 ms (git / vcs / sl), 1% modified costs the same as clean in all three. No file-system monitor (Watchman) was configured for Sapling and vcs's status watcher was left at its default.
* **log (50)**: git 5 to 6 ms, vcs 11 to 16 ms, Sapling 298 to 435 ms. Sapling's cost is its per-process start-up (about 0.3 s CPU), not the history walk; it would not grow with the 50 commits measured here.
* **checkout (1% differ)**: binary 360 / 503 / 984 ms (git / vcs / sl), deep 484 / 580 / 1,468 ms. On flat the wall times are disk-bound under load (git 1,154, vcs 4,840, sl 4,539 ms) while CPU was 548 / 1,100 / 1,631 ms: read the CPU row for the flat comparison.
* **clone** (local, 50 commits): binary 9.9 / 9.1 / 10.4 s, flat 11.2 / 13.4 / 19.8 s, deep 15.9 / 26.8 / 79.3 s (deep ran under load and disk contention, CPU 8.4 / 17.2 / 17.9 s).
* Compared with the earlier git/vcs tables in the README (different machine, a 30-core box, git 2.34.1, different load) the absolute values here differ, notably vcs `checkout`/`clone` wall time on flat; compare tools **within** this document only.

## Not measured, caveats

* One machine, one filesystem, one mode. Sapling's other modes were not run: its native (non-Git) store, EdenFS virtual file system (not usable without Eden), Watchman-assisted status, the Sapling server / Mononoke, `sl` with a remote.
* No cold-cache runs, no multi-user runs, no network. Clone is local (`file`-style paths), so no transfer protocol cost is in it. Git's source repo was repacked, Sapling's was filled by `sl push` (a packed bare repo), vcs's was a published directory.
* `sl log` and `sl status` start-up cost is included in every Sapling number as part of the command; this is how users run it. Sapling's run was a single process per command (I did not configure a command server).
* CPU excludes processes that outlive the command (a detached `git gc --auto`, background daemons); for git the effect shows up in the following cells' wall time, not in CPU.
* Sapling's commit hash is the Git SHA-1; the history is 50 linear commits, so no merge/rebase/graph-heavy workflow was measured. Features such as stacked commits, `sl absorb`, `sl isl` were not tried.
* The vcs figures here are for one build (`28e2980`); vcs changes quickly and earlier README numbers used other builds.
* `scripts/bench_scale.sh` as it exists does not set `VCS_AUTHOR_NAME`/`VCS_AUTHOR_EMAIL`; at this commit `vcs commit` needs an identity (it refused with "user.name and user.email are not set" when I tried), so the new script exports them. I did not change `bench_scale.sh`.
* A local `sl clone <sl repo>` of a Git-store repository failed in this release (`failed to open file .../.sl/store/gitdir`), which is why clone used a bare Git repo as the source. It is reported as observed, I did not investigate it.

## Reproduce

```bash
# on a Linux box; own directory only, no sudo
mkdir -p ~/rt/slbench/dl && cd ~/rt/slbench/dl
T=0.2.20261028-123539+7415fb2f
curl -sSL -o sl.tar.xz "https://github.com/facebook/sapling/releases/download/$T/sapling-$T-linux-x64.tar.xz"
sha256sum sl.tar.xz        # 9504b9514ed16dce3b5443811f7625742a363e66a44b316bcaad70a9d013b274
mkdir x && tar -xJf sl.tar.xz -C x
cargo build --release -p vcs-cli --bin vcs -p vcs-bench --bin datagen      # then copy both to ~/rt/slbench/bin
SL_BIN=~/rt/slbench/dl/x/sl scripts/bench_sapling.sh binary 100000 5
```

## Raw output (verbatim, `scripts/bench_sapling.sh`)

In the table rows, `load1 a..b` is the load average before the first and before the last run of that tool, not a min and a max.
Times are ms, runs listed in execution order.

### flat

```text
## Environment
date:     2026-11-01T04:23:49Z
uname:    Linux ip-172-31-7-56 7.0.0-1014-aws #14~24.04.1-Ubuntu SMP PREEMPT Fri Sep 18 21:24:59 UTC 2026 x86_64 x86_64 x86_64 GNU/Linux
cpu:      AMD EPYC 7R32  cores=8  mem=31GiB  fs=ext4
git:      git version 2.43.0
vcs:      vcs 0.1.0 (/home/ubuntu/rt/slbench/bin/vcs)
sl:       Sapling 0.2.20261028-123539+7415fb2f (/home/ubuntu/rt/slbench/dl/x/sl)
dataset:  layout=flat files=100000 seed=42 (datagen default); runs per cell=5; WARM cache; tools=git vcs sl
load1 at start: 29.41   free disk: 61.84 GiB

## Results (ms; raw runs in run order, median, load1 sampled before each run: min..max)
| workflow | tool | wall ms | median | cpu ms | median | load |
|---|---|---|---|---|---|---|
| add (fresh index)        | git  | wall: 31758.7 32237.6 14367.7 21539.9 46650.3 | med 31758.7 | cpu: 12973.3 13326.0 12700.5 13601.3 14585.3 | med 13326.0 | load1 31.59..26.29 |
| add (fresh index)        | vcs  | wall: 6018.4 3763.9 2959.9 3862.3 3042.4 | med 3763.9 | cpu: 3128.4 2804.3 2882.1 2876.2 2828.7 | med 2876.2 | load1 29.67..24.78 |
| add (fresh index)        | sl   | wall: 21802.0 14797.5 13487.6 16142.5 15970.0 | med 15970.0 | cpu: 13696.7 13617.0 13217.9 14178.5 13837.7 | med 13696.7 | load1 30.66..25.52 |
| commit (all staged)      | git  | wall: 1358.3 1417.5 2758.3 4554.8 17461.4 | med 2758.3 | cpu: 1010.8 987.2 930.2 901.1 907.7 | med 930.2 | load1 23.88..15.22 |
| commit (all staged)      | vcs  | wall: 1389.9 569.5 2900.2 2729.7 1207.9 | med 1389.9 | cpu: 676.9 648.3 655.7 617.7 578.9 | med 648.3 | load1 23.33..10.73 |
| commit (all staged)      | sl   | wall: 153460.3 47297.6 111139.8 251649.1 505451.2 | med 153460.3 | cpu: 39561.8 28371.1 29642.9 30550.2 40212.1 | med 30550.2 | load1 21.92..14.48 |

## Disk (du -sk of the store dir, KiB)
after first commit: git .git 51404
after first commit: vcs .vcs 74940
after first commit: sl .sl 418056
working tree: 404032 KiB (du -sk)

## Results (continued)
| status (clean)           | git  | wall: 758.6 494.4 553.8 909.5 763.4 | med 758.6 | cpu: 309.3 302.4 300.1 317.0 303.3 | med 303.3 | load1 12.55..12.16 |
| status (clean)           | vcs  | wall: 742.5 460.8 145.7 151.8 161.9 | med 161.9 | cpu: 807.9 628.0 631.4 633.2 621.6 | med 631.4 | load1 12.43..12.16 |
| status (clean)           | sl   | wall: 562.1 1117.9 744.9 450.2 355.9 | med 562.1 | cpu: 879.6 892.4 886.6 886.7 879.1 | med 886.6 | load1 12.43..12.16 |
| status (1% modified)     | git  | wall: 901.0 1544.3 1012.8 716.0 839.4 | med 901.0 | cpu: 323.1 325.9 331.2 309.0 309.7 | med 323.1 | load1 10.85..10.06 |
| status (1% modified)     | vcs  | wall: 916.7 240.2 165.8 172.0 146.7 | med 172.0 | cpu: 655.2 668.2 641.6 640.5 634.3 | med 641.6 | load1 10.54..10.06 |
| status (1% modified)     | sl   | wall: 811.2 1850.0 1318.7 682.9 695.4 | med 811.2 | cpu: 887.0 905.4 885.6 895.3 890.3 | med 890.3 | load1 10.54..10.06 |
status line counts (1% modified): git=1000 vcs=1002 sl=1000 
history length git: 50 log lines (git/vcs: commits; sl: template lines)
history length vcs: 300 log lines (git/vcs: commits; sl: template lines)
history length sl: 50 log lines (git/vcs: commits; sl: template lines)
| log (50 commits)         | git  | wall: 6.1 5.9 5.9 6.0 5.9 | med 5.9 | cpu: 5.9 5.7 5.7 5.8 5.7 | med 5.7 | load1 14.39..14.52 |
| log (50 commits)         | vcs  | wall: 11.4 14.2 11.5 11.4 10.9 | med 11.4 | cpu: 11.2 13.8 11.3 11.2 10.6 | med 11.2 | load1 14.39..14.52 |
| log (50 commits)         | sl   | wall: 855.5 411.7 435.8 398.8 434.8 | med 434.8 | cpu: 349.1 355.9 328.3 339.0 337.4 | med 339.0 | load1 14.39..14.52 |
| checkout (1% differ)     | git  | wall: 723.2 1028.3 1976.6 1154.0 3279.0 | med 1154.0 | cpu: 520.9 553.4 534.9 548.0 590.9 | med 548.0 | load1 14.52..21.45 |
| checkout (1% differ)     | vcs  | wall: 6557.3 6236.6 4839.8 3653.2 4204.7 | med 4839.8 | cpu: 1066.7 1099.5 1116.0 1097.6 1100.9 | med 1099.5 | load1 15.44..21.84 |
| checkout (1% differ)     | sl   | wall: 4555.4 4895.2 4538.9 3288.9 2898.6 | med 4538.9 | cpu: 1561.5 1596.5 1631.0 1634.5 1717.7 | med 1631.0 | load1 17.44..22.01 |
| clone (50 commits)       | git  | wall: 8425.8 14721.9 10639.8 11244.4 11962.6 | med 11244.4 | cpu: 6684.5 5652.8 6016.5 6092.0 5704.4 | med 6016.5 | load1 18.00..13.42 |
| clone (50 commits)       | vcs  | wall: 11805.0 28502.0 15978.4 7801.3 13359.0 | med 13359.0 | cpu: 12869.1 12346.0 12887.6 12079.6 12551.3 | med 12551.3 | load1 16.72..18.38 |
| clone (50 commits)       | sl   | wall: 19789.6 28238.3 34441.8 16135.7 15482.4 | med 19789.6 | cpu: 11006.3 10314.9 10299.4 11220.8 10556.6 | med 10556.6 | load1 16.19..14.94 |

## Disk after history and clone (KiB)
clone git: files=100000  du -sk total=456336
clone vcs: files=100000  du -sk total=484976
clone sl: files=100000  du -sk total=451060
store after 50 commits: git .git 62204
store after 50 commits: vcs .vcs 84704
store after 50 commits: sl .sl 458860
clone source src-git: 44872
clone source src-vcs: 52628
clone source src-sl: 44412

free disk at end: 49.20 GiB; load1 at end: 13.24
```

### deep

```text
## Environment
date:     2026-11-01T05:26:27Z
uname:    Linux ip-172-31-7-56 7.0.0-1014-aws #14~24.04.1-Ubuntu SMP PREEMPT Fri Sep 18 21:24:59 UTC 2026 x86_64 x86_64 x86_64 GNU/Linux
cpu:      AMD EPYC 7R32  cores=8  mem=31GiB  fs=ext4
git:      git version 2.43.0
vcs:      vcs 0.1.0 (/home/ubuntu/rt/slbench/bin/vcs)
sl:       Sapling 0.2.20261028-123539+7415fb2f (/home/ubuntu/rt/slbench/dl/x/sl)
dataset:  layout=deep files=100000 seed=42 (datagen default); runs per cell=5; WARM cache; tools=git vcs sl
load1 at start: 13.33   free disk: 50.96 GiB

## Results (ms; raw runs in run order, median, load1 sampled before each run: min..max)
| workflow | tool | wall ms | median | cpu ms | median | load |
|---|---|---|---|---|---|---|
| add (fresh index)        | git  | wall: 85872.3 37605.3 135692.6 53647.8 74703.9 | med 74703.9 | cpu: 13432.8 11712.2 12530.9 12627.1 12613.8 | med 12613.8 | load1 17.18..9.00 |
| add (fresh index)        | vcs  | wall: 38401.7 3620.1 6000.2 4157.2 6117.4 | med 6000.2 | cpu: 5748.3 4028.7 3966.9 4036.1 3970.8 | med 4028.7 | load1 11.64..8.53 |
| add (fresh index)        | sl   | wall: 24745.7 13191.4 17369.7 15829.4 14126.0 | med 15829.4 | cpu: 15738.1 14592.1 14472.0 15196.9 15056.7 | med 15056.7 | load1 12.42..8.57 |
| commit (all staged)      | git  | wall: 89133.4 218597.5 110408.2 199379.5 59651.0 | med 110408.2 | cpu: 16079.0 15260.5 14232.7 15563.2 14855.3 | med 15260.5 | load1 8.85..6.54 |
| commit (all staged)      | vcs  | wall: 2830.2 4679.0 8180.2 4376.7 3687.5 | med 4376.7 | cpu: 1981.4 1967.6 1866.6 1885.6 1863.2 | med 1885.6 | load1 8.39..7.66 |
| commit (all staged)      | sl   | wall: 245203.2 119884.2 1178983.4 636346.1 143478.8 | med 245203.2 | cpu: 47016.4 46633.3 55632.6 50175.3 43702.2 | med 47016.4 | load1 8.48..8.24 |

## Disk (du -sk of the store dir, KiB)
after first commit: git .git 62976
after first commit: vcs .vcs 84260
after first commit: sl .sl 772112
working tree: 749524 KiB (du -sk)

## Results (continued)
| status (clean)           | git  | wall: 946.8 980.4 946.6 969.4 975.1 | med 969.4 | cpu: 1368.9 1398.7 1373.3 1391.7 1396.6 | med 1391.7 | load1 2.63..2.75 |
| status (clean)           | vcs  | wall: 718.7 470.6 482.0 481.3 482.6 | med 482.0 | cpu: 3908.2 2316.5 2330.3 2351.1 2356.8 | med 2351.1 | load1 2.63..2.75 |
| status (clean)           | sl   | wall: 844.7 823.6 871.6 819.4 833.2 | med 833.2 | cpu: 3372.5 3357.3 3381.0 3359.4 3371.4 | med 3371.4 | load1 2.90..2.75 |
| status (1% modified)     | git  | wall: 1034.4 999.2 989.4 998.2 1010.3 | med 999.2 | cpu: 1451.1 1417.7 1413.5 1423.5 1436.0 | med 1423.5 | load1 2.55..3.56 |
| status (1% modified)     | vcs  | wall: 536.9 458.7 492.9 525.8 470.7 | med 492.9 | cpu: 2370.5 2280.4 2396.4 2290.8 2329.3 | med 2329.3 | load1 2.55..3.56 |
| status (1% modified)     | sl   | wall: 823.9 828.4 852.1 831.0 825.4 | med 828.4 | cpu: 3341.5 3351.2 3385.4 3361.2 3361.3 | med 3361.2 | load1 2.55..3.56 |
status line counts (1% modified): git=1000 vcs=1002 sl=1000 
history length git: 50 log lines (git/vcs: commits; sl: template lines)
history length vcs: 300 log lines (git/vcs: commits; sl: template lines)
history length sl: 50 log lines (git/vcs: commits; sl: template lines)
| log (50 commits)         | git  | wall: 6.1 6.1 6.0 6.0 6.0 | med 6.0 | cpu: 6.0 5.9 5.9 5.9 5.9 | med 5.9 | load1 2.67..2.53 |
| log (50 commits)         | vcs  | wall: 15.9 16.0 14.2 14.5 15.9 | med 15.9 | cpu: 15.7 15.7 14.0 14.4 15.7 | med 15.7 | load1 2.67..2.53 |
| log (50 commits)         | sl   | wall: 297.8 298.0 298.8 298.8 297.4 | med 298.0 | cpu: 298.7 298.9 299.6 299.7 298.2 | med 298.9 | load1 2.67..2.53 |
| checkout (1% differ)     | git  | wall: 481.5 496.8 483.9 481.7 485.5 | med 483.9 | cpu: 906.2 911.2 905.3 908.2 905.0 | med 906.2 | load1 2.53..4.28 |
| checkout (1% differ)     | vcs  | wall: 586.9 678.8 559.5 550.0 580.2 | med 580.2 | cpu: 1515.6 1528.1 1502.5 1506.9 1502.4 | med 1506.9 | load1 2.53..4.48 |
| checkout (1% differ)     | sl   | wall: 1480.9 1462.6 1467.8 1422.5 1492.7 | med 1467.8 | cpu: 4103.0 4070.4 4090.5 3999.6 4088.4 | med 4088.4 | load1 2.53..4.48 |
| clone (50 commits)       | git  | wall: 15892.3 15865.9 14449.5 17472.4 14074.9 | med 15865.9 | cpu: 8375.8 8430.5 8404.8 8400.5 8401.1 | med 8401.1 | load1 3.12..7.56 |
| clone (50 commits)       | vcs  | wall: 28186.6 30128.0 26766.4 26523.8 25861.8 | med 26766.4 | cpu: 17172.3 17241.4 17711.5 17191.0 18976.7 | med 17241.4 | load1 2.52..6.92 |
| clone (50 commits)       | sl   | wall: 80790.9 78797.9 75029.6 79263.5 79779.9 | med 79263.5 | cpu: 17865.3 17695.9 17658.7 19111.7 19004.1 | med 17865.3 | load1 4.37..6.05 |

## Disk after history and clone (KiB)
clone git: files=100000  du -sk total=813188
clone vcs: files=100000  du -sk total=833896
clone sl: files=100000  du -sk total=804588
store after 50 commits: git .git 87500
store after 50 commits: vcs .vcs 94124
store after 50 commits: sl .sl 801296
clone source src-git: 51992
clone source src-vcs: 56660
clone source src-sl: 51144

free disk at end: 53.29 GiB; load1 at end: 5.80
```

### binary

```text
## Environment
date:     2026-11-01T07:27:24Z
uname:    Linux ip-172-31-7-56 7.0.0-1014-aws #14~24.04.1-Ubuntu SMP PREEMPT Fri Sep 18 21:24:59 UTC 2026 x86_64 x86_64 x86_64 GNU/Linux
cpu:      AMD EPYC 7R32  cores=8  mem=31GiB  fs=ext4
git:      git version 2.43.0
vcs:      vcs 0.1.0 (/home/ubuntu/rt/slbench/bin/vcs)
sl:       Sapling 0.2.20261028-123539+7415fb2f (/home/ubuntu/rt/slbench/dl/x/sl)
dataset:  layout=binary files=100000 seed=42 (datagen default); runs per cell=5; WARM cache; tools=git vcs sl
load1 at start: 2.97   free disk: 56.52 GiB

## Results (ms; raw runs in run order, median, load1 sampled before each run: min..max)
| workflow | tool | wall ms | median | cpu ms | median | load |
|---|---|---|---|---|---|---|
| add (fresh index)        | git  | wall: 18742.3 18155.5 18115.8 18196.5 18079.4 | med 18155.5 | cpu: 18255.6 18053.9 18059.7 18138.7 18001.7 | med 18059.7 | load1 2.67..1.43 |
| add (fresh index)        | vcs  | wall: 6769.0 8642.4 5197.8 5195.2 7816.8 | med 6769.0 | cpu: 11896.8 10906.6 11567.5 11576.0 11375.2 | med 11567.5 | load1 2.19..1.34 |
| add (fresh index)        | sl   | wall: 8564.0 8525.6 8489.3 8168.3 8485.0 | med 8489.3 | cpu: 9090.6 9087.5 9084.3 8761.7 9041.3 | med 9084.3 | load1 2.18..1.47 |
| commit (all staged)      | git  | wall: 574.5 557.9 570.2 552.8 551.6 | med 557.9 | cpu: 736.9 724.1 733.2 716.8 714.6 | med 724.1 | load1 1.24..1.68 |
| commit (all staged)      | vcs  | wall: 436.4 529.3 445.5 441.3 329.4 | med 441.3 | cpu: 618.2 564.6 627.5 606.5 565.4 | med 606.5 | load1 2.68..2.68 |
| commit (all staged)      | sl   | wall: 34320.2 30596.2 31013.5 32002.4 30356.7 | med 31013.5 | cpu: 32173.4 31735.9 31413.1 31723.8 31533.5 | med 31723.8 | load1 3.09..2.42 |

## Disk (du -sk of the store dir, KiB)
after first commit: git .git 414964
after first commit: vcs .vcs 420924
after first commit: sl .sl 818152
working tree: 404032 KiB (du -sk)

## Results (continued)
| status (clean)           | git  | wall: 111.8 111.4 113.8 112.5 112.5 | med 112.5 | cpu: 280.0 280.7 277.9 279.4 279.7 | med 279.7 | load1 2.33..2.78 |
| status (clean)           | vcs  | wall: 158.0 141.4 142.5 140.8 140.9 | med 141.4 | cpu: 788.6 621.8 618.2 619.9 619.8 | med 619.9 | load1 2.33..2.33 |
| status (clean)           | sl   | wall: 265.5 270.2 267.1 268.0 268.6 | med 268.0 | cpu: 862.1 863.9 862.8 865.6 864.1 | med 863.9 | load1 2.33..2.33 |
| status (1% modified)     | git  | wall: 118.5 122.3 120.1 120.2 118.7 | med 120.1 | cpu: 286.5 288.9 286.4 286.8 288.0 | med 286.8 | load1 2.78..2.78 |
| status (1% modified)     | vcs  | wall: 141.1 141.3 142.2 141.9 141.3 | med 141.3 | cpu: 624.9 623.9 625.8 626.7 624.6 | med 624.9 | load1 2.78..2.78 |
| status (1% modified)     | sl   | wall: 266.6 271.0 270.5 269.9 272.4 | med 270.5 | cpu: 864.1 869.7 863.6 864.6 859.7 | med 864.1 | load1 2.78..2.78 |
status line counts (1% modified): git=1000 vcs=1002 sl=1000 
history length git: 50 log lines (git/vcs: commits; sl: template lines)
history length vcs: 300 log lines (git/vcs: commits; sl: template lines)
history length sl: 50 log lines (git/vcs: commits; sl: template lines)
| log (50 commits)         | git  | wall: 5.5 5.4 5.4 5.5 5.4 | med 5.4 | cpu: 5.4 5.2 5.3 5.3 5.3 | med 5.3 | load1 1.91..1.91 |
| log (50 commits)         | vcs  | wall: 10.8 10.7 10.0 10.8 10.8 | med 10.8 | cpu: 10.6 10.5 9.8 10.6 10.6 | med 10.6 | load1 1.91..1.91 |
| log (50 commits)         | sl   | wall: 298.3 297.4 297.8 296.3 297.5 | med 297.5 | cpu: 299.1 298.2 298.6 297.0 298.3 | med 298.3 | load1 1.91..1.91 |
| checkout (1% differ)     | git  | wall: 359.6 363.5 355.3 353.0 362.0 | med 359.6 | cpu: 521.0 527.8 519.9 520.3 524.1 | med 521.0 | load1 1.91..2.31 |
| checkout (1% differ)     | vcs  | wall: 496.1 502.6 500.0 522.1 519.0 | med 502.6 | cpu: 1031.8 1031.0 1031.8 1033.1 1032.5 | med 1031.8 | load1 1.91..2.31 |
| checkout (1% differ)     | sl   | wall: 996.2 922.2 984.1 993.0 938.3 | med 984.1 | cpu: 1617.5 1634.4 1628.1 1626.9 1612.4 | med 1626.9 | load1 1.91..2.31 |
| clone (50 commits)       | git  | wall: 9671.0 9812.0 9856.0 11164.5 9990.8 | med 9856.0 | cpu: 9914.6 10002.5 10077.4 10403.3 10288.4 | med 10077.4 | load1 2.73..1.94 |
| clone (50 commits)       | vcs  | wall: 9064.5 11986.9 9138.2 8740.5 11233.8 | med 9138.2 | cpu: 17196.2 17450.9 17252.1 17243.7 17228.3 | med 17243.7 | load1 2.61..1.78 |
| clone (50 commits)       | sl   | wall: 10412.2 10549.2 11039.5 10012.9 10004.0 | med 10412.2 | cpu: 14546.7 15032.2 14838.3 14507.3 14386.3 | med 14546.7 | load1 2.37..1.83 |

## Disk after history and clone (KiB)
clone git: files=100000  du -sk total=819888
clone vcs: files=100000  du -sk total=827412
clone sl: files=100000  du -sk total=814560
store after 50 commits: git .git 425764
store after 50 commits: vcs .vcs 430672
store after 50 commits: sl .sl 879604
clone source src-git: 408432
clone source src-vcs: 413816
clone source src-sl: 407968

free disk at end: 52.54 GiB; load1 at end: 1.84
```
