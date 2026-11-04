# Real S3 and S3 Express One Zone: CI-style benchmark and remote optimizations (issues #187, #313)

Measured 2026-11-04. Raw data: `docs/research/data/real-s3/`. Driver: `scripts/s3_real_bench.py` (fixtures and cells of `scripts/ci_bench.py`),
tables: `scripts/s3_real_report.py`, micro-benchmarks: `s3e2e` (`--throughput`, `--storm`), per-request trace: `VCS_S3_TRACE=1`.

## Read this first

- **Machine**: AWS c6i.4xlarge (box e, 16 vCPU, us-east-1a = `use1-az1`), git 2.43.0, vcs built from origin/main plus this PR. The box is shared with other
  agents' benchmarks; every run below was started under `flock ~/bench.lock`, but other agents' non-locked work ran at times (load1 up to 600 was seen on the
  first attempt, those numbers were discarded; the runs kept started with load1 < 2). Min-max is shown next to every median; 3 rounds per cell.
- **Buckets**: a general-purpose bucket in us-east-1 (private, public access blocked, SSE-S3, 2-day lifecycle) and an S3 Express One Zone directory bucket in `use1-az4`
  (Express is not offered in `use1-az1`, so the box talks across AZs; the 4 to 5 ms GETs below include that). Both were emptied and deleted after the run.
  The credentials belong to a different account than the instance, so all S3 traffic goes over the public regional endpoint (no VPC gateway endpoint).
- **Baseline**: git has no native S3, so the baseline is **git over git:// served by `git daemon` on the same box** (partial clone `--filter=blob:none`, sparse checkout,
  depth 1, and full). That baseline has **no WAN in the path** (loopback, local EBS), which flatters git. **walgit was not run**: building the third-party repository on the
  box was refused by the sandbox, and the older loopback comparison stays in [walgit-benchmark.md](walgit-benchmark.md).
- **Data**: `datagen flat` 10,000 and 100,000 files (57 MiB), 50 directories, job slice `g07` (2 %). 100k: 2000 files. History: initial + 5 commits.
- **Cold** = `VCS_NO_CACHE=1` (no persistent client cache); **warm** = the persistent client cache populated by the first clone. The OS page cache was not dropped.
- "std" = general-purpose S3 bucket, "express" = directory bucket.

## What the trace showed (10k files, cold slice clone, before)

`docs/research/data/real-s3/trace-clone-before.txt`: of about 1.5 s, the five-request provider probe at `connect` took about 120 ms (every command), the metadata read
made 3 sequential adjacent ranged GETs (5.5 KB, 283 KB, 175 KB), and the 32-byte pack trailer check was a separate round trip before the data read. One pooled GET of 1 KiB
costs 25 ms to std S3 (4.6 ms to Express); a fresh TLS connection adds 15 ms (37 % of a std GET, 69 % of an Express GET).

## Changes (all measured before/after below, tests green: `cargo test -p vcs-remote -p vcs-store-s3 -p vcs-cli`)

| change | why the numbers justify it | status |
|---|---|---|
| provider probe remembered 24 h per (https endpoint, region, bucket) in the cache dir; `VCS_S3_PROBE=always` restores it | 5 sequential requests, about 120 ms on every `fetch`/`clone`/`publish`; a property of the provider, not the repo | on |
| bounded metadata read asks for at least 256 KiB (or 1/8 of what is left of a small pack); `VCS_REMOTE_MIN_READ` | a round trip is 25 ms, 256 KiB is 2 ms at 100 MB/s; 16 to 14 requests per slice clone | on |
| pack trailer check runs in the same wave as the first ranged read of the pack | one round trip less on fetch/checkout; not A/B measured on its own | on |
| **S3 Express One Zone** (`s3://NAME--azid--x-s3?endpoint=https://s3express-azid.us-east-1.amazonaws.com`): CreateSession, `s3express` SigV4, `x-amz-s3session-token`, virtual-hosted addressing | GET 25 to 4.6 ms, PUT 29 to 7 ms; see below | on, auto-detected from the `--x-s3` suffix |
| multipart upload for create-only PUTs of at least `VCS_S3_MULTIPART_MB` MiB, 8 MiB parts on up to 16 connections | one stream moves 97 MB/s, 16 parallel PUTs 523 MB/s | **opt-in** (64 MiB: 690 to 440 ms) |
| `VCS_S3_TRACE=1` per-request trace | how the table above was found | on (off by default) |

Not changed, because the numbers do not justify it: **parallel ranged GETs** (already 8 threads; 1 thread 97 MB/s, 4 threads 362, 8 threads 738, 16 x 4 MiB 1026 MB/s,
`bulk-nomultipart.txt`: the existing default is on the plateau), **HTTP/2** (S3 speaks HTTP/1.1 only; connection reuse is already on: pool of 16),
**request coalescing** (already merges ranges within a gap; see the min-read row), **speculative prefetch from the zone profile** (not implemented, no measurement
points at it: the remaining cost of a slice clone is 14 requests in 4 dependent rounds).

