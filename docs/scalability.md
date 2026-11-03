# Scalability: what breaks first, and what was done

Audit of what degrades as repositories, refs, writers and connections grow. Every number below was measured by the
author of this chapter unless it says "not run". Tests: `crates/vcs-server/tests/scale_inflight.rs`, import unit test in
`crates/vcs-repo/src/import.rs`. Probe scripts are in the PR description of the change that added this chapter (stream
generator for `git fast-import`-style streams: `deep`, `bigdir`, `wide`, `tags`).

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
| 6 | Manifest CAS under 512 writers | Mem backend, 1000 zones x 5 branches (249 KB), 3 publishes per writer, one run: 1 writer 133/s; 8: 62/s; 64: 106/s, p95 1.4 s; 256: 86/s, p95 6.4 s, 91.7 percent CAS lost, 11 retries per publish; **512: 41/s, p50 5.9 s, p95 25.5 s, max 37 s, 92.8 percent lost, 12.9 retries per publish, 31 requests per publish**. All 1536 publishes completed (no starvation failure in this run) | **designed** (see Designs); sharded prototype from manifest-scaling.md stays off |
| 7 | Manifest size | See manifest-scaling.md: linear, about 40-63 bytes per branch, 1 MiB at 17-26k refs; 2.6 MB at 10k zones x 5 | **designed**, unchanged |
| 8 | Refs file is read and rewritten whole (`.vcs/refs`) | 10k tags + 1 branch: tag create 0.04 s. 100k tags (4.4 MB file): tag create 0.18 s, `log -n 1` 0.09 s, `tag` list 0.37 s, gc 0.69 s. **1M tags (44.9 MB): tag create 1.85 s / 522 MB RSS, branch create 1.42 s, `log --oneline -n 1` 1.11 s / 265 MB, `tag` list 3.91 s, gc 8.5 s / 541 MB**. Import of 1M tags 3.65 s | **open**: linear in refs per command; fine up to about 100k refs, design below |
| 9 | Deep history | 100,000 linear commits (435 MB .vcs): import 83.7 s; first `rev-list HEAD` **13.2 s** (builds the commit-graph cache), later `rev-list HEAD` 0.10 s, `log -n 20` 0.01 s, `merge-base` 0.02 s, `gc` 9.6 s / 91 MB. 10k commits: first rev-list 2.3 s, gc 0.8 s | **ok when warm**; cold graph build and gc are linear (about 0.1 ms per commit). 1M commits not run |
| 10 | Wide trees | 300,000 files in 300 dirs x 100 subdirs: import 29.0 s / 357 MB; **first `status` after import 22.2 s** (flat dir: 6.2 s); later `status` after one edit 0.26 s, commit 0.17-0.38 s, `ls-files` 0.05 s, `show --stat` 1.7 s, gc 2.3 s | **open**: first-status index build is slow on many directories (about 74 us per file); 10M-file trees not run |
| 11 | GC at scale | Above: 100k commits 9.6 s, 300k files 2-2.3 s, 1M refs 8.5 s (all local, unlocked, no concurrent push). Memory grows with objects (91 MB at 100k commits, 541 MB at 1M refs) | **measured locally only**; remote GC concurrent with pushes and lease renewal at scale **not run** (model tests `model_gc.rs`, `gc_real.rs` cover correctness, not scale) |
| 12 | Lock/lease service with thousands of holders | Locks live in the manifest, so each holder adds bytes to the one CAS object and each acquire/release is a manifest publish (finding 6 applies) | **not run**; designed: same fix as 6 |
| 13 | S3 request shaping, 503 SlowDown | Code audit: 5xx/408/429 map to `Unavailable`, `RetryBackend` retries with jitter (`crates/vcs-remote/src/retry.rs`); object keys fan out over 256 prefixes (`objects/<hh>/`); `manifest` is one key | **not tested** against a fault-injecting backend (none built); no 503 SlowDown test was run on RustFS/MinIO |

## Designs and open work

1. **Manifest write fan-in (findings 6, 12).** One key admits one winner per round trip, so throughput cannot grow with
   writers and p95 grows with queue length (25 s at 512). Cheapest next step, not built: **group commit in
   `vcs-server`**. The server already validates every manifest PUT; it can queue disjoint transactions that arrive within
   a few milliseconds, merge them into one manifest write and answer each client (clients keep the txid/applied
   protocol, so a lost ack stays safe). That turns N contending CAS rounds into one. The per-zone sharded protocol
   (`sharded` feature) removes the key limit but needs decision-record GC, shard-aware remote GC and cross-shard locks
   first (manifest-scaling.md, "The sharding gate"). Until either lands: keep fewer than 8 concurrent direct (non-server)
   publishers per repository.
2. **Refs (finding 8).** Replace the single framed file by a base file plus an append-only delta log (compacted when the
   log exceeds a fraction of the base), loaded lazily by zone so that `log -n 1` reads only the branches it needs.
   Needs a format version and a crash story (the log is the only newly written file per update, so a torn tail is
   dropped by checksum). Not started.
3. **Thread per connection (finding 3).** Memory per connection is a thread stack plus buffers; 256 default cap is the
   hard ceiling, and 503 + Retry-After is returned past it. A readiness-based front (accept and header parse on a small
   pool, handoff for bodies) would lift it; not needed below a few hundred concurrent clients.
4. **Commit-graph on import and gc (finding 9).** The first history walk after an import pays the 0.13 ms per commit
   graph build. Writing the graph at the end of `import-git` and `gc` would move that cost to a command that already is
   linear. Not done.
5. **First `status` after import (finding 10).** Not investigated beyond the measurement.

## Not run

Real S3/R2/GCS, a fault-injecting S3 backend for SlowDown, 1M-commit history, 10M-file trees, 100k-path commit through
`vcs add`/`commit` (only via import), lock service with thousands of holders, remote GC with concurrent pushes at scale,
slowloris with thousands of sockets (the existing test uses a handful), per-client fair queuing (only the per-principal
token bucket exists; no queueing).
