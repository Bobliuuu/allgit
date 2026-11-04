# Compaction, write buffers and tiering experiments (issue #184, experiments F and G; GC sections by the meta agent)

Part of epic #190. Ownership: sections 1 to 4 (LSM-style write buffers and compaction policies, hot/cold tiering) are
written by the packing agent (`s_packing`). Sections whose heading starts with "GC" belong to the meta agent and are
added below the marker in section 6; edits to this file are additive. Companion: [packing-experiments.md](packing-experiments.md)
(experiments A, C, E; same lab, same datasets, same machine and caveats, sections 2 and 7 there).

Evidence labels as in the companion: **CONFIRMED** (measured here), **HYPOTHESIS**. All numbers are generated from
`docs/research/storage/data/packing/*.jsonl` by `crates/vcs-storage-lab/tools/tables.py`. Both experiments are
**simulations on real key streams**: no files, no network, no object store. Every byte "written" or "read" is a byte the
modelled implementation would write or read; latencies in G are model parameters, never measurements.

## 1. Experiment F: write buffers and compaction

### 1.1 Model

* **Keys and sizes.** The real object ids of each dataset in arrival (commit) order, with their real zstd-3 compressed size
  plus 4 bytes framing. Datasets: ripgrep, cargo, gitgit (git.git), synth and the 16 `sb_*` workloads (see the companion).
* **Write buffer.** Objects accumulate in memory until `buffer` bytes, then flush as one immutable *sorted run* (a pack
  with a sorted index; one PUT). Buffers of 256 KiB, 1, 4 and 16 MiB are scaled to the datasets (cargo stores 373 MiB,
  git.git 2.6 GB; a production buffer would be larger and the datasets bigger; run counts scale accordingly).
* **Duplicate puts.** Content addressing makes re-uploading an object harmless but wasteful. With rate d, each ingested
  object is accompanied (with probability d) by a re-put of a uniformly random earlier object. d = 0 and 0.25 are reported.
* **Policies.** *Flush only* (no compaction); *tiered* with size ratio T (a level holds up to T runs; when full they merge
  into one run in the next level, so each byte is rewritten about once per level); *leveled* with ratio T (L0 holds up
  to 4 overlapping runs, then L0 and every overlapping L1 file merge; level i has target size buffer x T^i; one file of
  `buffer` bytes at a time is merged into the overlapping files of level i+1, LevelDB-style, round-robin).