## Results: general-purpose S3, before vs after (10k files, medians, ms; `data/real-s3/b10k.json` vs `a10k.json`, same box, back to back)

| cell | before | after probe cache + min read | git daemon, same box |
|---|---|---|---|
| warm runner: fetch new commit + checkout | 572 [536-617] | 290 [282-318] | 103 to 166 |
| push of a 3-file commit, p50 of 30 | 322 | 211 | 54 |
| cold slice clone, base history | 451 [415-493] | not rerun | 147 to 219 |
| cold slice clone after 30 pushes (58 to 49 requests) | 894 | 571 | 165 to 232 |

100k files (`b100k.json` vs `a100k.json`; 1000 sequential pushes each):

| cell | before | after | git daemon, same box |
|---|---|---|---|
| cold slice clone (2000 files, 21 MB, 16 to 14 requests) | 566 [536-1179] | 510 [380-558] | blobless 426 to 480, depth-1 sparse 1601 |
| warm runner: fetch + checkout | 603 [593-607] | 312 [306-368] | 221 to 233 |
| push p50 / p95 / p99 of 1000 | 328 / 537 / 3327 | 219 / 643 / 3392 | 79 / 80 / 81 |
| `status` with 20 modified files | 12 | 12 | 57 to 58 |

The p99 of 3.3 s is the automatic compaction (every 8th generation); it is in both runs.

## Results: general-purpose S3 vs S3 Express (final build; `std10k/exp10k/std100k/exp100k.json`)

| cell (ms) | std 10k | express 10k | std 100k | express 100k | git daemon 10k / 100k |
|---|---|---|---|---|---|
| cold slice clone, base history | 378 | **180** | 341 | **245** | 144 to 221 / 450 to 1667 |
| warm fetch + checkout | 251 | **109** | 264 | **149** | 107 to 168 / 231 to 245 |
| push p50 (100 / 300 pushes) | 217 | **57** | 232 | **60** | 56 / 83 |
| push p95 | 345 | 87 | 397 | 136 | 57 / 84 |
| initial publish of the whole repo | 337 | 141 | 1208 | 937 | 738 / 9981 |
| `s3e2e` micro: 1 KiB pooled GET / PUT | 24.6 / 29.1 | 4.6 / 7.2 | | | |
| `s3e2e` micro: fetch of one commit, 300 files | 127 | 33 | | | |

**Express limit found: no remote GC and no automatic compaction.** Directory buckets reject the in-place CopyObject that `touch` and the server clock use
("Request must be signed with AWS Credentials"), so `supports_gc()` is false there and autocompact is skipped. Every push stays one pack, so a clone after
300 pushes (100k) needs 1850 requests and 3.6 s (std, compacted: 113 requests, 1.5 s; git 0.56 s), and 100 pushes at 10k need 650 requests and 1.4 s. Express is therefore
good for the push and fetch hot path now, but a repository on it needs compaction run elsewhere before this is a default. The design the numbers point to is the one asked
for in #187: manifest, refs and tx records on Express, packs on the standard bucket (not implemented here).

## Other measurements

- **Bulk throughput** (64 MiB object, `bulk-nomultipart.txt`): single PUT 97 MB/s, single GET 100 MB/s; 16 ranged GETs of 4 MiB on 16 threads 1026 MB/s; 16 x 8 MiB PUTs on 16 threads 523 MB/s.
  With `VCS_S3_MULTIPART_MB=16` a 64 MiB create-only PUT takes 440 ms instead of 690 ms (153 vs 97 MB/s; create-only refusal of a second PUT and a byte-for-byte readback were checked
  against real S3, `bulk-multipart.txt`).
- **SlowDown**: 128 threads of 256-byte PUTs on one prefix for 15 s: 59,025 requests, 3,902 req/s, **zero 503s** (`storm.txt`). The single box cannot reach the 3,500 PUT/s per-prefix
  ceiling reliably, so the backoff path (`RetryBackend`: 3 retries, 100 ms base, 2 s cap, jitter) was **not exercised on real S3**; it stays validated only by the in-process simulation.
- **Connection setup**: 15 to 40 ms per new TLS connection; the pool of 16 keeps it off the hot path.

## Not run / caveats

walgit; git over HTTPS or SSH (git:// only); cold OS caches; 1000 pushes on Express and std at 100k in the final build (300 pushes there; the 1000-push run is the older build);
Express in the box's own AZ; a gateway VPC endpoint; credentials in another account than the box. The "after" 10k and 100k general-purpose columns in the first two tables
are the build with the probe cache and min read only (before the trailer overlap and Express); the std columns of the third table are the final build.
Cost: about 1.5 h of c6i.4xlarge (about US$1), S3 requests and transfer well under US$1.
