# Packing, deltas and placement experiments (issue #184, experiments A, C, E)

Part of epic #190. Status: **measured**; every number below is generated from the JSON-lines files in
`docs/research/storage/data/packing/` by `crates/vcs-storage-lab/tools/tables.py`. Nothing here changes a production
format: the prototype is the separate crate `crates/vcs-storage-lab` (its own Cargo workspace, not built by CI or by
`cargo build --workspace`). Companion document for experiments F and G (write buffers, compaction, tiering):
[compaction-experiments.md](compaction-experiments.md).

Evidence labels: **CONFIRMED** = measured here on the datasets listed, with the stated caveats. **HYPOTHESIS** = plausible,
not measured or not measured well enough. Negative results are listed explicitly in section 6.

## 1. Questions

The current pack (`crates/vcs-store/src/pack.rs`, `docs/object-model.md`) stores each object whole, sorted by id, with one
44-byte index entry per object and no compression or deltas. The questions:

1. **A.** How much would Git-style delta compression (window, depth, ordering) save, and what does it cost in reads?
2. **C.** Which bounded-depth, checkpoint or block constraint keeps most of the saving while bounding reconstruction work?
3. **E.** Which object ordering ("placement") minimises requests and wasted bytes for sparse (zone-subset) reads, static
   or adapted to the observed workload, and what does reorganising cost?

## 2. Method

**Lab.** `crates/vcs-storage-lab` (Rust, release build). Loads a real git history through the `git` binary
(`git log --reverse --topo-order --all --root -t -r --raw`, then `git cat-file --batch`), so objects are *git's* objects
(sha1 ids, git encodings), not allgit's. Content and history shape are real; identities and exact byte encodings differ.
It then implements, in about 2,500 lines: a git-format copy/insert delta encoder and decoder (`delta.rs`, round-trip
tested), delta-candidate search over a sliding window in a chosen order (`packlab.rs`), base selection under depth and
block constraints, per-object zstd-3 or zlib-6 compression, reconstruction with timing, and simulators for E, F and G.

**Zones.** The loader assigns an object to the zone of the first path component (the second as well when one top-level
directory holds more than 40 percent of the paths and splits into 4..200 sub-zones; root files go to `<root>`;
commit objects go to `<root>`). This is a *rule of this lab*, not allgit zone configuration. Counts are in the table below.

**Datasets** (all measured with the same code):

| dataset | commits | objects | blobs | raw MB | zones (lab rule) | HEAD objects |
|---|---|---|---|---|---|---|
| cargo | 24370 | 178393 | 57699 | 1452 | 93 | 4695 |
| gitgit | 85936 | 366460 | 137330 | 6626 | 1122 | 4762 |
| ripgrep | 2343 | 13869 | 5129 | 124 | 57 | 285 |
| sb_binary_heavy | 6 | 240 | 222 | 58 | 2 | 193 |
| sb_concurrent_publishing | 6 | 6471 | 6450 | 17 | 5 | 6404 |
| sb_deep_history | 301 | 3781 | 2200 | 5 | 82 | 1681 |
| sb_frequent_large_rewrites | 21 | 323 | 260 | 68 | 2 | 161 |
| sb_frequent_small_edits | 101 | 3376 | 2700 | 8 | 82 | 2481 |
| sb_game_assets | 6 | 628 | 602 | 221 | 4 | 596 |
| sb_highly_compressible | 6 | 166 | 148 | 39 | 2 | 129 |
| sb_incompressible | 6 | 156 | 138 | 36 | 2 | 129 |
| sb_large_monorepo | 11 | 35882 | 32200 | 38 | 9 | 35336 |
| sb_many_branches | 4 | 2548 | 2430 | 6 | 82 | 2481 |
| sb_many_similar | 6 | 16373 | 16100 | 18 | 162 | 16161 |
| sb_medium_app | 11 | 2067 | 1960 | 39 | 35 | 1955 |
| sb_ml_checkpoints | 6 | 58 | 40 | 180 | 2 | 26 |
| sb_repeated_across_zones | 11 | 2376 | 2320 | 5 | 5 | 7052 |
| sb_small_source | 11 | 8736 | 8100 | 18 | 22 | 8421 |
| sb_zone_subscription | 4 | 16141 | 16030 | 33 | 101 | 16100 |
| synth | 3001 | 38767 | 16737 | 774 | 41 | 7091 |