* **Metrics.** *Write amplification* = (flush + compaction output bytes) / bytes ingested (duplicates included).
  *Space amplification* = bytes held at the end / bytes of distinct objects; *peak space amplification* also counts the
  output of a compaction that is in flight while its inputs still exist. *Read amplification*: probes (one ranged read of a
  run's index or data) needed to find a present object, or to establish that an absent object is absent, searching newest to
  oldest; *with bloom* models a 1 percent false-positive bloom filter per sorted run (model, not implemented: a present key
  costs 1 + 0.01 x (runs before it)). Real membership is computed on the real keys, 600 random lookups per snapshot.
  *Objects PUT* and *compaction read MiB* are request and byte counts of a remote implementation.

### 1.2 Results

Header: buffer in KiB. Duplicate rate 0 first (pure append of unique objects), then 0.25.

**cargo, d = 0:**

| buffer KiB | policy | write amp | space amp | peak space amp | sorted runs | hit probes (no bloom) | hit probes (bloom 1%) | miss probes (no bloom) | miss probes (bloom 1%) | objects PUT | compaction read MiB |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 256 | flush only (no compaction) | 1.00 | 1.00 | 1.00 | 1453.0 | 798.85 | 9.16 | 1453.00 | 14.53 | 1453 | 0.0 |
| 256 | tiered T=4 | 5.54 | 1.00 | 1.40 | 10.0 | 9.61 | 1.09 | 10.00 | 0.10 | 1934 | 1695.4 |
| 256 | tiered T=10 | 3.65 | 1.00 | 1.37 | 13.0 | 12.29 | 1.11 | 13.00 | 0.13 | 1613 | 988.2 |
| 256 | leveled T=4 (L0 trigger 4, file size = buffer) | 13.57 | 1.00 | 1.00 | 7.0 | 5.85 | 1.05 | 7.00 | 0.07 | 21761 | 4693.2 |
| 256 | leveled T=10 (L0 trigger 4, file size = buffer) | 14.88 | 1.00 | 1.01 | 5.0 | 4.18 | 1.03 | 5.00 | 0.05 | 22956 | 5182.4 |
| 1024 | flush only (no compaction) | 1.00 | 1.00 | 1.00 | 371.0 | 205.09 | 3.09 | 371.00 | 3.71 | 371 | 0.0 |
| 1024 | tiered T=4 | 4.54 | 1.00 | 1.38 | 8.0 | 7.57 | 1.07 | 8.00 | 0.08 | 492 | 1322.9 |
| 1024 | tiered T=10 | 2.81 | 1.00 | 1.08 | 11.0 | 9.35 | 1.08 | 11.00 | 0.11 | 411 | 675.6 |
| 1024 | leveled T=4 (L0 trigger 4, file size = buffer) | 10.44 | 1.00 | 1.00 | 8.0 | 6.86 | 1.06 | 8.00 | 0.08 | 4260 | 3524.3 |
| 1024 | leveled T=10 (L0 trigger 4, file size = buffer) | 11.33 | 1.00 | 1.02 | 6.0 | 5.70 | 1.05 | 6.00 | 0.06 | 4453 | 3854.9 |
| 4096 | flush only (no compaction) | 1.00 | 1.00 | 1.00 | 94.0 | 52.67 | 1.53 | 94.00 | 0.94 | 94 | 0.0 |
| 4096 | tiered T=4 | 3.53 | 1.00 | 1.37 | 7.0 | 6.56 | 1.06 | 7.00 | 0.07 | 123 | 945.6 |
| 4096 | tiered T=10 | 1.97 | 1.00 | 1.07 | 13.0 | 9.27 | 1.08 | 13.00 | 0.13 | 103 | 360.6 |
| 4096 | leveled T=4 (L0 trigger 4, file size = buffer) | 7.44 | 1.00 | 1.02 | 6.0 | 4.94 | 1.04 | 6.00 | 0.06 | 769 | 2403.8 |
| 4096 | leveled T=10 (L0 trigger 4, file size = buffer) | 7.36 | 1.00 | 1.08 | 4.0 | 3.90 | 1.03 | 4.00 | 0.04 | 735 | 2372.3 |
| 16384 | flush only (no compaction) | 1.00 | 1.00 | 1.00 | 24.0 | 14.01 | 1.13 | 24.00 | 0.24 | 24 | 0.0 |
| 16384 | tiered T=4 | 2.69 | 1.00 | 1.37 | 3.0 | 2.65 | 1.02 | 3.00 | 0.03 | 31 | 629.3 |
| 16384 | tiered T=10 | 1.86 | 1.00 | 1.29 | 6.0 | 5.28 | 1.04 | 6.00 | 0.06 | 26 | 320.1 |
| 16384 | leveled T=4 (L0 trigger 4, file size = buffer) | 4.74 | 1.00 | 1.13 | 3.0 | 2.10 | 1.01 | 3.00 | 0.03 | 118 | 1397.5 |
| 16384 | leveled T=10 (L0 trigger 4, file size = buffer) | 4.27 | 1.00 | 1.39 | 2.0 | 1.62 | 1.01 | 2.00 | 0.02 | 105 | 1221.5 |

**gitgit (git.git), d = 0.25:**

| buffer KiB | policy | write amp | space amp | peak space amp | sorted runs | hit probes (no bloom) | hit probes (bloom 1%) | miss probes (no bloom) | miss probes (bloom 1%) | objects PUT | compaction read MiB |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 256 | flush only (no compaction) | 1.00 | 1.19 | 1.19 | 11895.0 | 6077.44 | 62.21 | 11895.00 | 118.95 | 11895 | 0.0 |
| 256 | tiered T=4 | 6.50 | 1.10 | 1.14 | 15.0 | 13.62 | 1.13 | 15.00 | 0.15 | 15855 | 17467.0 |
| 256 | tiered T=10 | 4.59 | 1.03 | 1.81 | 24.0 | 23.34 | 1.22 | 24.00 | 0.24 | 13214 | 11660.4 |
| 256 | leveled T=4 (L0 trigger 4, file size = buffer) | 17.59 | 1.06 | 1.06 | 10.0 | 9.42 | 1.08 | 10.00 | 0.10 | 231357 | 52311.9 |
| 256 | leveled T=10 (L0 trigger 4, file size = buffer) | 19.47 | 1.02 | 1.02 | 7.0 | 6.90 | 1.06 | 7.00 | 0.07 | 248137 | 58308.7 |
| 1024 | flush only (no compaction) | 1.00 | 1.19 | 1.19 | 3091.0 | 1587.38 | 16.98 | 3091.00 | 30.91 | 3091 | 0.0 |
| 1024 | tiered T=4 | 5.85 | 1.09 | 1.47 | 7.0 | 6.07 | 1.05 | 7.00 | 0.07 | 4119 | 15457.6 |
| 1024 | tiered T=10 | 3.84 | 1.09 | 1.44 | 13.0 | 11.91 | 1.11 | 13.00 | 0.13 | 3433 | 9158.8 |
| 1024 | leveled T=4 (L0 trigger 4, file size = buffer) | 14.73 | 1.06 | 1.06 | 9.0 | 8.40 | 1.07 | 9.00 | 0.09 | 49715 | 43345.9 |
| 1024 | leveled T=10 (L0 trigger 4, file size = buffer) | 16.43 | 1.05 | 1.05 | 7.0 | 6.59 | 1.06 | 7.00 | 0.07 | 53473 | 48691.5 |
| 4096 | flush only (no compaction) | 1.00 | 1.19 | 1.19 | 781.0 | 402.29 | 5.04 | 781.00 | 7.81 | 781 | 0.0 |
| 4096 | tiered T=4 | 4.82 | 1.09 | 1.46 | 7.0 | 6.04 | 1.05 | 7.00 | 0.07 | 1039 | 12238.2 |
| 4096 | tiered T=10 | 2.85 | 1.14 | 1.17 | 16.0 | 12.24 | 1.11 | 16.00 | 0.16 | 866 | 5915.6 |
| 4096 | leveled T=4 (L0 trigger 4, file size = buffer) | 11.61 | 1.05 | 1.06 | 6.0 | 5.41 | 1.04 | 6.00 | 0.06 | 9613 | 33576.1 |
| 4096 | leveled T=10 (L0 trigger 4, file size = buffer) | 12.35 | 1.03 | 1.04 | 4.0 | 3.84 | 1.03 | 4.00 | 0.04 | 10111 | 35994.8 |
| 16384 | flush only (no compaction) | 1.00 | 1.18 | 1.18 | 196.0 | 101.48 | 2.01 | 196.00 | 1.96 | 196 | 0.0 |
| 16384 | tiered T=4 | 3.84 | 1.09 | 1.45 | 4.0 | 3.05 | 1.02 | 4.00 | 0.04 | 260 | 9145.7 |
| 16384 | tiered T=10 | 2.38 | 1.09 | 1.12 | 16.0 | 13.47 | 1.12 | 16.00 | 0.16 | 216 | 4577.8 |
| 16384 | leveled T=4 (L0 trigger 4, file size = buffer) | 8.48 | 1.05 | 1.08 | 4.0 | 3.45 | 1.02 | 4.00 | 0.04 | 1745 | 23794.5 |
| 16384 | leveled T=10 (L0 trigger 4, file size = buffer) | 8.93 | 1.02 | 1.08 | 3.0 | 2.31 | 1.01 | 3.00 | 0.03 | 1822 | 25280.9 |

**synth, d = 0.25:**

| buffer KiB | policy | write amp | space amp | peak space amp | sorted runs | hit probes (no bloom) | hit probes (bloom 1%) | miss probes (no bloom) | miss probes (bloom 1%) | objects PUT | compaction read MiB |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 256 | flush only (no compaction) | 1.00 | 1.33 | 1.33 | 1029.0 | 360.69 | 4.71 | 1029.00 | 10.29 | 1029 | 0.0 |
| 256 | tiered T=4 | 5.57 | 1.00 | 2.19 | 3.0 | 2.99 | 1.02 | 3.00 | 0.03 | 1371 | 4095.3 |
| 256 | tiered T=10 | 3.63 | 1.02 | 2.19 | 12.0 | 11.80 | 1.11 | 12.00 | 0.12 | 1142 | 2433.1 |
| 256 | leveled T=4 (L0 trigger 4, file size = buffer) | 11.46 | 1.10 | 1.11 | 7.0 | 6.21 | 1.05 | 7.00 | 0.07 | 13096 | 9037.5 |
| 256 | leveled T=10 (L0 trigger 4, file size = buffer) | 12.38 | 1.10 | 1.12 | 5.0 | 4.42 | 1.03 | 5.00 | 0.05 | 13358 | 9822.5 |
| 1024 | flush only (no compaction) | 1.00 | 1.33 | 1.33 | 539.0 | 193.41 | 2.99 | 539.00 | 5.39 | 539 | 0.0 |
| 1024 | tiered T=4 | 4.65 | 1.13 | 1.67 | 8.0 | 7.11 | 1.06 | 8.00 | 0.08 | 716 | 3223.1 |
| 1024 | tiered T=10 | 2.82 | 1.23 | 1.38 | 17.0 | 13.73 | 1.13 | 17.00 | 0.17 | 597 | 1612.5 |
| 1024 | leveled T=4 (L0 trigger 4, file size = buffer) | 10.32 | 1.11 | 1.12 | 8.0 | 7.14 | 1.06 | 8.00 | 0.08 | 5901 | 8060.1 |
| 1024 | leveled T=10 (L0 trigger 4, file size = buffer) | 10.54 | 1.05 | 1.06 | 6.0 | 5.73 | 1.05 | 6.00 | 0.06 | 5917 | 8286.9 |
| 4096 | flush only (no compaction) | 1.00 | 1.32 | 1.32 | 188.0 | 67.94 | 1.69 | 188.00 | 1.88 | 188 | 0.0 |
| 4096 | tiered T=4 | 3.46 | 1.19 | 1.23 | 8.0 | 5.89 | 1.05 | 8.00 | 0.08 | 248 | 2178.0 |
| 4096 | tiered T=10 | 2.32 | 1.15 | 1.20 | 17.0 | 13.32 | 1.13 | 17.00 | 0.17 | 207 | 1236.8 |
| 4096 | leveled T=4 (L0 trigger 4, file size = buffer) | 7.98 | 1.09 | 1.11 | 4.0 | 3.22 | 1.02 | 4.00 | 0.04 | 1631 | 6080.7 |
| 4096 | leveled T=10 (L0 trigger 4, file size = buffer) | 8.35 | 1.04 | 1.14 | 3.0 | 2.26 | 1.01 | 3.00 | 0.03 | 1648 | 6428.0 |
| 16384 | flush only (no compaction) | 0.98 | 1.30 | 1.30 | 52.0 | 19.38 | 1.19 | 52.00 | 0.52 | 52 | 0.0 |
| 16384 | tiered T=4 | 2.75 | 1.18 | 1.49 | 4.0 | 2.43 | 1.01 | 4.00 | 0.04 | 68 | 1584.9 |
| 16384 | tiered T=10 | 1.87 | 1.22 | 1.44 | 7.0 | 4.17 | 1.03 | 7.00 | 0.07 | 57 | 809.4 |
| 16384 | leveled T=4 (L0 trigger 4, file size = buffer) | 5.32 | 1.08 | 1.16 | 3.0 | 2.31 | 1.01 | 3.00 | 0.03 | 291 | 3826.5 |
| 16384 | leveled T=10 (L0 trigger 4, file size = buffer) | 4.95 | 1.05 | 1.30 | 2.0 | 1.71 | 1.01 | 2.00 | 0.02 | 271 | 3530.8 |

Other datasets (ripgrep, sb_*, the remaining duplicate rates) are in the data files.

### 1.3 Findings

* **Without duplicates compaction reclaims no space. CONFIRMED.** Space amplification is exactly 1.00 for every policy at
  d = 0 (cargo table): content addressing means there is nothing to overwrite or delete, so compaction exists only to cut
  *read cost and request count*, never to save bytes. With d = 0.25 it removes the duplicates (git.git 1.19 for flush-only
  down to 1.02 to 1.14; synth 1.30 to 1.33 down to 1.00 to 1.23).
* **Flush-only is cheap to write and expensive to read. CONFIRMED.** cargo at 1 MiB buffer: 371 sorted runs, 205 probes
  per present lookup and 371 per absent lookup without filters; 3.1 and 3.7 with 1 percent bloom filters. git.git at 256 KiB:
  11,895 runs. A cold reader must also fetch each run's index (or filter) before the first lookup, so the run count is also
  the cold-start request count. [HYPOTHESIS: with filters cached, a few hundred runs is workable; the filter memory
  (about 10 bits per object per run) and the cold start are the real cost.]
* **Tiered wins on write amplification by 2.3x to 4.3x; leveled wins on read amplification and peak space. CONFIRMED.**
  cargo, 1 MiB buffer: tiered T=4 write amp 4.54 (8 runs), T=10 2.81 (11 runs); leveled T=4 10.44 (8 runs), T=10 11.33
  (6 runs). git.git, 4 MiB, d=0.25: tiered T=4 4.82 (7 runs), T=10 2.85 (16 runs); leveled T=4 11.61 (6 runs), T=10 12.35
  (4 runs). Peak space amplification: tiered 1.07 to 2.19 over the three datasets (the largest merge briefly doubles its inputs);
  leveled 1.00 to 1.39.
* **Leveled compaction is unusually expensive here because keys are hashes. CONFIRMED.** A flushed run covers the whole
  key range, so every L0 to L1 merge overlaps all of L1; leveled write amplification is 4.3 to 19.5x and object PUTs are 4x to
  15x those of tiered T=4 (cargo 256 KiB: 21,761 and 22,956 PUTs against 1,934 and 1,613; git.git: 231k to 248k against 15.9k). On an object
  store where PUTs cost money and latency, that count, not the bytes, is the dominant penalty. [HYPOTHESIS: a layout that
  splits the key space by range *before* the flush (one run per key range) would lower leveled's cost; not tested.]
* **Larger buffers dominate every other knob. CONFIRMED.** Going from 256 KiB to 16 MiB cuts tiered T=4 write amplification
  from 5.5 to 2.7 (cargo) and 6.5 to 3.8 (git.git), and runs from 10 to 3 (cargo). Buffer size is bounded by what a client
  can hold and lose on a crash; that durability trade-off is not modelled.
* **Pareto (cargo, d = 0).** Flush-only: lowest write cost, highest read cost. Tiered T=10: low write cost (1.9 to 3.7), 6 to
  13 runs. Tiered T=4: write 2.7 to 5.5, 3 to 10 runs. Leveled: fewest runs (2 to 8) at 4.3 to 14.9 write amplification and
  4x to 15x more PUTs. No policy wins on write, read and PUT count together.

## 2. Notes on assumptions (F)

* Keys arrive in commit order and are random-looking (hash); a store with locality-preserving keys would behave differently.
* No deletions: GC is out of scope here and owned by the meta agent. Deletions would give compaction a space-reclaim role
  that this experiment shows it does not otherwise have.
* Bloom filters are an analytic model; run indexes are assumed to be fetched once per client and cached.
* The simulation counts bytes; it does not model compaction latency, concurrency with writers, or publish atomicity.

## 3. Experiment G: hot/cold tiering

### 3.1 Model

* **Tiers.** A *hot* tier (local disk or cache, capacity 5, 10, 25 or 50 percent of the stored bytes) and a *cold* tier (the
  object store, always complete). Unit of placement and migration: a 1 MiB block of a layout (companion section 5).
  Layouts: commit (arrival) order and "zone, path, commit".
* **Workload.** 2000 requests in two phases of 1000: 60 percent sparse checkout of 5 percent of the zones (Zipf 1.0
  popularity), 25 percent history of one file (Zipf), 15 percent incremental pull of 20 consecutive commits (uniform
  start). At the phase boundary the popularity permutation changes (drift). A request touches the blocks holding its objects.
* **Policies.** *All cold*; *static age* (the blocks with the newest commits are hot, placed once, never changes); *LRU,
  promote on miss* (a miss fetches and keeps the whole block); *epoch frequency* (every 200 requests the hot set becomes
  the top blocks by decayed access count, decay 0.5; blocks that become hot are fetched); *oracle* (the top blocks by true
  frequency over the whole trace, placed at time 0: an upper bound that no online policy can reach).
* **Cost.** A miss reads the whole missing block from cold. Cold GET count = runs of adjacent missing blocks in one
  request. Model times: cold first-byte latency swept over 20, 50 and 100 ms at 100 MB/s; hot 0.3 ms per GET at 1.5 GB/s
  **(model parameters, chosen to bracket common object-store figures; not measurements)**. Migration bytes are reported
  separately (promotion into hot is cold-read traffic; demotion is free because cold is complete).

### 3.2 Results (phase 2, after the drift; layout "zone, path, commit")

**cargo**:

| hot capacity | policy | block hit rate | cold GETs/req | cold MiB/req | MiB migrated in | model ms/req (cold GET 20/50/100 ms) |
|---|---|---|---|---|---|---|
| 5% | all cold (no hot tier) | 0% | 6.80 | 13.56 | 0 | 278 / 482 / 822 |
| 5% | static hot = newest blocks (age) | 8% | 6.62 | 12.43 | 0 | 264 / 463 / 794 |
| 5% | LRU, promote on miss | 17% | 6.12 | 11.28 | 11282 | 243 / 426 / 732 |
| 5% | epoch frequency (200 req, decay 0.5) | 22% | 6.73 | 10.55 | 33 | 248 / 450 / 786 |
| 5% | oracle: top blocks by true frequency | 23% | 6.44 | 10.43 | 0 | 241 / 434 / 757 |
| 10% | all cold (no hot tier) | 0% | 6.80 | 13.56 | 0 | 278 / 482 / 822 |
| 10% | static hot = newest blocks (age) | 19% | 6.11 | 11.03 | 0 | 240 / 423 / 728 |
| 10% | LRU, promote on miss | 33% | 5.26 | 9.07 | 9068 | 204 / 362 / 625 |
| 10% | epoch frequency (200 req, decay 0.5) | 44% | 5.13 | 7.62 | 37 | 188 / 342 / 598 |
| 10% | oracle: top blocks by true frequency | 46% | 5.02 | 7.37 | 0 | 183 / 334 / 585 |
| 25% | all cold (no hot tier) | 0% | 6.80 | 13.56 | 0 | 278 / 482 / 822 |
| 25% | static hot = newest blocks (age) | 70% | 3.12 | 4.01 | 0 | 112 / 206 / 362 |
| 25% | LRU, promote on miss | 66% | 3.25 | 4.63 | 4632 | 121 / 218 / 381 |
| 25% | epoch frequency (200 req, decay 0.5) | 78% | 2.35 | 3.02 | 27 | 88 / 158 / 275 |
| 25% | oracle: top blocks by true frequency | 79% | 2.24 | 2.90 | 0 | 84 / 151 / 263 |
| 50% | all cold (no hot tier) | 0% | 6.80 | 13.56 | 0 | 278 / 482 / 822 |
| 50% | static hot = newest blocks (age) | 86% | 1.75 | 1.90 | 0 | 65 / 117 / 205 |
| 50% | LRU, promote on miss | 88% | 1.43 | 1.66 | 1664 | 56 / 99 / 170 |
| 50% | epoch frequency (200 req, decay 0.5) | 92% | 1.04 | 1.12 | 78 | 43 / 75 / 127 |
| 50% | oracle: top blocks by true frequency | 93% | 0.96 | 1.00 | 0 | 40 / 69 / 117 |

**gitgit (git.git)**:

| hot capacity | policy | block hit rate | cold GETs/req | cold MiB/req | MiB migrated in | model ms/req (cold GET 20/50/100 ms) |
|---|---|---|---|---|---|---|
| 5% | all cold (no hot tier) | 0% | 6.12 | 9.55 | 0 | 223 / 406 / 713 |
| 5% | static hot = newest blocks (age) | 34% | 4.64 | 6.30 | 0 | 162 / 301 / 533 |
| 5% | LRU, promote on miss | 34% | 4.36 | 6.30 | 6299 | 156 / 287 / 505 |
| 5% | epoch frequency (200 req, decay 0.5) | 43% | 3.84 | 5.42 | 89 | 137 / 252 / 444 |
| 5% | oracle: top blocks by true frequency | 41% | 3.77 | 5.62 | 0 | 138 / 251 / 440 |
| 10% | all cold (no hot tier) | 0% | 6.12 | 9.55 | 0 | 223 / 406 / 713 |
| 10% | static hot = newest blocks (age) | 55% | 3.45 | 4.33 | 0 | 119 / 223 / 395 |
| 10% | LRU, promote on miss | 55% | 3.29 | 4.33 | 4335 | 116 / 215 / 379 |
| 10% | epoch frequency (200 req, decay 0.5) | 69% | 2.58 | 2.98 | 151 | 89 / 166 / 295 |
| 10% | oracle: top blocks by true frequency | 72% | 2.42 | 2.71 | 0 | 83 / 155 / 276 |
| 25% | all cold (no hot tier) | 0% | 6.12 | 9.55 | 0 | 223 / 406 / 713 |
| 25% | static hot = newest blocks (age) | 82% | 1.52 | 1.68 | 0 | 55 / 101 / 177 |
| 25% | LRU, promote on miss | 87% | 1.16 | 1.26 | 1256 | 44 / 79 / 137 |
| 25% | epoch frequency (200 req, decay 0.5) | 88% | 1.03 | 1.11 | 869 | 40 / 71 / 122 |
| 25% | oracle: top blocks by true frequency | 92% | 0.67 | 0.73 | 0 | 29 / 49 / 82 |
| 50% | all cold (no hot tier) | 0% | 6.12 | 9.55 | 0 | 223 / 406 / 713 |
| 50% | static hot = newest blocks (age) | 88% | 1.02 | 1.11 | 0 | 39 / 70 / 121 |
| 50% | LRU, promote on miss | 95% | 0.44 | 0.49 | 486 | 22 / 35 / 57 |
| 50% | epoch frequency (200 req, decay 0.5) | 95% | 0.44 | 0.49 | 456 | 22 / 35 / 57 |
| 50% | oracle: top blocks by true frequency | 98% | 0.14 | 0.15 | 0 | 13 / 17 / 24 |

Layout "commit order", phase 1 and the other datasets are in the data files (`exp = "G"`).

### 3.3 Findings

* **A small hot tier barely helps; a quarter of the data does. CONFIRMED on this workload.** cargo, 5 percent hot: block hit
  rate 8 to 23 percent depending on policy and 426 to 463 ms model time per request (50 ms cold latency) against 482 for all
  cold; at 25 percent hot: 66 to 79 percent and 151 to 218 ms. git.git: 34 to 43 percent at 5 percent hot and 82 to 92
  percent at 25 percent hot. [HYPOTHESIS for the cause: sparse checkouts, file histories and pulls touch different blocks, so the working set
  is wider than the Zipf-hot zones alone.]
* **Epoch-frequency retention is close to the oracle and beats the simpler policies at 25 percent. CONFIRMED.** Hit rate
  at 25 percent hot: cargo 78 vs oracle 79 vs LRU 66 vs age 70; ripgrep 51 vs 53 vs 37 vs 34; synth 82 vs 82 vs 74 vs 75;
  git.git 88 vs 92 vs 87 vs 82. At 5 percent hot the ranking is mixed: LRU promote-on-miss can have fewer cold GETs than
  frequency (cargo 6.12 vs 6.73 GETs per request) because promoting whole blocks coalesces requests.
* **Migration traffic is small for frequency retention and large for LRU. CONFIRMED.** cargo at 25 percent hot, phase 2:
  the frequency policy migrated 27 MiB, LRU promoted 4,632 MiB (every miss moves a 1 MiB
  block), i.e. LRU buys its hit rate with cold-read traffic. [Not a recommendation without egress pricing: see
  cost-model.md.]
* **Static age-based placement is a good first approximation only when recent data is the hot data.** Its hit rate is
  8 to 9 percent at 5 percent hot on cargo and ripgrep (young blocks are not where Zipf-popular zones live) but on cargo
  70 and 86 percent at 25 and 50 percent hot (within 8 points of the frequency policy; ripgrep: 34 and 72 percent);
  after a drift it does not change at all.
* **Cost of a miss.** At 50 ms cold latency a request costs 482 ms all-cold on cargo with 6.8 cold GETs and 13.6 MiB, so
  roughly 340 ms is latency and 140 ms is transfer; halving GET count (layout) matters as much as raising hit rate. The
  layout comparison (data, layout "commit order" vs "zone, path, commit") should be read together with companion section 5.

## 4. Pareto summary (F and G)

| Axis | Best | Worst | Source |
|---|---|---|---|
| Write amplification | flush only (1.0), then tiered T=10 | leveled (4.3 to 19.5x on hash keys) | 1.3 |
| Read probes / cold-start requests | leveled, large buffer | flush only (hundreds to thousands of runs) | 1.3 |
| PUT count | flush only, tiered | leveled (4x to 15x more than tiered) | 1.3 |
| Peak space | leveled, flush only | tiered T=4 at small buffers (up to 2.2x) | 1.3 |
| Hit rate per hot byte | epoch frequency (near oracle) | static age at small capacity | 3.3 |
| Migration traffic | epoch frequency, static | LRU promote on miss | 3.3 |

## 5. Limits

Synthetic request traces; real datasets but no real access logs. Simulated I/O only; no concurrency, no failure
handling, no atomic publish. G treats cold as always complete and demotion as free. Block size is fixed at 1 MiB for G.
Hot-tier metadata and index memory are not counted. Costs in money (requests, egress, storage) are not computed here:
use `docs/research/storage/cost-model.md` with the request and byte counts in the data.

## 6. GC sections (meta agent)

*Reserved for the meta agent. Add GC sections below this line; do not edit sections 1 to 5 without coordination.*

## 7. Sources

Accessed 2026-10-31.

* RocksDB Universal Compaction (tiered vs leveled: lower write amplification, higher read and space amplification):
  https://github.com/facebook/rocksdb/wiki/Universal-Compaction (wiki page; revision not shown).
* O'Neil, Cheng, Gawlick, O'Neil, "The Log-Structured Merge-Tree (LSM-Tree)", Acta Informatica 33(4), 1996
  [unverified: not fetched].
* Dayan, Idreos, "Dostoevsky: Better Space-Time Trade-Offs for LSM-Tree Based Key-Value Stores via Adaptive Removal of
  Superfluous Merging", SIGMOD 2018 [unverified: not fetched].
* Local: `docs/research/storage/cost-model.md`, `docs/research/storage/workloads.md`, `crates/vcs-store-s3`.

