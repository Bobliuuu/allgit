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
| 8 | Refs file is read and rewritten whole (`.vcs/refs`) | 10k tags + 1 branch: tag create 0.04 s. 100k tags (4.4 MB file): tag create 0.18 s, `log -n 1` 0.09 s, `tag` list 0.37 s, gc 0.69 s. **1M tags (44.9 MB): tag create 1.85 s / 522 MB RSS, branch create 1.42 s, `log --oneline -n 1` 1.11 s / 265 MB, `tag` list 3.91 s, gc 8.5 s / 541 MB** (single run each). Import of 1M tags 3.65 s | **partly fixed** (this change, #436 item 2a): the tag table is validated without allocating and parsed only when a command looks at it; an untouched table is written back byte for byte; `tag` list peels each distinct target once. Commands that record an operation still parse (the op view stores the set; #454 will make that cheap). Numbers below the table. The delta log (2c) is still open |
| 9 | Deep history | 100,000 linear commits: import 74.4 s (3 runs, median); first `rev-list HEAD` after import **1.78 s** (3 runs: 2.15, 1.68, 1.78; it builds the commit-graph cache), later `rev-list HEAD` 0.10 s, `log -n 20` 0.01 s, `merge-base` 0.02 s, `gc` 9.6 s / 91 MB (single run). An earlier single run here measured 13 s for the first rev-list under load 13; the medians supersede it | **fixed** (#449): graph built at the end of import and gc, first rev-list 0.103 s (3 runs). gc stays linear (about 0.1 ms per commit). 1M commits not run |
| 10 | Wide trees | 300,000 files in 300 dirs x 100 subdirs: import 29.0 s / 357 MB; **first `status` after import 22.2 s** (flat dir: 6.2 s); later `status` after one edit 0.26 s, commit 0.17-0.38 s, `ls-files` 0.05 s, `show --stat` 1.7 s, gc 2.3 s (single runs) | **investigated, no easy win**: `perf` of the first status (the index has no stat data after import) is flat, dominated by lstat/open/read syscalls and blake3 of every file (top symbol 5 percent). The tracked scan is parallel now (f1d87bb, `VCS_STATUS_THREADS`). Re-run on a macOS laptop (APFS, same 300,000 files in 3,000 dirs, busy machine, single runs): `import-git` then **first `status` 15.2 s**, next three clean `status` 7.8, 4.3, 4.2, 3.6 s; `git status` on the same tree 5.8, 13.7, 8.4 s (git checkout of the same stream, no fsmonitor). No win or loss claimed: the box was noisy, and both are bound by lstat of 300k files. 10M-file trees not run. With the watcher (#293) the same clean `status` costs 7 ms CPU on a 100k-file tree in 2,100 directories (git 720 ms CPU, see benchmarks.md); on macOS the watcher uses one recursive FSEvents stream, because one stream per directory had not started after 3 minutes |
| 11 | GC at scale | Above: 100k commits 9.6 s, 300k files 2-2.3 s, 1M refs 8.5 s (all local, unlocked, no concurrent push). Memory grows with objects (91 MB at 100k commits, 541 MB at 1M refs) | **measured locally only**; remote GC concurrent with pushes and lease renewal at scale **not run** (model tests `model_gc.rs`, `gc_real.rs` cover correctness, not scale) |
| 12 | Lock/lease service with thousands of holders | Locks live in the manifest, so each holder adds bytes to the one CAS object and each acquire/release is a manifest publish (finding 6 applies) | **model-checked, not run at thousands**: locks spanning zones are plain transactions with the lock map of every touched zone as a precondition (`model_sharded.rs`: a guarded push cannot interleave with a steal; mutation test removes the rule and fails); the real publisher+lock path is unchanged on top of `ShardedBackend`. A lock operation also touches the root participant (token counter), so lock-heavy repositories still serialise those operations on one small record |
| 13 | S3 request shaping, 503 SlowDown | Fault-injecting backend (`crates/vcs-remote/tests/slowdown.rs`, #447): per-prefix budget per 100 ms, 24 publishers x 4 publishes through `RetryBackend` defaults, one run each: 17, 37, 55 percent of requests shed: all 96 publishes complete; 74 percent shed: 20 of 96 give up with `Unknown` (retries exhausted, state unknown, safe to rerun by txid). Object keys fan out over 256 prefixes; `manifest` is one key | **tested in-process** (not RustFS/MinIO/AWS); adaptive shared back-off not built |
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

## Designs and open work

1. **Manifest write fan-in (findings 6, 12). Server side DONE (#452).** The paragraph below is the original analysis; what is built is the reconciliation of stale writes inside `vcs-server` (`groupcommit.rs`), which validates and stores the merged manifest by CAS on the current tag; the per-zone sharded protocol remains the answer for publishers that write the store directly. One key admits one winner per round trip, so throughput cannot grow with
   writers and p95 grows with queue length (25 s at 512). Cheapest next step, not built: **group commit in
   `vcs-server`**. The server already validates every manifest PUT; it can queue disjoint transactions that arrive within
   a few milliseconds, merge them into one manifest write and answer each client (clients keep the txid/applied
   protocol, so a lost ack stays safe). That turns N contending CAS rounds into one. The per-zone sharded protocol
   removes the key limit (built, opt-in, see the update below). Until either lands: keep fewer than 8 concurrent direct (non-server)
   publishers per repository. **Update (#313):** the sharded protocol is now integrated as an opt-in (`vcs-remote/src/sharded.rs`, docs/manifest-scaling.md last chapter); decision-record GC, shard-aware GC roots, cross-zone locks, per-shard applied ids and migration were model-checked first (`model_sharded*.rs`).
2. **Refs (finding 8). Not built, and why.** `Repo::refs()` decodes the whole file into pub `BTreeMap`s used in about 25 places, and every operation read the tag set into its view (finding 14, now cheap on disk but still parsed). A delta log needs a save API that knows the previous state (17 write sites call `write_atomic(refs, &refs.encode())`), and a lazy tag map needs the operation view to compare a digest of the raw tag bytes instead of parsing. Plan: (a) `Refs.tags` becomes a lazily parsed wrapper that derefs to the map and re-emits its raw bytes untouched; (b) `record_op` hashes the raw bytes; (c) then the delta log. (a)+(b) need no format change and would remove the 1.1 s load for commands that never look at tags; (c) needs a feature gate. Original sketch: Replace the single framed file by a base file plus an append-only delta log (compacted when the
   log exceeds a fraction of the base), loaded lazily by zone so that `log -n 1` reads only the branches it needs.
   Needs a format version and a crash story (the log is the only newly written file per update, so a torn tail is
   dropped by checksum). Not started.
3. **Thread per connection (finding 3).** Memory per connection is a thread stack plus buffers; 256 default cap is the
   hard ceiling, and 503 + Retry-After is returned past it. A readiness-based front (accept and header parse on a small
   pool, handoff for bodies) would lift it; not needed below a few hundred concurrent clients.
4. **Commit-graph on import and gc (finding 9).** Done (#449).
5. **First `status` after import (finding 10).** Profile is flat (syscalls + hashing); the parallel first scan is built (see finding 10); the remaining lever is the watcher (`vcs watch`, #293).

## Not run

R2/GCS, S3 SlowDown counts under load (real S3 throughput measured, SlowDown counters not captured), several server replicas for writes, sharded manifest on a real object store (only an in-memory backend with injected latency and a local disk server were run), thousands of idle sockets (the 512-writer runs needed `ulimit -n` above the box default of 1024), 1M-commit history, 10M-file trees, 100k-path commit through
`vcs add`/`commit` (only via import), lock service with thousands of holders, remote GC with concurrent pushes at scale,
slowloris with thousands of sockets (the existing test uses a handful), per-client fair queuing (only the per-principal
token bucket exists; no queueing).
