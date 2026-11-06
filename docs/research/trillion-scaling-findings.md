# Scaling toward a trillion: findings

Box: i4i.8xlarge (32 vCPU, 247 GB RAM, 7 TB NVMe), 2026-11-06. Raw data: `docs/research/data/scaling-trillion/`
(`out`, `out2`, `out3`, `probes`); tables are generated: `out/tables.md` (micro fits, `scripts/scaling/analyze.py`) and
`out*/tables_e2e.md` (`scripts/scaling/e2e_tables.py`).

Reading rules:

* **MEASURED** = one run on this box. Every cell is a SINGLE run (no repeats, no variance), so treat differences under
  about 20 percent as noise. Probes in the last section ran while the driver was still running other jobs on the same box.
* **PROJECTED** = a log-log fit extrapolated to 1e12 by `analyze.py`. It is a model, not a measurement. For the O(n)
  rows it says only "this does not fit in RAM or in a day", which is the point.
* Peak RSS from `/usr/bin/time` includes file pages of memory-mapped packs (reclaimable page cache). Where it matters the
  anonymous peak is given separately (`RssAnon`, sampled every 0.5 s by `scripts/scaling/anon_peak.sh`).

## What holds at scale (MEASURED)

| structure | largest N measured | result |
|---|---|---|
| pack index, multi-pack | 1e11 objects, 40 packs, 4.4 TB of index | build 3127 s, 44 B/object, open 5.7 ms, 91 MB anonymous; lookup hit p50 22.5 ms (cold-cache disk: 33 ms), miss p50 38 ms |
| pack index, 4 packs | 1e10 objects | build 292 s, open 0.5 ms, cold lookup p50 3.4 ms |
| commit-graph point queries | 1e9 commits | `index_of` 3 us, merge-base at small fork distance sub-ms to ms; fork distance 1e6 is 1.8 s |
| reach-bitmap membership, path-filter lookup, AGI5 open, sharded manifest commit | 1e9 / 1e8 / 2e8 / 1e5 zones | all flat or sub-linear (sharded commit 230 us, flat) |
| `add` of one file | 30M files | 7 ms (flat from 10k) |
| deep history `commit` of one file | 10M commits | 7 ms |

At 1e11 objects the pack-index lookup is disk-bound (22 to 38 ms p50 because the 4.4 TB of index no longer fits the 155 GB
page cache); open time and anonymous memory are still flat.

## What breaks first (MEASURED, linear in N)

| structure | largest N | time | memory | PROJECTED at 1e12 |
|---|---|---|---|---|
| commit-graph `decode` (whole file) | 1e9 commits | 55.3 s | 99 GB anonymous peak (51 GB resident after) | 14.6 h, about 100 TB |
| commit-graph `count_reachable_not_from` | 1e9 | 280 s | | 6 days |
| reach-bitmap `load_verify` | 1e9 | 19.1 s | 32.3 GB | 5.7 h, 32 TB |
| path-filter `load_parse` | 1e8 | 4.2 s | 6.5 GB | 12 h, 64 TB |
| commit-graph extend by one commit | 1e7 | 546 ms | | 11 days (O(n^1.2)) |
| manifest decode / locks decode | 1e6 zones / 1e7 locks | 1.7 s / 9 s | | 39 d / 19 d |
| AGI5 `scan_all_status_pass` | 2e8 files | 12.7 s | | 19 h |

The commit-graph, reach-bitmap and path-filter readers decode the whole file into heap structures. They must become
mmap-style lazy readers to reach 1e12; at 1e9 the commit-graph already needs 99 GB of RAM to open.

## End to end (MEASURED, vcs only; single runs)

Deep = N linear commits, one file touched per commit (4 objects per commit). Wide = one commit with N files in N/100 dirs.

| shape, N | import | status (first) | one-file `commit` | publish (first) | clone | gc (first / second) | fsck |
|---|---|---|---|---|---|---|---|
| deep 1M commits | 54 s | 0.02 s | 0.01 s | 40 s, 10.5 GiB RSS | 85 s, 2.6 GiB | 47 s / n.a. | 198 s, 5.9 GiB (15.6 GiB before batching) |
| deep 10M commits | 590 s | 0.02 s | 0.01 s | 405 s, **101 GiB** | 1596 s, 21.5 GiB | 602 s, 52.9 GiB / 54 s | 2926 s, 56.5 GiB |
| wide 10M files | 197 s | 23 s | 7.6 s | 35 s | 222 s | 52 s | 55 s |
| wide 30M files | 593 s | 73 s | 21 s | 110 s | 764 s | 214 s | 223 s |
| wide 100M files | 2327 s, 100 GiB | 1053 s / 183 s | **74 s**, 27.8 GiB | 702 s | **4631 s, 74.9 GiB** | 846 s / 428 s, 52.8 / 48.4 GiB | 1108 s, 47.7 GiB |

git on the same deep streams (single runs): 10M commits import 905 s, publish 576 s, clone 674 s, gc 511 s, fsck 479 s with
peak RSS under 15 GiB everywhere. So at 10M commits vcs is faster at import (590 vs 905 s) and `rev-list` (29 vs 49 s) but
faster at the second gc (54 s vs 449 s, the reach cache), but slower at clone (2.4x) and fsck (6x), and uses 4 to 8x more
memory in publish, gc and fsck. No git number exists for wide: git was not run there (`--no-git`; the 10M git import was
killed with rc -9 at 1694 s).

