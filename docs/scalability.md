# Scalability: what breaks first, and what was done

Audit of what degrades as repositories, refs, writers and connections grow. Every number below was measured by the
author of this chapter unless it says "not run". Tests: `crates/vcs-server/tests/scale_inflight.rs`, import unit test in
`crates/vcs-repo/src/import.rs`. Probe scripts: `scripts/scalability/` (`gen.py` makes `git fast-import` streams: `deep`, `bigdir`, `wide`, `tags`; the
`.sh` files expect the repo at `~/rt/scalability` and a scratch dir `~/rt/scalability-probe`, and import then time commands).

## Method and honesty notes

* Machine: AWS box E (c6i.4xlarge, 16 vCPU) shared with other agents; load1 was 4-16 during runs, so absolute times are
  noisy. Every cell is ONE run (no medians); treat differences under about 30 percent as noise. Timing runs took
  `flock ~/bench.lock`. Release builds. Synthetic data from `vcs import-git` streams (linear history, tiny files).
* Manifest numbers reuse `docs/manifest-scaling.md` (not repeated); only the 512-writer extension is new.
* Not compared against another tool here; no win/loss claims.

## Findings

| # | Risk | Evidence | Status |
|---|---|---|---|
| 1 | Accept loop spins on `EMFILE` (descriptor table full) | 1.00 s CPU per 1.00 s wall (a whole core) with the table full; test fails before the fix, passes after | **fixed** (#432): 50 ms back-off and a log line |
| 2 | Unbounded request-body memory: 256 connections x `max_body` 1 GiB | By construction (body is buffered in a `Vec`); no OOM was provoked | **fixed** (#432): server-wide `--max-inflight-body` (default 2 GiB), reserved from Content-Length before reading, RAII release, 503 + `Retry-After: 1`; test: second 800 KiB PUT is shed while the first stalls, budget returns after the stalled client leaves |
| 3 | Many idle connections | 250 idle connections held: 20 `/healthz` requests complete in under 5 s (threshold in test); cap of `max_connections` (256 default) returns 503 + Retry-After (existing test) | **ok**; thread per connection is the ceiling (see Designs) |
| 4 | Slowloris, header/body size caps, per-IP caps, auth throttle | Existing tests `slowloris_stalls_time_out`, `size_limits_give_clean_errors`, `abuse_limits.rs` | **already covered**, not re-run beyond the package suite |
| 5 | Import of a huge directory is quadratic (`remove_prefix` scanned the whole tree per file) | One commit, 300,000 files in one directory: over 4.5 min unfinished at 100 percent CPU (perf: 63.5 percent in `BTreeMap::retain`) before; **24.0 s**, 322 MB RSS after | **fixed** (#434) |
| 6 | Manifest CAS under 512 writers | Direct to the store (Mem backend, 249 KB manifest, one run): 512 writers 41/s, p95 25.5 s, 92.8 percent CAS lost. Through the server over HTTP, same binary, 3 runs, medians, box E load1 17-22: **group commit off: 27.3/s, p95 37.2 s, 96.9 percent lost, 67.8 requests/publish; on: 69.7/s, p95 8.5 s, about 0 percent lost, 5.3 requests/publish** (64 writers: 42.2/s, p95 3.4 s, 87.6 percent lost off; 57.3/s, p95 1.4 s, 0 percent on) | **fixed for clients of vcs-server** (#452); direct-to-store publishers (S3 without a server): **per-zone sharded manifest, opt-in** (#313, `vcs-server migrate-sharded`): in-memory backend with 25 ms per request, 64 disjoint writers, one run: single 27.6 pushes/s, p99 7.7 s, 3,701 CAS lost; sharded 58.9 pushes/s, p99 2.1 s, 0 lost, but 169 instead of 28 requests per publish because a manifest read composes every zone (see manifest-scaling.md, last chapter). Through the server on a local fsync disk sharding is SLOWER (46.7 vs 9.6 pushes/s, 32 writers): batching one fsync per batch wins there. On real S3 the merge still cost one PUT per push, see finding 15 |
| 7 | Manifest size | See manifest-scaling.md: linear, about 40-63 bytes per branch, 1 MiB at 17-26k refs; 2.6 MB at 10k zones x 5 | **designed**, unchanged |
| 8 | Refs file is read and rewritten whole (`.vcs/refs`) | 10k tags + 1 branch: tag create 0.04 s. 100k tags (4.4 MB file): tag create 0.18 s, `log -n 1` 0.09 s, `tag` list 0.37 s, gc 0.69 s. **1M tags (44.9 MB): tag create 1.85 s / 522 MB RSS, branch create 1.42 s, `log --oneline -n 1` 1.11 s / 265 MB, `tag` list 3.91 s, gc 8.5 s / 541 MB** (single run each). Import of 1M tags 3.65 s | **fixed** (2a, 2b, and the delta log 2c, #527; numbers in "Refs delta log" below). Earlier: partly fixed (#436 item 2a): the tag table is validated without allocating and parsed only when a command looks at it; an untouched table is written back byte for byte; `tag` list peels each distinct target once. Commands that record an operation still parse (the op view stores the set; #454 will make that cheap). Numbers below the table. The delta log (2c) is built: see below |
| 9 | Deep history | 100,000 linear commits: import 74.4 s (3 runs, median); first `rev-list HEAD` after import **1.78 s** (3 runs: 2.15, 1.68, 1.78; it builds the commit-graph cache), later `rev-list HEAD` 0.10 s, `log -n 20` 0.01 s, `merge-base` 0.02 s, `gc` 9.6 s / 91 MB (single run). An earlier single run here measured 13 s for the first rev-list under load 13; the medians supersede it | **fixed** (#449): graph built at the end of import and gc, first rev-list 0.103 s (3 runs). gc stays linear (about 0.1 ms per commit). **1M commits run** (box f, below) |
| 10 | Wide trees | 300,000 files in 300 dirs x 100 subdirs: import 29.0 s / 357 MB; **first `status` after import 22.2 s** (flat dir: 6.2 s); later `status` after one edit 0.26 s, commit 0.17-0.38 s, `ls-files` 0.05 s, `show --stat` 1.7 s, gc 2.3 s (single runs) | **investigated, no easy win**: `perf` of the first status (the index has no stat data after import) is flat, dominated by lstat/open/read syscalls and blake3 of every file (top symbol 5 percent). The tracked scan is parallel now (f1d87bb, `VCS_STATUS_THREADS`). Re-run on a macOS laptop (APFS, same 300,000 files in 3,000 dirs, busy machine, single runs): `import-git` then **first `status` 15.2 s**, next three clean `status` 7.8, 4.3, 4.2, 3.6 s; `git status` on the same tree 5.8, 13.7, 8.4 s (git checkout of the same stream, no fsmonitor). No win or loss claimed: the box was noisy, and both are bound by lstat of 300k files. 10M-file trees not run. With the watcher (#293) the same clean `status` costs 7 ms CPU on a 100k-file tree in 2,100 directories (git 720 ms CPU, see benchmarks.md); on macOS the watcher uses one recursive FSEvents stream, because one stream per directory had not started after 3 minutes |
| 11 | GC at scale | Above: 100k commits 9.6 s, 300k files 2-2.3 s, 1M refs 8.5 s (all local, unlocked, no concurrent push). Memory grows with objects (91 MB at 100k commits, 541 MB at 1M refs) | **measured**; remote GC with concurrent pushes at scale **run on the in-memory backend** (local, not S3; see "Remote GC and the lock service at scale" below). It found and fixed a bug: GC read the roots after listing packs and aborted as damaged when a push landed in between (#524) |
| 12 | Lock/lease service with thousands of holders | Locks live in the manifest, so each holder adds bytes to the one CAS object and each acquire/release is a manifest publish (finding 6 applies) | **run on the in-memory backend** (2,000 holders: 1,005 acquires/s, 6,000: 156/s; see below). It found a decoder bug (a compact manifest above 64 KiB with many locks was refused, fixed in 8b9ee0f); each acquire is still a whole-manifest publish, so cost per acquire grows with holders |
| 13 | S3 request shaping, 503 SlowDown | Fault-injecting backend (`crates/vcs-remote/tests/slowdown.rs`, #447): per-prefix budget per 100 ms, 24 publishers x 4 publishes through `RetryBackend` defaults, one run each: 17, 37, 55 percent of requests shed: all 96 publishes complete; 74 percent shed: 20 of 96 give up with `Unknown` (retries exhausted, state unknown, safe to rerun by txid). Object keys fan out over 256 prefixes; `manifest` is one key | **tested in-process** (not RustFS/MinIO/AWS). **Adaptive shared throttle built** (this change, `RetryBackend`, env `VCS_REMOTE_MAX_CONCURRENCY`, default 64, 0 = off): AIMD concurrency cap shared by every thread using one client object, halved on 503/SlowDown/429 (once per shared cool-down), +1/limit per success, requests wait for the shared cool-down instead of burning retries. Test `shared_throttle_vs_none` (box: i4i 32 vCPU, release, 2 runs each, same fault injector, ONE shared `RetryBackend`, 3 retries, base 20 ms): **24 writers x 4, 20 requests/100 ms/prefix: no throttle 51% shed (625 and 656 of ~1,250 requests), 1.1 s; throttle on 10-11% shed (115 to 135 of ~1,150), 3.6 to 3.9 s; all 96 publishes complete in both. 48 writers x 4, 10 per 100 ms: no throttle 132 and 140 of 192 publishes gave up (`Unknown`, 83-84% of requests shed); throttle on 0 gave up (192 of 192), 18-19% shed, 18.5 to 20.4 s.** So it trades latency for requests and for not losing publishes (the publish is safe to rerun by txid either way). Per-thread clients (the old shape, nothing shared) are unchanged; a process gets the benefit only if its threads share one client. Still not run against RustFS/MinIO/AWS |
| 14 | Every operation stored the whole tag set (op log growth) | 100,000 tags: **4,389,153 object bytes per commit/branch/tag operation** (3 runs, identical) | **fixed** (#454): unchanged sets of 64 or more tags are omitted: 260 bytes per operation; feature `op-tags-delta` gates older binaries; `vcs branch` time 0.13 s before vs 0.14-0.23 s after under load (not shown to be equal) |
| 15 | Manifest PUT per push on real S3 | **Real S3 us-east-1** (c6i.4xlarge, one run per cell): disjoint writers, 0 conflicts, yet **19 to 22 pushes/s at 64, 256, 512 and 1024 writers** (p99 3 s to 54 s): one conditional PUT (~50 ms) per push under one lock; server idle (6 CPU s per 2048 pushes) | **fixed** (this change): flat-combining batch commit, one PUT per batch: **215 to 350 pushes/s at 256 to 1024 writers, p99 1.3 to 7.4 s** (10 to 17x); local disk 138 to 288 before, 654 to 1,794 after. [many-writers-s3](research/many-writers-s3.md) |
| 16 | All writers on ONE branch | Retry storm: disk 6.3 pushes/s at 256 writers (77,639 conflicts, 66 CPU s), 0 acked in 240 s at 512; S3 1.4/s at 64. Batch + decorrelated jitter: S3 6.4/s at 64, alive at 256 (6.7/s) but p99 30 to 110 s | **fixed with an opt-in per-branch ticket queue** (`POST /v1/ticket?zone=&branch=` blocks FIFO until it is the writer's turn, `DELETE` releases, 15 s lease; `HttpBackend::ticket_acquire`, `writerbench --ticket`). LOCAL, vcs-server over a disk backend, box load1 30-68 so absolute numbers are inflated (about 10x slower than the S3 chapter's disk rows) and only the pairs mean anything, one run per cell: **64 writers x 3 pushes: decorrelated-jitter polling 0.8 pushes/s, p99 228 s, 5,878 conflicts; tickets 5.5/s, p99 12.1 s, 0 conflicts. 256 writers x 1: polling 0.3/s, p99 877 s, 18,730 conflicts; tickets 18.9/s, p99 13.5 s, 0 conflicts.** 512 writers x 1 with tickets: 20.5/s, p99 24.8 s, 0 conflicts (no polling cell at 512: it did not finish at 256 in 15 minutes). Advisory: a push without a ticket behaves as before; correctness never depends on it |

### Lazy tag table (macOS arm64, release, 300,000 lightweight tags to one commit, 13.4 MB `.vcs/refs`)

Median of 5 runs per cell on a loaded laptop (load average 40+, so absolute times are inflated; base and new alternate on identical copies of one repository). Base is origin/main at 4a2bc5e.

| command | before | after |
|---|---|---|
| `log --oneline -n 1` | 0.307 s | 0.062 s (4.9x) |
| `status` | 0.187 s | 0.055 s (3.4x) |
| `branch bq` (records an operation, so parses tags) | 1.432 s | 0.275 s (5.2x, bulk build of sorted input) |
| `tag` (list) | 7.459 s | 1.153 s (6.5x, one object read per distinct target instead of per tag) |

Tests: `reftags.rs` unit tests (raw re-emit is byte identical, mutation drops the raw bytes, corrupt names/truncation/trailing bytes refused at decode, unsorted tables still decode).

## Refs delta log (#436 item 2c, #527)

Format (code: `crates/vcs-repo/src/refsdelta.rs`, `reftags.rs`; gate: feature `refs-delta` in `.vcs/features`, written before the
first AGR2 file; older binaries refuse the repository). From `VCS_REFS_DELTA_MIN` tags (default 50,000; `VCS_REFS_DELTA=0`
keeps the single file) `refs` becomes `AGR2`: branches, snapshots and the id and count of the tag table, a content-addressed
file `reftags.<blake3>`. `refs.log` has a header naming the hash of the `refs` file it extends and one checksummed record per
operation (changes to branches, snapshots and tags as absolute values, consecutive sequence numbers). Reading `refs` reads the
small file and the log; the tag table is read when a command looks at tags, and `Tags::get_key`/`set`/`unset` (tag create,
delete, fetch) never parse it. Compaction (log over max(256 KiB, tag bytes / 4)) writes the new table, then replaces `refs` by ONE
rename, which makes the old log stale (its header names the old hash); old tables are removed after 5 minutes so a concurrent
reader is not cut off. Operation log: an operation stores the tag changes (`OpView::tags_delta`, full copy every 64) located
through a token over base and log, so `record_op` does not read the table either; gc keeps delta targets as roots. The ref journal
declines in this mode (it already declined at 64+ tags). Crash story: every state-file write is a failpoint; a torn append is
dropped by its checksum and the next append truncates it; the conversion and the compaction are table first, one rename second.
Tests `crates/vcs-repo/tests/refs_delta.rs`: random operations equal a model across reopens, log truncated at every byte and
corrupted at every third byte never misread, crash at every write while converting and while appending, compaction, undo and
restore through tag deltas.

Not done: the branch table is still loaded whole (it is small; per-zone lazy loading of branches would need the same split), and
`tag` listing and `gc` still parse the whole table (they need all of it).

### Numbers: 1,000,000 lightweight tags (LOCAL: macOS arm64, release-like dev profile opt-level 1, machine at load 100-240 from other agents, single runs)

`legacy` is `VCS_REFS_DELTA=0` on the same binary (the single 44.9 MB file, with the lazy tag table of item 2a already in).

| command | legacy | delta log |
|---|---|---|
| import-git (1M tags) | 7.2 s / 562 MB | 5.4 s / 630 MB |
| tag create | 1.93 s / 462 MB | 0.46 s / 93 MB (second run 0.34 s) |
| tag delete | 1.76 s / 454 MB | 0.14 s / 5 MB |
| branch create | 1.12 s / 299 MB | 0.26 s / 5 MB |
| log --oneline -n 1 | 0.31 s / 94 MB | 0.019 s / 8 MB |
| status | 0.31 s / 94 MB | 0.08 s / 8 MB |
| tag (list) | 1.70 s / 520 MB | 1.66 s / 470 MB (needs every tag) |
| gc | 6.4 s / 665 MB | 1.8 s / 481 MB |
| state on disk after the runs | `refs` 44.9 MB | `refs` 84 bytes + `reftags.<hash>` 44.9 MB + `refs.log` 231 bytes |

At 100,000 tags: tag create 0.43 s / 51 MB to 0.22 s / 16 MB, tag delete 1.31 s to 0.17 s, branch create 0.94 s to 0.20 s, gc 0.90 s to 0.26 s.
Harness: `scripts/scalability/refs_bench.py BINARY N WORKDIR [legacy]`. The remaining 0.3 to 0.4 s of a tag create is the duplicate check
(read and hash of the 45 MB table) plus process start and the object barrier.

## Remote GC and the lock service at scale (#436 item 5, #524; LOCAL: in-memory backend, not S3)

Test `crates/vcs-remote/tests/scale_locks_gc.rs` (env `SCALE_GC_COMMITS`, `SCALE_GC_ORPHANS`, `SCALE_GC_WRITERS`, `SCALE_GC_PUSHES`,
`SCALE_LOCK_N`). Loaded machine, single runs; the backend is `MemBackend`, so no network latency.

* Remote GC while writers push: 1,500 published commits (one pack each), 300 aborted publishes (old orphan packs), then GC with 4
  writers pushing 40 commits each: all pushes acknowledged, every branch landed, closure of the heads complete afterwards (G-REACH),
  900 orphan objects and 300 packs swept, 44 ms wall for GC plus pushes. Larger: 8,000 commits, 1,500 orphans, 8 writers x 60 pushes:
  28.6k objects in 9.5k packs, 4,500 objects and 1,500 packs deleted, 0.40 s wall, G-REACH held. Building the 8,000-commit history took
  122 s (one publish each under load). **Bug found and fixed:** GC listed packs and then read the roots; a push in between made a head
  whose pack the listing lacked and the sweep aborted as "damaged" (safe, but GC could not finish while anyone pushed; failed 10 of 10
  runs with one writer). Roots are read first now. The exhaustive interleaving search (`gc_real.rs`, `model_gc.rs`) still passes.
* Lock service: N principals each acquire one lock (a manifest publish each): 2,000 holders 1,005 acquires/s (first 50 acquires 23 us
  each, last 50 1.8 ms), manifest 113 KB (56 bytes per holder); 6,000 holders 156 acquires/s (first 112 us, last 6.0 ms), 281 KB. Per-acquire
  cost grows with holders because the whole manifest is decoded, validated and re-encoded per publish. The run also exposed that a
  compact (v5) manifest with many locks in one zone was refused at decode (fixed upstream in 8b9ee0f, regression covered by this test).

## 1,000,000 commits (#436 item 5; box f = AWS c6i.4xlarge, 16 vCPU, 30 GB, EBS gp3, release build, single run, under `flock ~/bench.lock`)

`gen.py deep 1000000`: 1M linear commits, each touching one of 1,000 files in 100 directories, plus 100,000 tags on every tenth
commit (so the repository is in refs-delta mode). Stream 201 MB.

| step | wall | peak RSS |
|---|---|---|
| `import-git` (graph written at the end) | 1,091 s (1.1 ms per commit; 74 s per 100k earlier, so about 1.5x per-commit slower at 10x) | 2.2 GB |
| `.vcs` on disk | 4.3 GB | |
| first `rev-list HEAD` (prints 1M ids) | 1.53 s | 259 MB |
| second `rev-list HEAD` | 1.56 s | 259 MB |
| `log -n 20`, `log --oneline -n 1`, `status`, `branch`, `tag` | under 0.02 s each | under 25 MB |
| `gc` | 40.0 s | **5.8 GB** |
| `rev-list HEAD` after gc | 1.56 s | 259 MB |
| `fsck` | 112 s | **16.2 GB** |

Reading: everything that walks a prefix of history (`log`, `status`, tag and branch creation) is flat in history length. `rev-list
HEAD` is linear in the output (1.5 us per commit, the commit-graph is used). `gc` (40 s) and especially `fsck` (112 s, 16 GB)
hold the whole object set in memory: that is the first thing to break at this scale, not a time problem. Not done: bounding the
memory of `gc` (streaming marks). `fsck` now reads and decodes objects in batches of 8192 and keeps only zone, root and parents per
commit, so it no longer holds the decoded store; the 16 GB figure above is the old code and is not re-measured at 1M commits yet.

## 10,000,000 files (box f, release, single run, one commit of 10M files in 100,000 directories)

`gen.py wide 10000000` (stream 763 MB): `import-git` **853 s, 11.5 GB peak RSS**, `.vcs` 1.9 GB. The first `status` (nothing
cached, 10M lstat on a cold gp3 volume) had run **30 minutes at 9 percent CPU (IO bound, 1.5 GB RSS) without finishing** and was
killed, so the first-status time at 10M, and the later commands, are NOT measured. Memory at import (about 1.1 KB per file) is
the first limit; the first status is bound by random metadata reads, as at 300k files (finding 10).

## Clone and fsck at scale (i4i.8xlarge, 32 vCPU, 247 GB, NVMe; 2026-11-06; MEASURED, before and after)

Why: at 10M commits `clone` was 2.4x and `fsck` 6x slower than git, and `fsck` used 56 GiB (see `docs/research/trillion-scaling-findings.md`).
Profiles (`perf` with frame pointers) found the real causes, none of them I/O:

| operation | cause (measured) | fix |
|---|---|---|
| `fsck` | one thread read, hashed and decoded every object in id order (`store.get`, random pack access, one decoded block per object in v3 packs); `verify_packs` then hashed every pack again; a `remote_closure` BTreeSet walk of the whole repository (17 percent of the profile, single thread) ran even for a healthy repo; one pack-index probe per tree entry (1e9 at 10M commits); a `BTreeMap` of every commit and a `HashMap` of every id | `crates/vcs-repo/src/fsck.rs`: one pass over each mapped pack in pack order on every core (`Pack::scan`, shared with `verify_packs`, so the pack is hashed once); per-object state is 8 bytes (`facts`), commits keep zone, root, parents as indexes; existence of a referenced id is a bucket-index lookup in the sorted id list, prefetched a tree at a time (a plain binary search over 40M ids was 15 dependent cache misses per lookup and 70 percent of the time); remote closure only when something is missing, and then one parallel reachability walk; the commit-graph, ops, path-filter and reach-cache checks run alongside the object pass; index blobs are checked by streaming the index blocks on every core |
| `clone` | single-threaded post-order DFS: its stack held every commit, its read-ahead cache every metadata object (21.5 GiB), `known_end` was 22 percent (quadratic over pending ids), every object paid decode, two hash probes, `contains` and a store `put` on one thread; the file prefetch of a wide repository put each blob through the pending map | `crates/vcs-remote/src/bulkmeta.rs`: a reader streams each remote pack's commit and tree prefix, workers hash and strictly decode on all cores and check every named dependency against a sharded id set, a writer appends bounded local packs; holes (loose objects, metadata outside a prefix) fall back to the old walk; large file prefetches are written as streamed packs; `collect_missing_files` builds no path strings without a sparse set |

Method: single runs on the shared box. The box timing lock was held for hours by another job (`bench_status_fair.py`), so these
runs were **not** under `flock`: they ran at `nice -n 19` with load average 1 to 7 (mostly single-threaded jobs of other agents),
so wall times are an upper bound, and a first run after other jobs evicted the page cache is slower (fsck 1M: 4.6 s warm, 11.1 s cold).
Peak memory is anonymous RSS sampled every 0.5 s (`scripts/scaling/anon_peak.sh`); mapped pack pages are not counted (they are
reclaimable page cache; they are what git's 4 GB "peak_file" does not show). Same generated data feeds vcs and git
(`scripts/scalability/gen.py deep|wide`). "Before" is the previous binary on the same box except where marked (10M deep: the
earlier driver run on the same generator; 10M deep was not re-run with the old code, it would take 4.5 hours).

Deep history (one file touched per commit, 4 objects per commit):

| | before (vcs) | after (vcs) | git |
|---|---|---|---|
| 1M commits `fsck` | 180.5 s, 6.0 GB maxrss | **4.6 s** warm / 11.1 s cold, 0.52 GB | 39.8 s to 50.3 s, 0.48 GB |
| 1M commits `clone` | 92.6 s to 100.4 s, 2.71 GB | **13.1 s to 14.1 s**, 1.43 to 1.54 GB | 63.4 s to 65.0 s |
| 1M `fsck` of that (lazy) clone, 999,000 blobs not cached | 112 s to 120 s, 3.7 to 4.2 GB | **5.6 s to 8.1 s**, 1.0 GB | n/a |
| 10M commits `fsck` | 2926 s, 56.5 GiB | **53 s to 68 s**, 4.7 GB | 520 s, 4.3 GB (earlier run: 479 s) |
| 10M commits `clone` | 1596 s, 21.5 GiB | **175 s to 185 s**, 6.5 to 6.9 GB | 697 s (earlier run: 674 s) |
| 10M `fsck` of that (lazy) clone, 9,999,000 blobs not cached | not run | 131.8 s to 148.6 s, 8.5 to 9.3 GB (first version: 385 s) | n/a |

Wide (one commit, N files, N/100 directories):

| | before (vcs) | after (vcs) | git |
|---|---|---|---|
| 1M files `fsck` | not run | 0.50 s, 0.8 MB | 3.5 s, 0.27 GB |
| 1M files `clone` | 12.6 s | 13.1 s (checkout bound) | 13.1 s (but git did not check out: HEAD was not `main`) |
| 10M files `fsck` | 55 s (findings) | **3.5 s** warm / 16.6 s cold, 1.24 GB | not run (git import killed at 10M) |
| 10M files `clone` | 151 s, 8.4 GB | **137 s**, 6.8 GB | not run |
| 100M files `clone`, `fsck` | 4631 s, 74.9 GiB / 1108 s, 47.7 GiB | **not re-measured** (import alone is 2300 s and 100 GiB) | not run |

What it shows:

* `fsck` is now faster than git at 10M commits (53 to 68 s against 520 s, 9x), memory is about 120 B per object
  (4.7 GB at 40M objects, 120 B per object; 13x lower than before) and the wide case is 7x faster than git at 1M files.
* The 10M-commit `clone` is 3.8x faster than git and 9x faster than before. It was not profiled again after the fix; the
  candidates are the single-threaded pack write (one `fsync`ed segment at a time), `adopt_remote_tags` (1M tags) and the checkout.
* The wide `clone` did not get much faster: 10M files are 10M `creat` calls, 17 s of `sys` time per 1M files, and the index
  (`transition_apply_ws`, `Index::insert`, a `BTreeMap`) is built in memory. That code belongs to the checkout and index owners.
  Its first version of the streamed prefetch made it 36 percent SLOWER (204 s vs 151 s): every store lookup tries the packs one
  after the other and 25 packs of 400k blobs cost 47 percent of the run in `PackIndex::search`; segments are 2 GiB now.

What the profile showed that the first fix missed (kept here because it will matter again):

* An id lookup is memory-latency bound: the first `IdSet` was an interpolation search and left `memcmp` at 70 percent of
  fsck (about 15 dependent misses per lookup); a bucket table plus prefetching a tree's 16 entries at a time cut CPU 3.6x.
* A lazily cloned repository misses every file blob. Building one message per reference was 4 GB and 110 s at 1M commits
  (200 B x 1e8 references at 10M); a missing id is recorded once and its message rebuilt from the referrer only when reported.

Tests: `crates/vcs-repo/tests/fsck_batches.rs` (batch size of 3 vs 1,000,000 on loose objects with a missing chunk) and
`fsck_packed.rs` (the same on packed objects, plus a flipped byte in a pack); `crates/vcs-remote/tests/bulk_meta.rs` (bulk fetch
equals the plain walk over several packs, fills holes left by loose objects, declines a remote without packs, rejects a tampered
pack). The full `vcs-store`, `vcs-repo`, `vcs-remote`, `vcs-cli` suites pass.

Not done: a multi-pack index (every `LocalStore::get` still tries the packs in turn; a bulk clone writes 1 pack per 2 GiB),
`Block::decode` allocates a `HashMap` per v3 block (a 1M-commit v3 pack took 16 s before the bucket index; not re-measured), the single-threaded segment
write and `fsync`, the 47 s commit-graph check at 10M commits (`cgraph.rs`, not touched; it now overlaps the object pass, so it
is the floor), zone-filtered clones (they still use the object-by-object walk), and the 1e8-file runs.

## Soak audit with the delta layout

`VCS_REFS_DELTA_MIN=0` (every repository in refs-delta mode, branches through the log) with `scripts/soak/soak.py run --kind vcs
--backend local --n 24 --minutes 6 --chaos kill9 --clientkill 0.03` (macOS, dev-profile binaries, local disk backend): 1,357 acknowledged
files, 0 missing, 0 wrong content, fresh-clone and copy-of-store `fsck` ok, remote gc dry run clean, 0 bad audits. It first
found a real bug (a delta repository with zero tags tried to read a nonexistent table; fixed, test
`a_delta_repository_with_no_tags_works`). Short run, not the 2 h soak; `server_alive_at_end` was false after the last chaos kill.

## Lazy mmap readers: commit-graph, reach cache, path filters (trillion-scaling follow-up)

Problem (docs/research/trillion-scaling-findings.md, MEASURED): the three cache readers decoded or verified the whole file on
every open: commit-graph 55 s and 99 GB anonymous at 1e9 commits, reach cache 19 s and 32 GB at 1e9 objects, path filters
4.2 s and 6.5 GB at 1e8 commits. Now all three map the file and open in O(1); `fsck` (and gc, for the destructive reach
cache) does the whole-file validation explicitly.

What changed:

* `vcs_store::mapfile::MapFile`: read-only `mmap` (heap bytes off unix), `MADV_RANDOM`; `blake3_big` hashes on all cores.
* Commit-graph **format v2** (gated by the version byte; `crates/vcs-merge/src/cgraph.rs` has the layout). v1 entries are
  variable length, so a lazy reader is impossible without a decode; v2 is columns (ids, times, generations, parent-end offsets,
  parents) with a 32-byte checksummed header. A v1 file still loads: it is fully validated, converted in memory and written back
  as v2 on first use (test `old_format_graph_still_loads_and_is_rewritten_as_v2`). Reach cache and path-filter formats are
  UNCHANGED (they were already random access).
* Open checks only the header and the exact file length (truncation is always caught) and, for files up to 4 MiB, the whole
  checksum and structure. Bigger files are not checksummed at open. Every query step on a lazily opened graph checks what it
  touches (parent index in range, generation strictly above the parents'); a violation poisons the graph (`is_bad`), the query
  answers `None`, and callers fall back to walking objects (`merge_bases`, `is_ancestor` check it explicitly). Path-filter
  lookups treat a malformed record as absent. Not detectable without a full pass: a flipped bit inside a time, a bloom filter or
  a reach bit that keeps the structure valid. That is what `fsck` is for (checksum plus structure plus comparison with the
  real commits; it now also checks `gc-reach`). **gc keeps verifying `gc-reach` completely** before trusting it (it deletes what the
  cache calls unreachable), but streams the check over the mapping on all cores, with no anonymous memory.
* `count_reachable_not_from` was O(history behind `hide`) (22 s at 1e8, 280 s at 1e9, allocated two bitmaps of n); it is now
  `range_ids(from, hide).len()`, which walks only the answer and the commits between it and `hide`.
* Path-filter save merges the sorted file and the new filters record by record (no `BTreeMap` of the whole file) and refuses to
  write when the 32-bit offsets of the format would overflow (about 1.4e8 commits of 30-byte records; this limit is NOT
  lifted, see open work).

Measured on the shared trillion box (i4i.8xlarge, release build, synthetic structures from `scalebench`, single runs, so read
differences under 10 percent as noise; the files had been written shortly before, so most of each file was in page cache):

**1e8 elements, before vs after, both under `flock /tmp/box-timing.lock`, one run each** (`scalebench cgraph|reach|pfilter`;
before = parent commit `4aeb6ab^`, after = this change; times are p50 of the samples named in `scalebench`, memory is anonymous):

| structure, operation | before | after |
|---|---|---|
| commit-graph, open | decode 5.83 s | **0.25 ms** (file mapped, header and length checked) |
| commit-graph, anonymous memory | 9.94 GB peak, 5.1 GB resident | **0.14 MB at open, 2.1 MB after all the queries below** |
| commit-graph, explicit full validation (fsck) | included in decode | 1.50 s, +1.8 MB (checksum on all cores, then structure) |
| commit-graph `index_of` | 2.2 us | 2.1 us |
| commit-graph `merge_base`, fork distance 1 / 100 / 1e4 / 1e6 | 305 us / 744 us / 49.0 ms / 401 ms | 329 us / 758 us / 49.2 ms / **435 ms (+8 percent)** |
| commit-graph `range_ids` (1000 commits) | 2.20 ms | 2.19 ms |
| commit-graph `count_reachable_not_from` (1000 commits) | **21.7 s** | **2.19 ms** (same answer, property-tested) |
| reach cache, open (`load_verify` before) | 2.79 s, 3.15 GB | **0.30 ms, 0 anonymous** |
| reach cache, `member_lookup` / `member_miss` / `roots_within_256` | 1.3 / 1.1 / 0.2 us | 1.2 / 1.2 / 0.2 us |
| reach cache, explicit full verify (what gc now runs before trusting it) | included in load | 3.6 s cold from NVMe (42 ms with the file in page cache), +2.2 MB |
| path filters, open (`load_parse` before) | 8.18 s, 6.35 GB | **0.27 ms, 0 anonymous** |
| path filters, lookup | 1.8 us | 1.8 us |
| path filters, explicit full verify (fsck) | included in load | 1.76 s, +1.8 MB |

**1e9 elements, after only** (the file was written by the same program a few minutes earlier, first run, NOT under the lock;
the "before" at 1e9 is the earlier measurement in `docs/research/trillion-scaling-findings.md`: commit-graph 55.3 s and 99 GB
anonymous, reach cache 19.1 s and 32.3 GB):

| structure, operation | after |
|---|---|
| commit-graph (52.4 GB file), open | **0.17 ms**, 0 bytes anonymous; 0.35 MB anonymous after all the queries below |
| commit-graph `count_reachable_not_from` (1000 commits) | 2.7 ms (was 280 s) |
| commit-graph `merge_base`, fork distance 1 / 100 / 1e4 / 1e6, `range_ids` | 0.33 ms / 0.97 ms / 50.7 ms / 1.66 s, 2.8 ms (the file was not fully in page cache: the first touches paid NVMe latency, p95 of the 1e6 case was 43 s; the 11 GB mapped resident after the queries is page cache) |
| commit-graph `index_of`, cold | p50 0.94 ms (page faults on a partly evicted file; 2 us once resident, as at 1e8) |
| reach cache (32.3 GB file), open | **0.12 ms**, 0 anonymous; full verify 23.8 s streaming over the mapping, +2.9 MB anonymous |
| reach cache `member_lookup` / `member_miss` | 1.7 / 1.7 us |

Notes: RSS of the "after" rows includes mapped file pages (reclaimable page cache); the anonymous column is `RssAnon`.
Cold point queries are bounded by the disk, not by this change: right after the 1e9 file was written and partly evicted,
`index_of` p50 was 0.94 ms (about 17 page faults per lookup on NVMe) instead of 2 us. The one slowdown, +8 percent on the
deepest merge-base walk (1e6 commits, 1e8 graph), was seen in both the locked run and an unlocked run (446 ms) and is probably
TLB misses: random reads of a file mapping use 4 KiB pages where the old heap vectors got transparent huge pages. The
per-parent generation check that makes lazy reading safe is paid only when a parent is pushed (an earlier version read it for
every edge and measured the same +8 percent, so it is not the cause).

Open work: the commit-graph and path-filter files are still rewritten whole when they grow (`extend` is O(n) and builds the
new image in memory); `fsck` still compares every graph entry with its real commit; the path-filter format has 32-bit data
offsets (writing is refused past about 1.4e8 commits, readers are fine); a bit flip inside a lazily opened body that keeps the
structure valid (a commit time, a Bloom filter byte, a reach bit) is found only by `fsck` or, for the reach cache, by gc's own
check; a graph that a query finds self-contradicting is deleted and rebuilt on next use. Test hook:
`VCS_CACHE_EAGER_VERIFY_MAX` (bytes) changes the size up to which a commit-graph is verified completely at open (default 4 MiB).

## Streaming publish (bounded memory; trillion box, i4i.8xlarge, release, single runs under `flock /tmp/box-timing.lock`)

Before, a packed publish (`Publisher::collect_packed`, crates/vcs-remote/src/protocol.rs) held every new object's encoding in one
`Vec`, then `split_packs` and `build_pack` built every pack up front: about 2.5x the data, 10.8 KB per commit anonymous.
Now Collect keeps only `(id, kind, encoded length)` per object (64 bytes) and buffers encodings up to a bound
(`VCS_PUBLISH_MEM_BYTES` or `Publisher::with_pack_mem`, default 1 GiB). Past the bound an object is re-read from the local store
(`get_encoded`, in parallel) when its pack is built. Each pack (`max_pack_bytes`, 256 MiB) is built in its `PutPack` step and dropped
after its index is up. Peak is about the bound plus two packs plus 64 bytes per new object, whatever the transaction size.
Same order, same format: the pack files are byte-identical for every bound (test `stream_publish.rs`; on the box the sha256 over
all 16 packs of the 1M-commit publish is the same for the old binary (second run) and the new one at bounds of 256 MiB, 1 GiB and
8 GiB). Caveat: the very first publish after an import wrote a different pack set than the later ones, for the old binary at 1M
and for the new one at 10M (the repository's own state changes at its first publish; not investigated), so old-vs-new byte
identity is shown on later publishes only. No format change, no gate: old readers read the same packs.

`gen.py deep 1000000` (4.0M objects, `.vcs` 4.3 GB), first `vcs publish` to an empty directory remote, `/usr/bin/time` and
`RssAnon` sampled every 0.5 s (`scripts/scaling/anon_peak.sh`). MEASURED, single runs; the first run of each binary hit a cold page
cache and a busy box (other agents' jobs), so only the warm runs compare; differences under 20 percent are noise.

| run | wall | anonymous peak |
|---|---|---|
| old (parent commit), cold | 267 s | 10.7 GB |
| old, warm | 44.8 s | 10.7 GB |
| new, default 1 GiB bound, cold | 114 s | 2.9 GB |
| new, default 1 GiB bound, warm | 42.9 s | 2.9 GB |
| new, bound 8 GiB (everything buffered) | 42.3 s | 6.7 GB |
| new, bound 256 MiB | 40.3 s | 1.9 GB |

10,000,000 commits: first publish of a 10,000,000-commit deep repository (40M objects, `.vcs` 44.6 GB, 158 packs, 42.9 GB on the remote), same box,
same method. The old binary is the parent commit; MEASURED, one run each, new first (so the old run had the warmer page cache):

| run | wall | anonymous peak |
|---|---|---|
| old | 771 s | **104.5 GB** (101-106 GiB reported before) |
| new, 1 GiB bound | 803 s | **9.9 GB** (10x less) |

Wall time is within noise (4 percent apart, one run each; the new run was first and cold). The 9.9 GB at 10M against 2.9 GB at 1M
is the O(n) part: the walk's `known`/`done` id sets (the same on both paths) and 64 bytes per object of plan, about 1 KB per commit.
Streaming removed the 10 KB per commit of object data, not the id sets (a competing system stays under 15 GiB here; bounding the
id sets would need a disk-backed set).

Reading: no slowdown at 1M commits (42.9 s against 44.8 s; the re-read of what is past the bound is hidden by building the
packs on several threads and by not allocating 10 GB). The remaining O(n) memory is the walk's own `known`/`done` id sets and the
64-byte plan entries, not data. #574: every `len as u32` in the pack writers (`build_pack*`, `write_pack_streaming`) is now a checked
conversion; an object of 4 GiB or more, or more than 2^32 entries, returns `StoreError::TooLarge` and writes nothing
(`try_build_pack*` return it; the old names panic with that message and are kept for tests). Single run each; the mid-size
bound runs were not repeated.

## Designs and open work

1. **Manifest write fan-in (findings 6, 12). Server side DONE (#452).** The paragraph below is the original analysis; what is built is the reconciliation of stale writes inside `vcs-server` (`groupcommit.rs`), which validates and stores the merged manifest by CAS on the current tag; the per-zone sharded protocol remains the answer for publishers that write the store directly. One key admits one winner per round trip, so throughput cannot grow with
   writers and p95 grows with queue length (25 s at 512). Cheapest next step, not built: **group commit in
   `vcs-server`**. The server already validates every manifest PUT; it can queue disjoint transactions that arrive within
   a few milliseconds, merge them into one manifest write and answer each client (clients keep the txid/applied
   protocol, so a lost ack stays safe). That turns N contending CAS rounds into one. The per-zone sharded protocol
   removes the key limit (built, opt-in, see the update below). Until either lands: keep fewer than 8 concurrent direct (non-server)
   publishers per repository. **Update (#313):** the sharded protocol is now integrated as an opt-in (`vcs-remote/src/sharded.rs`, docs/manifest-scaling.md last chapter); decision-record GC, shard-aware GC roots, cross-zone locks, per-shard applied ids and migration were model-checked first (`model_sharded*.rs`).
2. **Refs (finding 8). Built (#527), and why it took so long.** `Repo::refs()` decodes the whole file into pub `BTreeMap`s used in about 25 places, and every operation read the tag set into its view (finding 14, now cheap on disk but still parsed). A delta log needs a save API that knows the previous state (17 write sites call `write_atomic(refs, &refs.encode())`), and a lazy tag map needs the operation view to compare a digest of the raw tag bytes instead of parsing. Plan: (a) `Refs.tags` becomes a lazily parsed wrapper that derefs to the map and re-emits its raw bytes untouched; (b) `record_op` hashes the raw bytes; (c) then the delta log. (a)+(b) need no format change and would remove the 1.1 s load for commands that never look at tags; (c) needs a feature gate. Original sketch: Replace the single framed file by a base file plus an append-only delta log (compacted when the
   log exceeds a fraction of the base), loaded lazily by zone so that `log -n 1` reads only the branches it needs.
   Needs a format version and a crash story (the log is the only newly written file per update, so a torn tail is
   dropped by checksum). Done (#527): see "Refs delta log".
3. **Thread per connection (finding 3).** Memory per connection is a thread stack plus buffers; 256 default cap is the
   hard ceiling, and 503 + Retry-After is returned past it. A readiness-based front (accept and header parse on a small
   pool, handoff for bodies) would lift it; not needed below a few hundred concurrent clients.
4. **Commit-graph on import and gc (finding 9).** Done (#449).
5. **First `status` after import (finding 10).** Profile is flat (syscalls + hashing); the parallel first scan is built (see finding 10); the remaining lever is the watcher (`vcs watch`, #293).

## Not run

R2/GCS, S3 SlowDown counts under load on a real store (the shared throttle is only tested against the in-process injector), several server replicas for writes, thousands of idle sockets, the first `status` and later
commands at 10M files (see above, killed after 30 min), `gc` and `fsck` memory bounds at 1M commits (5.8 GB and 16 GB measured; fsck batched, not re-measured; gc not fixed), lock service or remote GC at scale on real S3 (in-memory backend only), sharded manifest on a real object store, 100k-path commit through `vcs add`/`commit`,
slowloris with thousands of sockets, per-client fair queuing, long (2 h) soak with the delta layout, macOS 1M-commit run.