* `ripgrep`, `cargo`, `gitgit` are public repositories cloned bare on 2026-10-31 (https://github.com/BurntSushi/ripgrep,
  https://github.com/rust-lang/cargo, https://github.com/git/git; heads in the machine lines below). `gitgit` is git.git.
* `synth` is a synthetic multi-zone monorepo (`lab synth 40 3000 1`): 40 zones, 30..280 files each, Zipf-skewed zone and
  file popularity, text edits of 1..3 line ranges per file, 3 percent binary assets of 0.2..2 MB mutated in 4 KiB regions.
* `sb_*`: the 16 seeded workloads of the storebench baseline (docs/research/storage/workloads.md; generators in
  `crates/vcs-bench`), materialised as git history at scale 8 and seed 42 with `lab fixture`, base commit plus the
  workload's revisions (extra branches omitted). They satisfy "same datasets as the baseline"; their histories are short
  (3 to 300 commits), so the large repositories carry the analysis and the `sb_*` results are summarised in section 3.5.

**Known dataset limits.** The object set is built from the non-merge diffs of `git log`; objects that exist only in merge
results (evil merges) and annotated tag objects are not included. Our sets are therefore 87 to 98 percent of git's object
count (cargo 178,393 vs 195,147; gitgit 366,460 vs 422,883). Sizes are compared as totals, not per object, so the
comparison with `git repack` is approximate (section 3.1).

**Machine.** Lambda box `lambda-final`: AMD EPYC 7J13, 30 cores visible, Linux 6.8.0-1046-nvidia, rustc 1.99.1, git 2.34.1,
24 lab threads. The box is **shared**: load average was 7 to 22 during the runs (see the `meta` rows in the data files). Sizes
are deterministic and unaffected; **all timings (us, secs) are noisy, taken warm, in memory, one run of 3 repetitions
(median reported, min/max in the data), and are only comparable within one table.** No cold-disk timings were taken.

ripgrep: 13869 objects (2343 commits, 6397 trees, 5129 blobs), 124 MB raw, 2343 commits, 57 zones, head 285 objects; repo head 91f5d22ef48b; AMD EPYC 7J13 64-Core Processor, 30 cores, Linux 6.8.0-1046-nvidia, load at start 16.10, 11.76, 14.64, , git version 2.34.1, 24 lab threads, 2026-10-31T23:36:09Z.

cargo: 178393 objects (24370 commits, 96324 trees, 57699 blobs), 1452 MB raw, 24370 commits, 93 zones, head 4695 objects; repo head 809bcc525b61; AMD EPYC 7J13 64-Core Processor, 30 cores, Linux 6.8.0-1046-nvidia, load at start 18.25, 23.29, 20.85, , git version 2.34.1, 24 lab threads, 2026-10-31T23:23:12Z.

gitgit: 366460 objects (85936 commits, 143194 trees, 137330 blobs), 6626 MB raw, 85936 commits, 1122 zones, head 4762 objects; repo head 50fe5cc63f9c; AMD EPYC 7J13 64-Core Processor, 30 cores, Linux 6.8.0-1046-nvidia, load at start 5.11, 10.27, 13.96, , git version 2.34.1, 24 lab threads, 2026-10-31T23:49:05Z.

synth: 38767 objects (3001 commits, 19029 trees, 16737 blobs), 774 MB raw, 3001 commits, 41 zones, head 7091 objects; repo head f9fb10e5bbe0; AMD EPYC 7J13 64-Core Processor, 30 cores, Linux 6.8.0-1046-nvidia, load at start 6.45, 11.71, 16.33, , git version 2.34.1, 24 lab threads, 2026-10-31T23:29:54Z.

**Reproduce.** See section 8.

## 3. Results: Git-style packing (A)

Columns: *stored* = sum of stored object bytes (compressed full objects or compressed deltas; no pack framing or index);
*vs git 10/50* = stored divided by the stored size of the real `git repack -adf --window=10 --depth=50` (zlib, git 2.34.1,
size of the `.pack` files) on the same repository; *reads* = objects fetched to reconstruct one object (1 = stored whole);
*us* = decompress + apply time per object, warm, in memory. "Random" = 2000 random non-commit objects; "HEAD" = all objects
of the checked-out tree. Delta search is a window over a sorted order, always inside one object kind; base choice is greedy
in order with the depth bound; a delta is kept only if its compressed size beats the compressed whole object.

### 3.1 Baseline, window, ordering, whole-pack contrast

**gitgit** (git.git, 6.3 GB raw):

| variant | codec | stored MiB | stored/raw | vs git 10/50 | deltas | mean depth | max depth | reads per random obj | reads per HEAD obj | us per random obj | us per HEAD obj | build s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| GIT: repack --window=0 --depth=0 | zlib (git 2.34.1) | 3151.6 | 0.4988 | 22.51 | 0 | 0.00 | 0 |  |  |  |  | 353.2 |
| GIT: repack --window=10 --depth=50 | zlib (git 2.34.1) | 140.0 | 0.0222 | 1.00 | 320702 | 14.11 | 50 |  |  |  |  | 45.7 |
| no deltas (per-object compression) | zlib-6 | 2530.3 | 0.4004 | 18.07 | 0 | 0.00 | 0 | 1.0 | 1.0 | 82 | 32 | 12.5 |
| no deltas (per-object compression) | zstd-3 | 2635.3 | 0.4171 | 18.82 | 0 | 0.00 | 0 | 1.0 | 1.0 | 31 | 15 | 2.4 |
| delta git-like order window=4 depth<=50 | zstd-3 | 158.6 | 0.0251 | 1.13 | 266315 | 14.90 | 50 | 20.4 | 2.3 | 116 | 21 | 2.4 |
| delta git-like order window=10 depth<=50 | zlib-6 | 132.8 | 0.0210 | 0.95 | 268059 | 15.10 | 50 | 20.8 | 2.2 | 257 | 48 | 5.2 |
| delta git-like order window=10 depth<=50 | zstd-3 | 136.1 | 0.0215 | 0.97 | 268058 | 15.10 | 50 | 20.8 | 2.2 | 113 | 21 | 5.1 |
| delta git-like order window=25 depth<=50 | zstd-3 | 129.8 | 0.0205 | 0.93 | 269264 | 16.27 | 50 | 22.2 | 2.1 | 123 | 20 | 11.7 |
| delta git-like order window=50 depth<=50 | zstd-3 | 136.0 | 0.0215 | 0.97 | 269992 | 17.32 | 50 | 24.0 | 2.1 | 131 | 22 | 22.8 |
| delta window=10 depth<=50 order=path,commit asc | zstd-3 | 166.0 | 0.0263 | 1.19 | 268474 | 17.21 | 50 | 23.3 | 8.9 | 110 | 53 | 4.0 |
| delta window=10 depth<=50 order=path,commit desc (newest first) | zstd-3 | 158.3 | 0.0251 | 1.13 | 268939 | 16.80 | 50 | 22.3 | 2.1 | 105 | 19 | 4.6 |
| delta window=10 depth<=50 order=commit order | zstd-3 | 991.8 | 0.1570 | 7.08 | 162064 | 6.55 | 50 | 9.6 | 1.9 | 61 | 24 | 17.3 |
| delta window=10 depth<=50 order=size desc | zstd-3 | 717.2 | 0.1135 | 5.12 | 170904 | 8.56 | 50 | 11.9 | 1.3 | 96 | 31 | 14.9 |
| delta window=10 depth<=50 order=id order (control) | zstd-3 | 2397.8 | 0.3795 | 17.12 | 29938 | 0.11 | 9 | 1.2 | 1.0 | 34 | 15 | 18.5 |
| whole-pack zstd-3 long-window=128MiB order=git-like(kind,namehash,size desc) | zstd-3 stream | 61.6 | 0.0097 | 0.44 |  |  |  |  |  |  |  | 9.7 |
| whole-pack zstd-3 long-window=128MiB order=id order (control) | zstd-3 stream | 291.3 | 0.0461 | 2.08 |  |  |  |  |  |  |  | 23.3 |

**cargo**:

| variant | codec | stored MiB | stored/raw | vs git 10/50 | deltas | mean depth | max depth | reads per random obj | reads per HEAD obj | us per random obj | us per HEAD obj | build s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| GIT: repack --window=0 --depth=0 | zlib (git 2.34.1) | 379.9 | 0.2744 | 9.70 | 0 | 0.00 | 0 |  |  |  |  | 48.9 |
| GIT: repack --window=10 --depth=50 | zlib (git 2.34.1) | 39.2 | 0.0283 | 1.00 | 136441 | 7.30 | 50 |  |  |  |  | 8.7 |
| no deltas (per-object compression) | zlib-6 | 351.6 | 0.2539 | 8.98 | 0 | 0.00 | 0 | 1.0 | 1.0 | 29 | 15 | 2.1 |
| no deltas (per-object compression) | zstd-3 | 372.6 | 0.2691 | 9.51 | 0 | 0.00 | 0 | 1.0 | 1.0 | 12 | 7 | 0.5 |
| delta git-like order window=4 depth<=50 | zstd-3 | 41.3 | 0.0299 | 1.06 | 139742 | 12.65 | 50 | 15.0 | 5.7 | 58 | 21 | 0.6 |
| delta git-like order window=10 depth<=50 | zlib-6 | 36.1 | 0.0261 | 0.92 | 141324 | 12.36 | 50 | 15.0 | 5.6 | 131 | 52 | 1.4 |
| delta git-like order window=10 depth<=50 | zstd-3 | 36.6 | 0.0265 | 0.94 | 141327 | 12.37 | 50 | 15.0 | 5.6 | 58 | 20 | 1.3 |
| delta git-like order window=25 depth<=50 | zstd-3 | 35.5 | 0.0256 | 0.91 | 142400 | 12.72 | 50 | 15.1 | 5.6 | 59 | 21 | 3.0 |
| delta git-like order window=50 depth<=50 | zstd-3 | 37.2 | 0.0268 | 0.95 | 142876 | 13.03 | 50 | 15.8 | 5.5 | 64 | 24 | 5.8 |
| delta window=10 depth<=50 order=path,commit asc | zstd-3 | 40.3 | 0.0291 | 1.03 | 140792 | 14.90 | 50 | 17.8 | 5.7 | 61 | 22 | 0.9 |
| delta window=10 depth<=50 order=path,commit desc (newest first) | zstd-3 | 37.4 | 0.0270 | 0.96 | 140482 | 13.86 | 50 | 16.8 | 2.1 | 59 | 11 | 0.8 |
| delta window=10 depth<=50 order=commit order | zstd-3 | 248.8 | 0.1797 | 6.35 | 77077 | 2.82 | 50 | 4.2 | 4.2 | 22 | 20 | 3.1 |
| delta window=10 depth<=50 order=size desc | zstd-3 | 171.8 | 0.1240 | 4.39 | 106567 | 7.90 | 50 | 10.2 | 2.9 | 33 | 12 | 2.9 |
| delta window=10 depth<=50 order=id order (control) | zstd-3 | 363.7 | 0.2626 | 9.29 | 7267 | 0.05 | 6 | 1.1 | 1.1 | 14 | 8 | 3.3 |
| whole-pack zstd-3 long-window=128MiB order=git-like(kind,namehash,size desc) | zstd-3 stream | 21.1 | 0.0152 | 0.54 |  |  |  |  |  |  |  | 2.4 |
| whole-pack zstd-3 long-window=128MiB order=id order (control) | zstd-3 stream | 42.9 | 0.0310 | 1.09 |  |  |  |  |  |  |  | 4.3 |

**synth**:

| variant | codec | stored MiB | stored/raw | vs git 10/50 | deltas | mean depth | max depth | reads per random obj | reads per HEAD obj | us per random obj | us per HEAD obj | build s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| GIT: repack --window=0 --depth=0 | zlib (git 2.34.1) | 642.8 | 0.8708 | 2.54 | 0 | 0.00 | 0 |  |  |  |  | 4.9 |
| GIT: repack --window=10 --depth=50 | zlib (git 2.34.1) | 253.2 | 0.3430 | 1.00 | 24269 | 4.96 | 47 |  |  |  |  | 9.6 |
| no deltas (per-object compression) | zlib-6 | 643.8 | 0.8722 | 2.54 | 0 | 0.00 | 0 | 1.0 | 1.0 | 38 | 64 | 1.4 |
| no deltas (per-object compression) | zstd-3 | 639.2 | 0.8659 | 2.52 | 0 | 0.00 | 0 | 1.0 | 1.0 | 12 | 20 | 0.1 |
| delta git-like order window=4 depth<=50 | zstd-3 | 251.9 | 0.3413 | 0.99 | 27002 | 7.83 | 50 | 9.6 | 2.2 | 31 | 23 | 2.2 |
| delta git-like order window=10 depth<=50 | zlib-6 | 250.9 | 0.3399 | 0.99 | 27854 | 8.19 | 50 | 9.8 | 2.3 | 91 | 73 | 5.4 |
| delta git-like order window=10 depth<=50 | zstd-3 | 249.4 | 0.3378 | 0.98 | 27854 | 8.19 | 50 | 9.8 | 2.3 | 33 | 24 | 5.4 |
| delta git-like order window=25 depth<=50 | zstd-3 | 249.0 | 0.3373 | 0.98 | 28096 | 8.74 | 50 | 10.7 | 2.3 | 35 | 24 | 13.3 |
| delta git-like order window=50 depth<=50 | zstd-3 | 249.1 | 0.3375 | 0.98 | 28111 | 9.02 | 50 | 10.8 | 2.3 | 36 | 24 | 26.5 |
| delta window=10 depth<=50 order=path,commit asc | zstd-3 | 248.3 | 0.3364 | 0.98 | 28092 | 8.96 | 50 | 10.8 | 3.4 | 38 | 30 | 2.8 |
| delta window=10 depth<=50 order=path,commit desc (newest first) | zstd-3 | 248.2 | 0.3362 | 0.98 | 28095 | 8.98 | 50 | 10.9 | 1.0 | 38 | 19 | 2.9 |
| delta window=10 depth<=50 order=commit order | zstd-3 | 624.6 | 0.8461 | 2.47 | 3531 | 1.55 | 50 | 2.7 | 1.0 | 14 | 20 | 1.3 |
| delta window=10 depth<=50 order=size desc | zstd-3 | 287.0 | 0.3888 | 1.13 | 14376 | 3.72 | 50 | 4.8 | 1.3 | 20 | 21 | 8.4 |
| delta window=10 depth<=50 order=id order (control) | zstd-3 | 638.3 | 0.8648 | 2.52 | 379 | 0.01 | 2 | 1.0 | 1.0 | 12 | 20 | 1.2 |
| whole-pack zstd-3 long-window=128MiB order=git-like(kind,namehash,size desc) | zstd-3 stream | 235.7 | 0.3193 | 0.93 |  |  |  |  |  |  |  | 1.8 |
| whole-pack zstd-3 long-window=128MiB order=id order (control) | zstd-3 stream | 333.3 | 0.4515 | 1.32 |  |  |  |  |  |  |  | 2.4 |

**ripgrep**:

| variant | codec | stored MiB | stored/raw | vs git 10/50 | deltas | mean depth | max depth | reads per random obj | reads per HEAD obj | us per random obj | us per HEAD obj | build s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| GIT: repack --window=0 --depth=0 | zlib (git 2.34.1) | 31.2 | 0.2640 | 8.79 | 0 | 0.00 | 0 |  |  |  |  | 4.1 |
| GIT: repack --window=10 --depth=50 | zlib (git 2.34.1) | 3.6 | 0.0300 | 1.00 | 10166 | 7.26 | 49 |  |  |  |  | 0.7 |
| GIT: repack --window=250 --depth=250 | zlib (git 2.34.1) | 3.2 | 0.0272 | 0.91 | 10323 | 14.22 | 164 |  |  |  |  | 1.5 |
| no deltas (per-object compression) | zlib-6 | 31.1 | 0.2628 | 8.75 | 0 | 0.00 | 0 | 1.0 | 1.0 | 34 | 31 | 0.2 |
| no deltas (per-object compression) | zstd-3 | 32.6 | 0.2758 | 9.18 | 0 | 0.00 | 0 | 1.0 | 1.0 | 15 | 15 | 0.0 |
| delta git-like order window=4 depth<=50 | zstd-3 | 3.6 | 0.0306 | 1.02 | 10497 | 12.54 | 50 | 15.8 | 4.2 | 56 | 25 | 0.1 |
| delta git-like order window=10 depth<=50 | zlib-6 | 3.4 | 0.0283 | 0.94 | 10549 | 12.31 | 50 | 15.3 | 3.6 | 143 | 56 | 0.2 |
| delta git-like order window=10 depth<=50 | zstd-3 | 3.4 | 0.0289 | 0.96 | 10548 | 12.30 | 50 | 15.3 | 3.6 | 56 | 23 | 0.2 |
| delta git-like order window=25 depth<=50 | zstd-3 | 3.4 | 0.0283 | 0.94 | 10597 | 12.97 | 50 | 16.1 | 3.5 | 64 | 31 | 0.4 |
| delta git-like order window=50 depth<=50 | zstd-3 | 3.4 | 0.0287 | 0.96 | 10629 | 13.56 | 50 | 16.9 | 3.6 | 63 | 24 | 0.8 |
| delta window=10 depth<=50 order=path,commit asc | zstd-3 | 3.8 | 0.0323 | 1.07 | 10509 | 14.62 | 50 | 18.7 | 7.7 | 62 | 41 | 0.1 |
| delta window=10 depth<=50 order=path,commit desc (newest first) | zstd-3 | 3.5 | 0.0299 | 1.00 | 10530 | 14.10 | 50 | 18.2 | 1.5 | 59 | 20 | 0.1 |
| delta window=10 depth<=50 order=commit order | zstd-3 | 18.8 | 0.1590 | 5.30 | 6613 | 5.13 | 50 | 7.4 | 1.5 | 28 | 17 | 0.3 |
| delta window=10 depth<=50 order=size desc | zstd-3 | 8.8 | 0.0744 | 2.48 | 9296 | 9.19 | 50 | 12.0 | 3.5 | 39 | 21 | 0.2 |
| delta window=10 depth<=50 order=id order (control) | zstd-3 | 28.3 | 0.2394 | 7.97 | 2218 | 0.24 | 9 | 1.3 | 1.1 | 16 | 15 | 0.3 |
| whole-pack zstd-3 long-window=128MiB order=git-like(kind,namehash,size desc) | zstd-3 stream | 2.2 | 0.0183 | 0.61 |  |  |  |  |  |  |  | 0.3 |
| whole-pack zstd-3 long-window=128MiB order=id order (control) | zstd-3 stream | 2.4 | 0.0203 | 0.68 |  |  |  |  |  |  |  | 0.3 |

Reading the tables:

* **Delta compression is the whole story for history-heavy data. CONFIRMED.** Per-object compression alone (what a pack
  with zstd but no deltas would give) stores 372.6 MiB for cargo and 2.6 GB for git.git; deltas bring them to 36.6 MiB
  (10.2x) and 136 MiB (19.4x). On `synth`, which has large mutated binaries and mostly unrelated text, the gain is 2.6x.
* **The lab's delta engine is in the same range as git's. CONFIRMED, approximately.** With zlib-6, git-like order,
  window 10, depth 50 the lab stores 0.92 to 0.99 of the size of real `git repack` (ripgrep 0.94, synth 0.99, cargo 0.92,
  git.git 0.95). It is smaller, not larger, but its object sets are 2 to 13 percent smaller (section 2) and it ignores
  pack framing (about 4 bytes per entry plus a base reference per delta), so it validates the order of magnitude only. The 250/250 git reference was run only for ripgrep (3.2 vs 3.6 MiB for 10/50); on the large repositories it was skipped for time.
  Build times in the table are not a fair speed comparison (different code and features, shared machine); git's wall
  time for `repack -adf` is given only for reference.
* **Object order decides the saving. CONFIRMED.** On cargo, the same window 10 gives 36.6 MiB with git's order (kind, then
  name hash, then size descending), 37.4 MiB when ordered by path then commit descending, 40.3 MiB path then commit
  ascending, 172 MiB by size alone, 249 MiB in commit (arrival) order and 364 MiB in id order (the current pack's order),
  where deltas almost never find a similar neighbour. The current id-sorted pack layout is therefore *incompatible* with
  window-based delta search; deltas require a separate ordering step.
* **Window size has small returns past 10. CONFIRMED.** cargo: window 4 / 10 / 25 / 50 = 41.3 / 36.6 / 35.5 / 37.2 MiB
  (window 50 is slightly worse than 25 because the greedy selection with a depth bound picks locally smallest deltas).
  The extra search cost is linear in the window (build s column).
* **Newest-first storage makes HEAD reads cheap. CONFIRMED.** With path-then-commit-descending order the newest version is
  the base: HEAD objects need 2.1 reads on cargo, 2.1 on git.git and 1.0 on synth, versus 5.6 / 2.2 / 2.3 in git order and
  5.7 / 8.9 / 3.4 in oldest-first order, at a total size within 2 to 8 percent of git order.
* **Long-window whole-pack compression beats deltas, at the price of random access. CONFIRMED.** One zstd-3 stream with a
  128 MiB window over the git-ordered pack is 21.1 MiB on cargo (0.58x of delta), 61.6 MiB on git.git (0.45x) and 235.7 MiB on synth (0.94x). That is a
  *size bound*, not a design: reading one object means decompressing from a stream start. Section 4.4 measures how much of
  it survives when the stream is cut into independently readable blocks.

### 3.2 Delta depth distribution

The mean chain depth with depth bound 50 is 13.0 (cargo), 17.3 (git.git), 9.0 (synth), 13.6 (ripgrep); git's own packs
have mean 7.3 (cargo), 14.1 (git.git) and 5.0 (synth) with the same bound. The per-variant buckets are in the data
(`depth_hist_le0_1_2_4_8_16_32_inf`).

## 4. Results: bounded depth, checkpoints, blocks (C)

### 4.1 C1: depth bound, git-like order, window 50

**cargo**:

| variant | stored MiB | stored/raw | deltas | mean depth | max depth | reads/random | blocks/random | KiB read/random | us/random | reads/HEAD | KiB read/HEAD | us/HEAD | us/HEAD (cached bases) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| depth bound 0 | 372.6 | 0.2691 | 0 | 0.00 | 0 | 1.0 | 1.00 | 2.41 | 12 | 1.0 | 1.16 | 7 | 6 |
| depth bound 1 | 97.2 | 0.0702 | 138137 | 0.77 | 1 | 1.9 | 1.06 | 3.12 | 18 | 1.6 | 1.34 | 9 | 8 |
| depth bound 2 | 78.1 | 0.0564 | 140181 | 1.42 | 2 | 2.6 | 1.07 | 3.39 | 21 | 2.0 | 1.41 | 10 | 8 |
| depth bound 3 | 72.3 | 0.0522 | 141055 | 2.01 | 3 | 3.3 | 1.08 | 3.57 | 24 | 2.3 | 1.45 | 11 | 9 |
| depth bound 4 | 68.5 | 0.0495 | 141570 | 2.54 | 4 | 3.9 | 1.10 | 3.70 | 26 | 2.6 | 1.48 | 12 | 9 |
| depth bound 8 | 58.2 | 0.0421 | 142209 | 4.32 | 8 | 5.9 | 1.13 | 4.05 | 33 | 3.2 | 1.55 | 13 | 10 |
| depth bound 16 | 47.6 | 0.0344 | 142552 | 6.88 | 16 | 8.8 | 1.15 | 4.51 | 41 | 4.0 | 1.73 | 16 | 10 |
| depth bound 32 | 40.4 | 0.0292 | 142787 | 10.35 | 32 | 12.7 | 1.20 | 5.20 | 54 | 4.9 | 2.02 | 20 | 10 |
| depth bound 50 | 37.2 | 0.0268 | 142876 | 13.03 | 50 | 15.8 | 1.26 | 6.03 | 64 | 5.5 | 2.44 | 24 | 10 |

**gitgit**:

| variant | stored MiB | stored/raw | deltas | mean depth | max depth | reads/random | blocks/random | KiB read/random | us/random | reads/HEAD | KiB read/HEAD | us/HEAD | us/HEAD (cached bases) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| depth bound 0 | 2635.3 | 0.4171 | 0 | 0.00 | 0 | 1.0 | 1.00 | 9.49 | 32 | 1.0 | 2.57 | 14 | 15 |
| depth bound 1 | 408.0 | 0.0646 | 265139 | 0.72 | 1 | 1.9 | 1.20 | 11.24 | 42 | 1.5 | 2.93 | 19 | 18 |
| depth bound 2 | 331.1 | 0.0524 | 266857 | 1.38 | 2 | 2.8 | 1.27 | 11.79 | 46 | 1.7 | 3.02 | 19 | 19 |
| depth bound 3 | 294.0 | 0.0465 | 267649 | 2.00 | 3 | 3.6 | 1.35 | 12.50 | 51 | 1.8 | 3.11 | 19 | 19 |
| depth bound 4 | 270.9 | 0.0429 | 268121 | 2.57 | 4 | 4.4 | 1.42 | 12.84 | 55 | 1.9 | 3.13 | 20 | 20 |
| depth bound 8 | 228.0 | 0.0361 | 268838 | 4.55 | 8 | 7.0 | 1.65 | 13.63 | 66 | 2.0 | 3.21 | 20 | 20 |
| depth bound 16 | 184.7 | 0.0292 | 269410 | 7.76 | 16 | 11.3 | 1.99 | 14.99 | 82 | 2.1 | 3.41 | 21 | 21 |
| depth bound 32 | 155.3 | 0.0246 | 269811 | 12.76 | 32 | 17.3 | 2.42 | 17.34 | 108 | 2.1 | 3.46 | 22 | 23 |
| depth bound 50 | 136.0 | 0.0215 | 269992 | 17.32 | 50 | 24.0 | 2.93 | 19.20 | 129 | 2.1 | 3.47 | 22 | 22 |

* **A depth bound trades size against reads smoothly; there is no free lunch. CONFIRMED.** git.git: bound 1 costs 3.0x the
  size of bound 50 (408 vs 136 MiB) for 1.9 reads per random object; bound 4 costs 2.0x for 4.4 reads; bound 8 costs 1.7x
  for 7.0 reads; bound 16 1.36x for 11.3; bound 32 1.14x for 17.3; bound 50 needs 24.0 reads per random object. cargo:
  bound 4 is 1.84x (68.5 vs 37.2 MiB), bound 8 1.56x, bound 16 1.28x, bound 32 1.09x.
* **HEAD reads stay cheap at any bound in git order: 1.5 to 5.5 reads.** CONFIRMED (cargo 2.6 reads at bound 4 and 5.5 at 50; git.git 1.9 and 2.1).
* **CPU cost is small relative to a network round trip. CONFIRMED for this warm in-memory measure only.** Reconstructing a
  random git.git object at bound 50 is 129 us (24 reads) against 32 us for a whole object; HEAD objects 22 us vs 14 us.
  Each *read* would be a ranged GET in a remote pack; the dominant cost on object storage is the number of reads (and
  whether they are in one block), not the zstd and delta-apply CPU. [HYPOTHESIS for remote: not measured against a real
  or emulated object store.]
* **A per-checkout cache of reconstructed bases removes most of the HEAD chain cost.** `us/HEAD (cached bases)` stays at
  6 to 10 us for cargo across bounds (bases shared between HEAD objects are reconstructed once).

### 4.2 C2: file-history chains with checkpoints (blobs only)

Each path's versions are one chain; a *checkpoint* is a version stored whole. Strategies: whole object every k versions,
single chain, and skip-deltas (each version deltas against the version obtained by clearing its lowest set bit,
Subversion-style, depth at most log2 n). Both directions: oldest-first (old version is the base) and newest-first (the
newest is the base). Sizes are blobs only (trees and commits are stored whole in this study).

**cargo**:

| variant | stored MiB (blobs only) |  | deltas | mean depth | max depth | reads/random | blocks/random | KiB read/random | us/random | reads/HEAD | KiB read/HEAD | us/HEAD | us/HEAD (cached bases) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| chain with full checkpoint every 1 versions; oldest-first (forward deltas) | 305.2 |  | 0 | 0.00 | 0 | 1.0 | 1.00 | 2.43 | 12 | 1.0 | 1.16 | 7 | 6 |
| chain with full checkpoint every 2 versions; oldest-first (forward deltas) | 161.4 |  | 27336 | 0.15 | 1 | 1.2 | 1.00 | 2.46 | 13 | 1.2 | 1.18 | 7 | 7 |
| chain with full checkpoint every 4 versions; oldest-first (forward deltas) | 89.6 |  | 40914 | 0.45 | 3 | 1.5 | 1.00 | 2.51 | 15 | 1.6 | 1.21 | 9 | 8 |
| chain with full checkpoint every 8 versions; oldest-first (forward deltas) | 54.0 |  | 47497 | 1.00 | 7 | 2.2 | 1.01 | 2.60 | 17 | 2.0 | 1.26 | 10 | 10 |
| chain with full checkpoint every 16 versions; oldest-first (forward deltas) | 36.2 |  | 50697 | 2.01 | 15 | 3.4 | 1.01 | 2.80 | 22 | 2.7 | 1.33 | 12 | 13 |
| chain with full checkpoint every 64 versions; oldest-first (forward deltas) | 23.2 |  | 52807 | 6.52 | 63 | 8.6 | 1.05 | 3.76 | 46 | 4.1 | 1.52 | 17 | 21 |
| single chain (checkpoint only at first version); oldest-first (forward deltas) | 20.0 |  | 53174 | 18.21 | 688 | 22.1 | 1.15 | 6.57 | 113 | 5.5 | 1.81 | 25 | 39 |
| skip-delta (base = version with lowest set bit cleared); oldest-first (forward deltas) | 33.8 |  | 52570 | 0.80 | 9 | 1.9 | 1.10 | 3.32 | 22 | 1.7 | 1.38 | 10 | 11 |
| chain with full checkpoint every 1 versions; newest-first (reverse deltas) | 305.2 |  | 0 | 0.00 | 0 | 1.0 | 1.00 | 2.40 | 12 | 1.0 | 1.16 | 7 | 7 |
| chain with full checkpoint every 2 versions; newest-first (reverse deltas) | 160.4 |  | 27335 | 0.15 | 1 | 1.2 | 1.00 | 2.43 | 13 | 1.0 | 1.17 | 7 | 7 |
| chain with full checkpoint every 4 versions; newest-first (reverse deltas) | 88.2 |  | 40946 | 0.45 | 3 | 1.5 | 1.00 | 2.48 | 14 | 1.0 | 1.17 | 7 | 7 |
| chain with full checkpoint every 8 versions; newest-first (reverse deltas) | 52.3 |  | 47529 | 1.00 | 7 | 2.2 | 1.01 | 2.60 | 17 | 1.0 | 1.17 | 7 | 6 |
| chain with full checkpoint every 16 versions; newest-first (reverse deltas) | 34.5 |  | 50728 | 2.01 | 15 | 3.4 | 1.01 | 2.82 | 21 | 1.0 | 1.17 | 7 | 6 |
| chain with full checkpoint every 64 versions; newest-first (reverse deltas) | 21.7 |  | 52834 | 6.55 | 63 | 8.7 | 1.06 | 3.96 | 44 | 1.0 | 1.17 | 7 | 6 |
| single chain (checkpoint only at first version); newest-first (reverse deltas) | 19.1 |  | 53200 | 18.15 | 688 | 22.4 | 1.22 | 7.20 | 111 | 1.0 | 1.17 | 7 | 7 |
| skip-delta (base = version with lowest set bit cleared); newest-first (reverse deltas) | 27.1 |  | 53036 | 0.86 | 10 | 2.0 | 1.15 | 4.25 | 25 | 1.0 | 1.17 | 7 | 6 |

**gitgit**:

| variant | stored MiB (blobs only) |  | deltas | mean depth | max depth | reads/random | blocks/random | KiB read/random | us/random | reads/HEAD | KiB read/HEAD | us/HEAD | us/HEAD (cached bases) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| chain with full checkpoint every 1 versions; oldest-first (forward deltas) | 1223.1 |  | 0 | 0.00 | 0 | 1.0 | 1.00 | 9.00 | 29 | 1.0 | 2.57 | 14 | 15 |
| chain with full checkpoint every 2 versions; oldest-first (forward deltas) | 645.4 |  | 66380 | 0.18 | 1 | 1.2 | 1.00 | 9.06 | 30 | 1.4 | 2.63 | 16 | 17 |
| chain with full checkpoint every 4 versions; oldest-first (forward deltas) | 356.1 |  | 99438 | 0.53 | 3 | 1.7 | 1.00 | 9.20 | 33 | 2.0 | 2.70 | 18 | 20 |
| chain with full checkpoint every 8 versions; oldest-first (forward deltas) | 212.3 |  | 115756 | 1.21 | 7 | 2.7 | 1.01 | 9.46 | 38 | 2.9 | 2.85 | 21 | 24 |
| chain with full checkpoint every 16 versions; oldest-first (forward deltas) | 140.2 |  | 123690 | 2.46 | 15 | 4.3 | 1.02 | 9.97 | 47 | 4.3 | 3.13 | 29 | 32 |
| chain with full checkpoint every 64 versions; oldest-first (forward deltas) | 86.3 |  | 129125 | 8.37 | 63 | 11.8 | 1.09 | 12.00 | 83 | 8.1 | 4.32 | 46 | 60 |
| single chain (checkpoint only at first version); oldest-first (forward deltas) | 71.3 |  | 130227 | 41.04 | 1522 | 52.2 | 1.63 | 31.06 | 357 | 14.4 | 6.15 | 82 | 121 |
| skip-delta (base = version with lowest set bit cleared); oldest-first (forward deltas) | 123.5 |  | 129092 | 1.05 | 10 | 2.4 | 1.21 | 10.65 | 49 | 2.3 | 3.42 | 24 | 28 |
| chain with full checkpoint every 1 versions; newest-first (reverse deltas) | 1223.1 |  | 0 | 0.00 | 0 | 1.0 | 1.00 | 8.71 | 29 | 1.0 | 2.57 | 14 | 15 |
| chain with full checkpoint every 2 versions; newest-first (reverse deltas) | 642.1 |  | 66385 | 0.18 | 1 | 1.2 | 1.00 | 8.77 | 30 | 1.1 | 2.58 | 15 | 16 |
| chain with full checkpoint every 4 versions; newest-first (reverse deltas) | 351.6 |  | 99431 | 0.53 | 3 | 1.7 | 1.00 | 8.94 | 32 | 1.1 | 2.59 | 16 | 16 |
| chain with full checkpoint every 8 versions; newest-first (reverse deltas) | 205.9 |  | 115766 | 1.21 | 7 | 2.7 | 1.02 | 9.21 | 43 | 1.2 | 2.60 | 15 | 16 |
| chain with full checkpoint every 16 versions; newest-first (reverse deltas) | 133.6 |  | 123699 | 2.46 | 15 | 4.3 | 1.03 | 9.73 | 45 | 1.2 | 2.61 | 17 | 17 |
| chain with full checkpoint every 64 versions; newest-first (reverse deltas) | 80.7 |  | 129129 | 8.36 | 63 | 11.9 | 1.10 | 12.31 | 89 | 1.2 | 2.61 | 15 | 16 |
| single chain (checkpoint only at first version); newest-first (reverse deltas) | 67.5 |  | 130228 | 41.02 | 1522 | 53.2 | 1.92 | 35.02 | 391 | 1.2 | 2.61 | 15 | 16 |
| skip-delta (base = version with lowest set bit cleared); newest-first (reverse deltas) | 97.8 |  | 129913 | 1.14 | 11 | 2.5 | 1.31 | 12.56 | 54 | 1.1 | 2.61 | 15 | 16 |

* **Checkpoint spacing is the depth bound in disguise: size falls and reads rise together. CONFIRMED.** git.git blobs:
  checkpoint every 4 versions = 356 MiB at 1.7 reads/random object; every 16 = 140 MiB at 4.3; every 64 = 86 MiB at 11.8;
  one unbounded chain = 71 MiB at 52 reads (max chain 1522).
* **Skip-deltas bound the worst case at a size premium. CONFIRMED.** Max depth 10 to 11 with mean 1.05, 2.4 reads, 123 MiB
  (oldest-first) versus the periodic checkpoint every 16 (140 MiB, 4.3 reads, max 15): on these histories skip-deltas are
  *smaller* than an equally-read-cheap periodic scheme (every 8 = 212 MiB at 2.7 reads) and need no tuning, but the
  unbounded chain is still 1.7x smaller.
* **Direction matters for HEAD, not for history. CONFIRMED.** Newest-first gives HEAD blobs exactly 1.0 read at any
  spacing (1.0 on cargo, 1.1 to 1.2 on git.git where a few HEAD blobs are not the newest version of their path) whereas oldest-first needs 14.4 reads for an unbounded chain. Random-version reads
  are the same in both directions. Newest-first costs slightly less total size here (git.git every 16: 133.6 vs 140.2 MiB).
  The price: appending a new version to a newest-first chain rewrites the previous newest as a delta (a write-time cost
  not measured here). [HYPOTHESIS: acceptable if done during compaction, not on the publish path.]

### 4.3 C3: deltas only inside a block versus across blocks

A block is a run of consecutive objects in git order whose whole compressed sizes add up to the stated size (the
*logical* block); after deltas the physical block is smaller (column "mean physical block KiB"). "Reads/blocks" counts
the distinct blocks a reconstruction must fetch.

**cargo**:

| variant | stored MiB | stored/raw | cross-block deltas | mean physical block KiB | deltas | mean depth | max depth | reads/random | blocks/random | KiB read/random | us/random | reads/HEAD | KiB read/HEAD | us/HEAD | us/HEAD (cached bases) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| block 256 KiB cross-block deltas allowed | 37.2 | 0.0268 | 7586 | 26 | 142876 | 13.03 | 50 | 15.8 | 1.95 | 6.03 | 62 | 5.5 | 2.44 | 24 | 10 |
| block 256 KiB in-block deltas only | 51.4 | 0.0371 | 0 | 36 | 141380 | 8.68 | 50 | 10.9 | 1.00 | 3.74 | 37 | 3.8 | 1.61 | 15 | 10 |
| block 1024 KiB cross-block deltas allowed | 37.2 | 0.0268 | 1987 | 103 | 142876 | 13.03 | 50 | 15.8 | 1.26 | 6.03 | 64 | 5.5 | 2.44 | 24 | 11 |
| block 1024 KiB in-block deltas only | 38.2 | 0.0276 | 0 | 106 | 142514 | 11.21 | 50 | 13.8 | 1.00 | 4.59 | 48 | 5.7 | 2.28 | 23 | 10 |
| block 4096 KiB cross-block deltas allowed | 37.2 | 0.0268 | 555 | 409 | 142876 | 13.03 | 50 | 15.8 | 1.06 | 6.03 | 65 | 5.5 | 2.44 | 24 | 10 |
| block 4096 KiB in-block deltas only | 36.1 | 0.0261 | 0 | 397 | 142790 | 12.53 | 50 | 15.3 | 1.00 | 5.40 | 57 | 5.4 | 2.41 | 24 | 10 |
| block 16384 KiB cross-block deltas allowed | 37.2 | 0.0268 | 143 | 1585 | 142876 | 13.03 | 50 | 15.8 | 1.01 | 6.03 | 65 | 5.5 | 2.44 | 26 | 10 |
| block 16384 KiB in-block deltas only | 37.2 | 0.0269 | 0 | 1588 | 142856 | 12.88 | 50 | 15.6 | 1.00 | 6.01 | 63 | 5.4 | 2.44 | 24 | 10 |

**gitgit**:

| variant | stored MiB | stored/raw | cross-block deltas | mean physical block KiB | deltas | mean depth | max depth | reads/random | blocks/random | KiB read/random | us/random | reads/HEAD | KiB read/HEAD | us/HEAD | us/HEAD (cached bases) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| block 256 KiB cross-block deltas allowed | 136.0 | 0.0215 | 64968 | 14 | 269992 | 17.32 | 50 | 24.0 | 6.02 | 19.20 | 131 | 2.1 | 3.47 | 22 | 22 |
| block 256 KiB in-block deltas only | 356.9 | 0.0565 | 0 | 36 | 259574 | 4.44 | 50 | 7.0 | 1.00 | 10.89 | 52 | 2.0 | 2.80 | 18 | 18 |
| block 1024 KiB cross-block deltas allowed | 136.0 | 0.0215 | 23774 | 53 | 269992 | 17.32 | 50 | 24.0 | 2.93 | 19.20 | 136 | 2.1 | 3.47 | 22 | 21 |
| block 1024 KiB in-block deltas only | 179.5 | 0.0284 | 0 | 71 | 267432 | 8.64 | 50 | 12.7 | 1.00 | 12.64 | 77 | 2.0 | 2.92 | 19 | 19 |
| block 4096 KiB cross-block deltas allowed | 136.0 | 0.0215 | 6109 | 212 | 269992 | 17.32 | 50 | 24.0 | 1.50 | 19.20 | 131 | 2.1 | 3.47 | 22 | 21 |
| block 4096 KiB in-block deltas only | 135.7 | 0.0215 | 0 | 212 | 269476 | 13.89 | 50 | 18.9 | 1.00 | 15.24 | 99 | 2.1 | 3.10 | 20 | 20 |
| block 16384 KiB cross-block deltas allowed | 136.0 | 0.0215 | 1709 | 844 | 269992 | 17.32 | 50 | 24.0 | 1.12 | 19.20 | 129 | 2.1 | 3.47 | 22 | 22 |
| block 16384 KiB in-block deltas only | 133.7 | 0.0212 | 0 | 830 | 269879 | 16.38 | 50 | 22.5 | 1.00 | 18.14 | 126 | 2.1 | 3.40 | 21 | 21 |

* **In-block-only deltas make a read touch exactly one block, at a size penalty that vanishes above a few MiB. CONFIRMED.**
  git.git: 256 KiB logical blocks cost 2.6x the size of unrestricted deltas (357 vs 136 MiB); 1 MiB 1.32x; 4 MiB 1.00x
  (135.7 vs 136.0 MiB); 16 MiB 0.98x. cargo: 1.38x, 1.03x, 0.97x, 1.00x for the same sizes. Unrestricted deltas touch 2.9
  (git.git) or 1.3 (cargo) blocks per random read at 1 MiB logical blocks; in-block-only touches 1.00. Because mean physical
  blocks are 10 to 20 times smaller than the logical size on these histories (212 KiB physical for 4 MiB logical on git.git,
  409 KiB on cargo), the *physical* block that must be fetched is only a few hundred KiB.
* **In-block-only also shortens chains (mean depth 13.9 vs 17.3 and 18.9 vs 24.0 reads per random object on git.git at 4 MiB), so reads per object fall at
  no size cost. CONFIRMED.** The mechanism is that bases outside the block are no longer available, which removes the
  longest chains; it is not a general property and depends on the git-like order putting related versions close.

### 4.4 C4: how much of whole-pack compression survives independent blocks (no deltas)

Objects in git order are cut into blocks of a given *raw* size and each block is compressed independently (zstd, optional
long-distance matching, window up to 128 MiB, but only within the block). Reading one object decompresses its whole block.

**cargo**:

| blocks | stored MiB | stored/raw | MiB decompressed per random read (expected) | compress s (wall, 24 threads) |
|---|---|---|---|---|
| zstd-3 blocks of 256 KiB raw | 89.6 | 0.0647 | 0.26 | 1.0 |
| zstd-3 +ldm blocks of 256 KiB raw | 87.0 | 0.0628 | 0.26 | 6.4 |
| zstd-19 +ldm blocks of 256 KiB raw | 75.2 | 0.0543 | 0.26 | 103.4 |
| zstd-3 blocks of 1024 KiB raw | 43.7 | 0.0315 | 1.01 | 0.4 |
| zstd-3 +ldm blocks of 1024 KiB raw | 41.4 | 0.0299 | 1.01 | 1.8 |
| zstd-19 +ldm blocks of 1024 KiB raw | 35.9 | 0.0259 | 1.01 | 30.2 |
| zstd-3 blocks of 4096 KiB raw | 29.3 | 0.0211 | 3.97 | 0.5 |
| zstd-3 +ldm blocks of 4096 KiB raw | 27.0 | 0.0195 | 3.97 | 1.1 |
| zstd-19 +ldm blocks of 4096 KiB raw | 23.3 | 0.0168 | 3.97 | 24.2 |
| zstd-3 blocks of 16384 KiB raw | 25.2 | 0.0182 | 15.04 | 1.4 |
| zstd-3 +ldm blocks of 16384 KiB raw | 22.8 | 0.0165 | 15.04 | 2.5 |
| zstd-19 +ldm blocks of 16384 KiB raw | 19.5 | 0.0141 | 15.04 | 54.2 |
| zstd-3 blocks of 65536 KiB raw | 24.1 | 0.0174 | 54.25 | 2.1 |
| zstd-3 +ldm blocks of 65536 KiB raw | 21.6 | 0.0156 | 54.25 | 3.4 |

**gitgit**:

| blocks | stored MiB | stored/raw | MiB decompressed per random read (expected) | compress s (wall, 24 threads) |
|---|---|---|---|---|
| zstd-3 blocks of 256 KiB raw | 620.0 | 0.0981 | 0.26 | 6.4 |
| zstd-3 +ldm blocks of 256 KiB raw | 617.1 | 0.0977 | 0.26 | 31.7 |
| zstd-19 +ldm blocks of 256 KiB raw | 533.2 | 0.0844 | 0.26 | 437.0 |
| zstd-3 blocks of 1024 KiB raw | 260.0 | 0.0411 | 1.01 | 2.5 |
| zstd-3 +ldm blocks of 1024 KiB raw | 256.2 | 0.0406 | 1.01 | 9.2 |
| zstd-19 +ldm blocks of 1024 KiB raw | 216.2 | 0.0342 | 1.01 | 128.6 |
| zstd-3 blocks of 4096 KiB raw | 128.8 | 0.0204 | 3.99 | 1.7 |
| zstd-3 +ldm blocks of 4096 KiB raw | 124.0 | 0.0196 | 3.99 | 3.3 |
| zstd-19 +ldm blocks of 4096 KiB raw | 100.4 | 0.0159 | 3.99 | 71.3 |
| zstd-3 blocks of 16384 KiB raw | 84.2 | 0.0133 | 15.52 | 1.8 |
| zstd-3 +ldm blocks of 16384 KiB raw | 78.3 | 0.0124 | 15.52 | 3.1 |
| zstd-19 +ldm blocks of 16384 KiB raw | 60.5 | 0.0096 | 15.52 | 105.4 |
| zstd-3 blocks of 65536 KiB raw | 72.3 | 0.0114 | 59.17 | 6.0 |
| zstd-3 +ldm blocks of 65536 KiB raw | 65.7 | 0.0104 | 59.17 | 10.0 |

* **Block compression without any delta code beats the object-level delta pack at 4 MiB blocks and above on text-like
  histories. CONFIRMED on cargo, git.git and ripgrep.** cargo: 1 MiB blocks (zstd-3 +ldm) = 41.4 MiB (13% larger than the 36.6 MiB
  delta pack), 4 MiB = 27.0 MiB (26% smaller), against 21.1 MiB for the unbounded stream; git.git: 1 MiB = 256 MiB (1.9x the
  136 MiB delta pack), 4 MiB = 124 MiB (9% smaller), 16 MiB = 78 MiB, unbounded 61.6 MiB; zstd-19 +ldm at 4 MiB = 23.3 MiB. The cost is read
  granularity: a random read decompresses 4.2 MiB of raw data (expected) instead of about 6 KiB, and compressing at level 19
  takes 24 s versus 1 s. On `synth`, where the content is mutated random binary, blocks do *not* help (323 vs 249 MiB for
  deltas at 4 MiB), because the redundancy is long-range within a file, which windowed text compression cannot see unless
  the block holds both versions.
* **HYPOTHESIS:** a hybrid (object-level deltas inside the block plus block compression) or a trained dictionary per zone
  would combine both; not built here.

### 4.5 Storebench baseline workloads (`sb_*`)

| workload (scale 8) | commits | raw MiB | git window=0 | git 10/50 | ours per-object zstd | ours delta W10 D50 zstd (git order) | ours delta, path newest-first | whole-pack zstd long | mean depth | reads/HEAD obj |
|---|---|---|---|---|---|---|---|---|---|---|
| binary_heavy | 6 | 55.5 | 55.5 | 48.5 | 55.5 | 48.5 | 48.5 | 48.5 | 0.2 | 1.2 |
| concurrent_publishing | 6 | 16.2 | 5.5 | 5.3 | 5.8 | 5.6 | 5.6 | 3.9 | 0.0 | 1.0 |
| deep_history | 301 | 5.0 | 2.3 | 1.2 | 2.3 | 1.2 | 1.2 | 0.8 | 3.2 | 1.4 |
| frequent_large_rewrites | 21 | 65.1 | 19.0 | 18.9 | 20.2 | 20.2 | 20.2 | 19.2 | 0.7 | 1.1 |
| frequent_small_edits | 101 | 7.2 | 2.6 | 2.0 | 2.7 | 2.1 | 2.1 | 1.5 | 1.1 | 1.2 |
| game_assets | 6 | 210.4 | 186.8 | 186.8 | 205.1 | 205.1 | 205.1 | 202.9 | 0.0 | 1.0 |
| highly_compressible | 6 | 37.0 | 0.3 | 0.3 | 0.3 | 0.3 | 0.3 | 0.2 | 0.2 | 1.2 |
| incompressible | 6 | 34.5 | 34.5 | 34.5 | 34.5 | 34.5 | 34.5 | 34.5 | 0.1 | 1.0 |
| large_monorepo | 11 | 36.0 | 14.6 | 14.5 | 15.6 | 15.4 | 15.4 | 9.0 | 0.0 | 1.0 |
| many_branches | 4 | 6.1 | 2.0 | 2.0 | 2.1 | 2.1 | 2.1 | 1.5 | 0.0 | 1.0 |
| many_similar | 6 | 16.7 | 11.0 | 2.8 | 11.1 | 2.7 | 2.7 | 1.2 | 26.0 | 27.2 |
| medium_app | 11 | 37.1 | 24.4 | 24.3 | 27.4 | 27.2 | 27.2 | 25.8 | 0.1 | 1.0 |
| ml_checkpoints | 6 | 172.0 | 161.3 | 103.8 | 161.2 | 103.9 | 103.9 | 99.7 | 0.6 | 1.8 |
| repeated_across_zones | 11 | 5.2 | 2.3 | 1.8 | 2.3 | 1.9 | 1.9 | 1.4 | 0.1 | 1.0 |
| small_source | 11 | 17.1 | 5.9 | 5.8 | 6.3 | 6.1 | 6.1 | 4.2 | 0.1 | 1.0 |
| zone_subscription | 4 | 31.9 | 11.2 | 11.2 | 11.9 | 11.8 | 11.8 | 7.9 | 0.0 | 1.0 |

These are the 16 seeded baseline workloads at scale 8 as git history (3 to 300 revisions; the full A/C/E/F/G records are in
`data/packing/sb_*.jsonl`). What the table shows:

* **Deltas matter where many near-duplicate objects exist, and are irrelevant elsewhere. CONFIRMED.** `many_similar`:
  per-object 11.1 MiB, delta 2.7 MiB (4.1x), but with mean chain depth 26 and 27 reads per HEAD object (the 2000 similar
  files form long chains in git order; a depth bound is needed there). `ml_checkpoints`: 161.2 to 103.9 MiB (1.55x, with
  1.8 reads/HEAD). `deep_history` and `frequent_small_edits`: 2.3 to 1.2 and 2.7 to 2.1 MiB (1.9x and 1.3x).
* **Random or already-dense content gains nothing from any scheme. CONFIRMED.** `incompressible` 34.5 MiB in every column;
  `game_assets` 205.1 per-object vs 205.1 delta (git itself without deltas: 186.8, 10 percent smaller: a codec difference,
  not a delta effect); `frequent_large_rewrites` 20.2 vs 20.2 (whole-file rewrites leave no common content).
* **`highly_compressible` is a codec result (0.3 MiB from 37 MiB), not a delta result.**
* **The lab matches `git repack` on these small sets within the codec difference** (zstd-3 per-object is 5 to 12 percent
  larger than git's zlib-6 with no deltas for `small_source`, `medium_app`, `zone_subscription`, `large_monorepo`; with
  deltas the gap stays the same, so it is the codec, not the delta search).
* **Whole-pack long-window zstd is smaller than deltas on 14 of 16 workloads**, and equal on the two incompressible ones (see
  4.4 for why that is not a design).

Short histories (3 to 10 revisions) leave little for deltas to find in most of these workloads, which is why the
large real repositories carry the analysis.

## 5. Results: placement (E)

Setup. Objects keep their compressed size (zstd-3, no deltas; the delta/placement interaction is not modelled). A layout is
an order of all objects, concatenated into one address space. A request is the set of objects it needs; the cost is the
number of ranged GETs after merging reads whose gap is at most 256 KiB, and the bytes fetched divided by the bytes
needed (*read amplification*; 1.0 is perfect). The cost-equivalent metric used for E2 is bytes + 5 MB per GET, i.e. 1 GET
is worth 5 MB of transfer (50 ms at 100 MB/s): **a model parameter, not a measurement**. Workloads: sparse checkout of
the HEAD objects of k zones chosen uniformly (1/5/10/50 percent of zones, at least 1), a full history fetch of the same
zones, the history of one file, and an incremental pull of 20 consecutive commits.

### 5.1 E1: static policies

Each cell is "GETs / read amplification", mean of 40 trials (200 for file history, 100 for incremental pull).

**gitgit** - sparse checkout of HEAD:

| layout (GETs / read amp) | 1% zones (k=1) | 5% zones (k=2) | 10% zones (k=3) | 50% zones (k=17) |
|---|---|---|---|---|
| id (hash) order [current pack] | 95.5 / 1.5x | 241.6 / 3.4x | 289.8 / 3.4x | 1680.6 / 10.9x |
| commit (arrival) order | 52.3 / 21.4x | 95.4 / 6.9x | 134.7 / 6.8x | 714.4 / 8.0x |
| zone, commit | 25.3 / 13.9x | 55.3 / 11.2x | 68.8 / 13.5x | 476.7 / 10.0x |
| zone, directory, commit | 12.1 / 8.1x | 23.3 / 11.1x | 34.9 / 12.1x | 204.4 / 7.6x |
| zone, path, commit (file history) | 12.1 / 8.2x | 23.6 / 10.9x | 35.3 / 11.7x | 206.0 / 7.5x |
| zone, head-first, path, commit | 1.0 / 1.0x | 1.9 / 1.0x | 2.9 / 1.1x | 11.7 / 1.1x |

**gitgit** - zone with full history:

| layout (GETs / read amp) | 1% zones (k=1) | 5% zones (k=2) | 10% zones (k=3) | 50% zones (k=17) |
|---|---|---|---|---|
| id (hash) order [current pack] | 681.2 / 4.3x | 1161.2 / 6.3x | 1234.5 / 5.7x | 354.0 / 3.6x |
| commit (arrival) order | 135.2 / 11.1x | 641.1 / 4.6x | 372.7 / 3.6x | 681.0 / 2.0x |
| zone, commit | 1.0 / 1.0x | 1.9 / 1.0x | 2.6 / 1.0x | 6.8 / 1.0x |
| zone, directory, commit | 1.0 / 1.0x | 1.9 / 1.0x | 2.6 / 1.0x | 6.8 / 1.0x |
| zone, path, commit (file history) | 1.0 / 1.0x | 1.9 / 1.0x | 2.6 / 1.0x | 6.8 / 1.0x |
| zone, head-first, path, commit | 1.0 / 1.0x | 1.9 / 1.0x | 2.6 / 1.0x | 6.8 / 1.0x |

**gitgit** - history of one file:

| layout | GETs / read amp |
|---|---|
| id (hash) order [current pack] | 28.8 / 1.4x |
| commit (arrival) order | 20.2 / 17.8x |
| zone, commit | 17.8 / 19.8x |
| zone, directory, commit | 1.0 / 1.0x |
| zone, path, commit (file history) | 1.0 / 1.0x |
| zone, head-first, path, commit | 1.6 / 1.0x |

**gitgit** - incremental pull of 20 commits:

| layout | GETs / read amp |
|---|---|
| id (hash) order [current pack] | 81.7 / 1.2x |
| commit (arrival) order | 1.0 / 1.0x |
| zone, commit | 4.0 / 1.0x |
| zone, directory, commit | 22.0 / 1.4x |
| zone, path, commit (file history) | 21.8 / 1.5x |
| zone, head-first, path, commit | 22.0 / 1.4x |

**cargo** - sparse checkout of HEAD:

| layout (GETs / read amp) | 1% zones (k=1) | 5% zones (k=1) | 10% zones (k=2) | 50% zones (k=10) |
|---|---|---|---|---|
| id (hash) order [current pack] | 70.6 / 20.0x | 69.3 / 6.4x | 152.4 / 28.9x | 414.1 / 39.4x |
| commit (arrival) order | 19.3 / 24.8x | 21.6 / 10.3x | 43.5 / 24.4x | 151.8 / 6.6x |
| zone, commit | 9.5 / 9.3x | 8.3 / 6.4x | 26.1 / 10.9x | 90.7 / 6.6x |
| zone, directory, commit | 7.3 / 15.3x | 6.6 / 5.1x | 20.4 / 7.8x | 70.4 / 6.8x |
| zone, path, commit (file history) | 7.2 / 24.7x | 6.4 / 5.1x | 19.6 / 8.5x | 68.5 / 6.8x |
| zone, head-first, path, commit | 1.0 / 1.0x | 1.0 / 1.0x | 2.0 / 1.1x | 7.6 / 1.2x |

**cargo** - history of one file / incremental pull:

| layout | GETs / read amp |
|---|---|
| id (hash) order [current pack] | 16.8 / 2.0x |
| commit (arrival) order | 12.0 / 20.6x |
| zone, commit | 9.6 / 39.1x |
| zone, directory, commit | 1.0 / 1.0x |
| zone, path, commit (file history) | 1.0 / 1.0x |
| zone, head-first, path, commit | 1.6 / 1.3x |

| layout | GETs / read amp |
|---|---|
| id (hash) order [current pack] | 125.9 / 7.6x |
| commit (arrival) order | 1.0 / 1.0x |
| zone, commit | 4.6 / 1.0x |
| zone, directory, commit | 35.5 / 4.1x |
| zone, path, commit (file history) | 34.8 / 4.1x |
| zone, head-first, path, commit | 34.8 / 4.1x |

**synth** - sparse checkout of HEAD:

| layout (GETs / read amp) | 1% zones (k=1) | 5% zones (k=2) | 10% zones (k=4) | 50% zones (k=20) |
|---|---|---|---|---|
| id (hash) order [current pack] | 132.5 / 1.7x | 220.5 / 1.9x | 314.0 / 2.1x | 379.6 / 1.6x |
| commit (arrival) order | 37.8 / 1.3x | 70.7 / 1.3x | 114.3 / 1.2x | 246.8 / 1.3x |
| zone, commit | 6.2 / 1.3x | 15.6 / 1.3x | 21.0 / 1.2x | 118.0 / 1.2x |
| zone, directory, commit | 3.5 / 1.4x | 8.2 / 1.4x | 14.0 / 1.2x | 67.0 / 1.2x |
| zone, path, commit (file history) | 3.5 / 1.4x | 8.2 / 1.4x | 14.0 / 1.2x | 67.0 / 1.2x |
| zone, head-first, path, commit | 1.0 / 1.0x | 2.0 / 1.0x | 4.0 / 1.0x | 20.0 / 1.0x |

Other datasets (ripgrep, synth, sb_*) are in the data files. Reading the tables:

* **The current id-sorted pack is the worst layout for every zone-scoped read. CONFIRMED.** On cargo, a 1-zone HEAD checkout
  takes 70.6 GETs; zone-major layouts take 7 to 9, and the layout that puts the zone's *current* objects first takes 1.0.
  On git.git, 50 percent of zones: 1681 GETs (id order) versus 204 (zone, directory) versus 11.7 (zone, head-first).
* **Grouping by zone alone is not enough for sparse checkouts of HEAD. CONFIRMED.** "Zone, commit" and "zone, path, commit"
  fetch 7.5 to 14x more bytes than needed on git.git (read amplification 7.5x to 13.9x at 1 to 50 percent of zones) because the zone's
  history is interleaved with its current objects. Only *separating live from historical objects* ("zone, head-first")
  reaches 1.0 to 1.1x on every dataset. Directory ordering improves GET count (git.git 25.3 to 12.1 for one zone) but not
  amplification.
* **Every layout optimises one workload and hurts another. CONFIRMED (Pareto).** Commit (arrival) order is the best (1.0
  GET, 1.0x) for incremental pulls and the worst-but-one for zone reads; "zone, path, commit" is perfect for file history
  (1.0 / 1.0x) but needs 35 GETs (4.1x) for a 20-commit pull on cargo and 68 GETs on synth where commit order needs 1.0;
  "zone, head-first" is perfect for sparse checkout, near-perfect for file history (1.6 GETs / 1.3x on cargo, 2.0 / 1.0x
  on synth) and as bad as the other zone-major layouts for pulls (35 GETs on cargo, 68 on synth). No static ordering
  dominates; a store serving all four patterns needs more than one physical arrangement (copies, or an index that makes the
  scattered reads cheap) or an explicit priority.
* **Zone history fetch is solved by any zone-major layout. CONFIRMED.** About 1 GET per zone and 1.0x on cargo and synth
  (e.g. 1.9 GETs for 2 zones).

### 5.2 E2: adaptive versus static under a skewed, drifting workload

A trace of four phases of 150 requests each: 60 percent sparse checkout of 5 percent of zones, 25 percent history of one
file, 15 percent incremental pull of 20 commits; zone and file popularity Zipf(1.0). Phase C and D use a different random
popularity permutation (drift). All policies see the identical request sequences. Reorganisation happens after phase A (and
again after C for the "twice" policies), trained only on the preceding phase. *Full co-access rewrite* re-sorts all
objects by the set of requests that touched them (hot groups first); *hot-set-only* writes just the touched objects as a
new hot region and leaves the rest in place. Reorganisation cost = bytes whose 1 MiB block changes (full) or bytes of
touched objects (hot-only), counted as read + write in the "net gain".

**cargo** (cost-equivalent MB per request; lower is better):

| policy | A (train) | B (same) | C (drift) | D (same as C) | rewritten MiB (total) | net gain vs static zone,commit after D (MB-eq) |
|---|---|---|---|---|---|---|
| static: commit order | 74.7 | 75.8 | 65.2 | 60.0 | 0.0 | -19689 |
| static: zone, commit | 38.0 | 43.1 | 34.1 | 29.4 | 0.0 | 0 |
| static: zone, head-first, path, commit | 27.5 | 33.7 | 38.3 | 31.7 | 0.0 | 2004 |
| adaptive: full co-access rewrite after phase A (once) | 38.0 | 34.9 | 39.3 | 32.4 | 367.9 | -778 |
| adaptive: hot-set-only rewrite after phase A (once) | 38.0 | 18.3 | 20.7 | 19.0 | 11.1 | 7263 |
| adaptive: full rewrite after A and again after C | 38.0 | 34.9 | 39.3 | 30.2 | 713.8 | -1170 |
| adaptive: hot-set-only after A and again after C | 38.0 | 18.3 | 20.7 | 18.3 | 27.1 | 7331 |

**gitgit**:

| policy | A (train) | B (same) | C (drift) | D (same as C) | rewritten MiB (total) | net gain vs static zone,commit after D (MB-eq) |
|---|---|---|---|---|---|---|
| static: commit order | 251.3 | 263.7 | 307.4 | 233.3 | 0.0 | -63148 |
| static: zone, commit | 145.7 | 154.6 | 191.3 | 143.2 | 0.0 | 0 |
| static: zone, head-first, path, commit | 25.2 | 26.6 | 26.2 | 31.7 | 0.0 | 78769 |
| adaptive: full co-access rewrite after phase A (once) | 145.7 | 26.8 | 26.6 | 32.0 | 2610.9 | 55095 |
| adaptive: hot-set-only rewrite after phase A (once) | 145.7 | 29.9 | 19.0 | 24.5 | 33.9 | 62303 |
| adaptive: full rewrite after A and again after C | 145.7 | 26.8 | 26.6 | 31.8 | 5127.6 | 49848 |
| adaptive: hot-set-only after A and again after C | 145.7 | 29.9 | 19.0 | 22.1 | 58.9 | 62604 |

**synth**:

| policy | A (train) | B (same) | C (drift) | D (same as C) | rewritten MiB (total) | net gain vs static zone,commit after D (MB-eq) |
|---|---|---|---|---|---|---|
| static: commit order | 235.2 | 219.9 | 215.0 | 193.0 | 0.0 | -99301 |
| static: zone, commit | 55.4 | 55.0 | 47.1 | 43.6 | 0.0 | 0 |
| static: zone, head-first, path, commit | 56.7 | 64.8 | 72.7 | 63.3 | 0.0 | -8452 |
| adaptive: full co-access rewrite after phase A (once) | 55.4 | 71.4 | 86.3 | 75.5 | 638.9 | -14473 |
| adaptive: hot-set-only rewrite after phase A (once) | 55.4 | 46.0 | 54.9 | 50.4 | 240.7 | -1342 |
| adaptive: full rewrite after A and again after C | 55.4 | 71.4 | 86.3 | 66.7 | 1278.1 | -14488 |
| adaptive: hot-set-only after A and again after C | 55.4 | 46.0 | 54.9 | 44.2 | 475.6 | -905 |

* **Adaptation by rewriting only the observed hot set paid for itself on cargo and git.git. CONFIRMED on these two traces.**
  cargo: 38.0 -> 18.3 per request after a one-off 11 MiB rewrite (the store is 373 MiB); git.git: 145.7 -> 29.9 after a
  34 MiB rewrite, and it adapts to drift without a second reorganisation (19.0 in phase C). The cumulative net gain,
  including read + write of the rewrite, is positive by phase B.
* **Full re-sorting by co-access is a negative result. CONFIRMED.** It rewrites the whole store (cargo 368 MiB, git.git 2.6 GB
  per pass), loses on zone-history and pull requests, and ends behind the best static layout ("zone, head-first") on
  git.git (32.0 vs 31.7 in phase D, cumulative gain 55k vs 79k MB-eq) and behind plain "zone, commit" on cargo and synth.
* **The adaptive gain is not universal. CONFIRMED on synth:** hot-set-only rewriting (241 MiB rewritten) ends 1.3k MB-eq
  *behind* static "zone, commit". [HYPOTHESIS for the cause: the touched set is large (241 MiB of 639 MiB), so moving it
  fragments the requests that the rest of the layout served well.] Adaptive placement is expected to depend on a small hot set.
* **A static head-first layout already captures most of the sparse-checkout benefit on git.git (25.2 per request, no
  reorganisation).** The catch is that "head-first" is defined against a particular HEAD; as HEAD advances, superseded
  objects must be moved out of the head region. That maintenance cost was **not measured** [HYPOTHESIS: small if done
  incrementally during compaction, since only objects that stopped being current move].

## 6. Summary: confirmed, hypotheses, negative results

**CONFIRMED (with data above):**

1. Deltas cut stored size 2.6x (mixed synthetic) to 19.4x (git.git) versus per-object compression; the lab's engine is
   within roughly 10 percent of `git repack` size (sets differ by up to 13 percent in object count).
2. Ordering by id (the current pack) defeats delta search (364 MiB vs 37 MiB on cargo); related versions must be adjacent.
3. Depth bounds, checkpoints and skip-deltas trade size for reads smoothly; bound 8 costs 1.56x to 1.7x size for about 6
   to 7 reads; unbounded needs 16 to 24 reads per random object.
4. Newest-first chains give HEAD reads of 1 base read regardless of history length.
5. Keeping deltas inside blocks of at least 4 MiB logical size costs no size and gives one block per read.
6. For sparse reads, grouping by zone and separating current from historical objects reduces GETs 10x to 140x and read
   amplification to about 1.0 versus the id-sorted pack; no static ordering is best for all four access patterns.
7. Hot-set-only reorganisation can pay for itself within one phase when the hot set is small (cargo, git.git).

**HYPOTHESES (not established):**

* Remote latency dominates CPU (CPU measured warm; no object store in the loop).
* Hybrid delta + block compression or per-zone dictionaries would beat both pure approaches.
* Maintaining a head-first region is cheap when done incrementally.
* Results transfer from git objects to allgit's encodings (blake3 ids, canonical varint trees): the byte layout of trees
  differs from git's, so tree delta ratios in particular may differ.

**NEGATIVE / UNHELPFUL RESULTS:**

* Window 50 is not better than window 25 (cargo 37.2 vs 35.5 MiB, git.git 136.0 vs 129.8, synth equal) under a depth-50 greedy selection.
* Commit-order and size-only orderings give 7x and 5x worse sizes than git's order on git.git.
* Path-ascending (oldest-first) order is 5 to 8 percent larger than newest-first on cargo and git.git (equal on synth) and needs 3 to 4x more reads for HEAD.
* Independent 4 MiB compressed blocks did not help the synthetic mutated-binary workload (1.3x worse than deltas).
* Full co-access rewrite is dominated by hot-set-only rewrite and by a static head-first layout (section 5.2).
* Adaptive hot-set rewrite lost to static zone,commit on `synth` (section 5.2).

## 7. Limits

* Git objects, not allgit objects; zones by path-prefix rule; non-merge-diff object sets.
* Timings are warm, in memory, on a shared machine; no cold cache, no remote store, no network.
* The depth/delta engine is a simple greedy 16-byte block matcher; a production encoder would be tuned.
* E uses undeltified compressed sizes, so placement and delta layout are studied separately; their interaction (a delta
  base in another zone's region) is only bounded by C3.
* Workloads for E/G are synthetic traces (Zipf zone popularity, uniform commits); no real access logs were available.
* `git repack` was run with default threads; the 250/250 reference was run only for ripgrep and cargo (see the data).
* Not measured: write-path cost of building these packs online, crash safety, fsck cost, index size with delta fields
  (about 4 extra bytes per delta entry for an offset), GC interaction.

## 8. Reproduce

```sh
# on the Lambda box, repositories cloned bare under repos/
cd crates/vcs-storage-lab && cargo build --release
lab synth 40 3000 1 | git -C repos/synth.git fast-import --quiet     # after git init --bare + HEAD -> refs/heads/main
lab fixture all repos/sb 8 42                                          # storebench workloads as git repos
LAB_SCRATCH=/some/scratch lab all <name> repos/<name>.git out.jsonl    # A, C, E, F, G (set LAB_SKIP250=1 to skip git 250/250)
lab c4 <name> repos/<name>.git out_c4.jsonl                            # C4 (independent compressed blocks)
python3 tools/tables.py <data_dir> <template.md>                       # regenerate the tables in this document
```

Raw JSON-lines: `docs/research/storage/data/packing/*.jsonl` (one file per dataset; `c4_*.jsonl` for C4). Columns are
self-describing keys. Large raw clones stay on the box (`~/rt/s_packing/repos`, about 1.3 GB plus the storebench fixtures).

## 9. Sources

Accessed 2026-10-31.

* Git pack heuristics (archived 2006 discussion: sort by type, name hash, size; window and depth; newest-first write
  order): https://git-scm.com/docs/pack-heuristics (rendered page; revision not stated by the site). The page itself warns
  the constants have changed since.
* Git pack format, delta representation (copy/insert opcodes): https://git-scm.com/docs/gitformat-pack [cited from
  knowledge, page not re-fetched in this session; format reproduced from the documentation and round-trip tested here].
* Skip-deltas in Subversion: https://svn.apache.org/repos/asf/subversion/trunk/notes/skip-deltas [unverified: not fetched].
* Zstandard: https://github.com/facebook/zstd (crate `zstd` 0.13.3, libzstd 1.5.7 as vendored by `zstd-sys` 2.1.2).
* Local sources: `crates/vcs-store/src/pack.rs`, `docs/object-model.md` ("Packs"), `docs/research/storage/workloads.md`.