Wide-repo costs are linear in files and every step except `add`, `log`, `tag` and `publish_incremental` pays them:
one-file `commit` is 0.76 s @1M, 7.6 s @10M, 21 s @30M, 74 s @100M files (about 0.74 us/file, linear, as diagnosed below).
`status` first run reads every file: 1053 s at 1e8.

## Diagnosis: one-file commit is O(files) (wide repos)

`Repo::commit_signed` (`crates/vcs-repo/src/repo.rs`) pays two linear costs even when one file changed:

1. `self.index()` is `index_image()?.to_index()`: it decodes every entry into a `BTreeMap<String, IndexEntry>`; that map
   is what `treeops::IndexTrees` walks. RSS 8.3 GB at 30M files, 27.8 GB at 100M (about 285 B/file).
2. After the commit it stores the tree cache with `Op::SetTrees(trees_snapshot())`, which clones and re-encodes the whole
   cached-tree map (one entry per directory) instead of a delta.

`add` is fast because it already appends log ops. The index log already carries `DropTrees` (dirs invalidated by an edit),
so the dirty directories are knowable without decoding the index.

Proposed fix (NOT done; larger than a safe small change): lazy commit. Load cached trees (O(dirs)), take dirty dirs =
ancestors in `DropTrees` since the last `SetTrees`, rebuild only those from the block `IndexView` children ranges on top of
the parent tree, and append a delta `SetTrees`. Must keep the invariant that a cache never changes an answer:
property-test against the full-map path.

## Memory probes on a 1M-commit repo (4.0M objects), anonymous peak

Done after the driver finished its main steps, so the box was partly busy (wide-1e8 clone, gc and fsck ran concurrently for
part of this); wall times here are indicative only.

| operation | wall | anonymous peak | per commit | note |
|---|---|---|---|---|
| fsck (batched, already on this branch) | 150 to 243 s | 1.63 GB (+4.3 GB file pages) | 1.6 KB | the 5.7 GB `maxrss` is mostly mapped pack pages; PROJECTED 16 GB anonymous at 10M commits |
| publish (first) | 218 s | **10.8 GB** | 10.8 KB | `collect_packed` builds the whole pack in memory (`out: Vec<(ObjectId, Vec<u8>)>` plus the pack buffer): about 2.5x the 4.3 GB of data. 106 GB at 10M. Needs a streaming/segmented pack writer; not done |
| clone | 95 s | 2.7 GB | 2.7 KB | |
| gc (first) before the fix | 53.9 s | 5.1 to 5.2 GB | 5.2 KB | |
| gc (first) after the fix, cache off | 55.4 s | **1.67 GB** | 1.7 KB | same time within noise; results identical |
| gc (second, nothing to collect) | 6.3 s | 0.88 GB (maxrss) | | reach cache |

### gc fix (pushed)

`gc_plan_known` held, for the whole run, the decoded child list of every object it had read (`Known`), and read each level
into one `Vec` before using it. In a deep history every commit owns a 100-entry root tree, so that is about 1.3 KB per
object, linear. Two changes in `crates/vcs-repo/src/gc.rs`:

* objects are read and decoded 256k at a time (`GC_CHUNK`), so the transient set is bounded;
* the cross-pass cache is now size-budgeted (`VCS_GC_KNOWN_BYTES`, default 1 GiB); overflow is used for the chunk at hand
  and simply re-read by the second pass. Transactions are always remembered (they are gc roots).

The cache bought no measurable speed at 1M commits (53.9 s with, 55.4 s without), which is why the default is 1 GiB and not
larger. Single run each, on a busy box: this does not prove "no slowdown" at every scale, only that none was visible here.
Tests: unit test for the budget; `gc`, `gc_reach`, `tags`, `chunked`, `autorepack`, `worktree` suites pass with
`VCS_GC_KNOWN_BYTES=0` and `model` + `gc` + `gc_reach` pass at the default. The wide-1e8 gc (52.8 GiB RSS) was measured
before the fix and was not re-measured: its working directory was deleted by the driver when it finished, and a rebuild
takes about 90 minutes of import alone.

Remaining O(n) memory in gc after the fix: `marked` set, the sorted object-id list, and the per-level frontier.
That is about 100 to 150 B/object, not 1.3 KB.

## Priorities toward 1e12 (ranked by how far each is from feasible)

1. Commit-graph, reach-bitmap, path-filter: lazy/mmap readers (100 TB, 32 TB, 64 TB projected resident).
2. publish: streaming pack writer (10.8 KB/commit anonymous; 106 GB at 10M commits).
3. One-file commit in a wide repo: lazy commit (74 s at 1e8 files, linear).
4. fsck: 6x slower than git at 10M commits and linear in anonymous memory (1.6 KB/commit).
5. clone: 2.4x slower than git on deep history, 4631 s and 75 GiB at 1e8 files.

Everything is linear in N except the pack index and the point queries; no single-machine path to 1e12 exists for the
whole-file readers above. The measured evidence reaches 1e11 objects (pack index), 1e9 commits (commit-graph, reach-bitmap),
1e8 files (end to end), and 1e7 commits (end to end).
