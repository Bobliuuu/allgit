# Many writers on real S3: where the one manifest saturates, and the batch commit that moves it

Measured 2026-11-04 on AWS box f (c6i.4xlarge, 16 vCPU, Ubuntu 24.04) in us-east-1 against a private, public-access-blocked
S3 bucket in the same region (**real S3, not RustFS**). The box was shared with another benchmark agent: all timed runs took
`flock ~/bench.lock`, so they did not overlap, but the box was not otherwise idle between runs. One run per cell, 2 to 3
pushes per writer; treat differences under about 30 percent as noise. Raw rows: `docs/research/many-writers-data/`.
Charts are rendered by `scripts/render_many_writers_charts.py`.

## Method

* Driver: `crates/vcs-bench/src/bin/writerbench.rs`. N simulated clients are threads, each with its own HTTP connection
  pool, its own in-memory object store and the real `Publisher` protocol over `HttpBackend` (no CLI process per push, so
  1024 writers fit in 4 processes). Each push uploads one pack and one manifest transaction.
* Server: `vcs-server --backend s3://BUCKET/PREFIX?endpoint=https://s3.us-east-1.amazonaws.com` on loopback, 4 driver
  processes on the same box, so client CPU competes with the server (server CPU stayed under 10 s per run, see the table).
* Workloads: `zones` (writer i pushes zone `z<i>`), `branches` (one zone, branch `b<i>`), `same` (all writers on one branch,
  recover-and-retry on `Diverged`).
* Server RSS is VmHWM, fds are the peak sampled every 0.5 s, manifest size is the bytes of `GET /v1/o/manifest` after the run.
* Not captured: the server's own S3 `SlowDown`/503 counters (my metrics query lacked the admin token and returned nothing), so
  **no SlowDown count is reported**; no push failed with a backend error in any run below.

## Finding 1: with the old server, one manifest PUT per push caps real S3 at about 20 pushes/s, at any writer count

Disjoint writers never conflict (0 conflicts in every cell), yet throughput does not grow with writers. Server-side group
commit reconciled stale writes, but each reconciled write still needed its own conditional PUT of the manifest, serialized
under one lock, and a conditional PUT on S3 costs about 45 to 50 ms from this box.

| writers | mode | pushes/s | p50 s | p95 s | p99 s | server CPU s | RSS MiB | peak fds | manifest bytes |
|---|---|---|---|---|---|---|---|---|---|
| 64 | zones | 20.8 | 3.0 | 3.2 | 3.2 | 0.2 | 26 | 151 | 6,722 |
| 256 | zones | 21.5 | 10.8 | 12.2 | 12.7 | 0.95 | 60 | 534 | 27,039 |
| 512 | zones | 19.4 | 20.0 | 30.0 | 33.0 | 2.2 | 102 | 1,046 | 54,175 |
| 1024 | zones | 19.2 | 47.7 | 52.2 | 54.2 | 6.0 | 226 | 2,071 | 108,563 |
| 1024 | branches | 19.1 | 46.0 | 53.8 | 57.4 | 6.2 | 224 | 2,071 | n/a |

Throughput is flat at 1 / PUT latency, latency is N / throughput (Little's law). The server is idle (6 CPU seconds for 2048
pushes): the bottleneck is a **serial chain of S3 round trips on one key**, not CPU, fds, memory or CAS retries (there were none).

## Fix: flat-combining batch commit (`manifest_combine` in `crates/vcs-server/src/server.rs`)

A manifest write now enqueues itself; whoever then takes the manifest lock becomes the leader, drains the queue, folds every
queued write into one manifest (each validated against the result of the ones before it; stale ones reconciled by the same
rules as before, `groupcommit.rs`), stores it with **one** conditional PUT and answers everybody. There is no timer window: the
batch is whatever queued during the previous PUT, so a lone writer sees batch size 1 and no added wait, and a slow store
automatically gets bigger batches. A write that cannot be folded in (same branch moved, invalid, unknown base) gets exactly the
answer it got before (412 or 4xx). If the single PUT fails every write in the batch fails with that error, and the clients'
txid protocol makes the retry safe. Disable with `Config::group_commit = false` as before.

Why not an adaptive window: the numbers show no wasted wait at low load to tune away (batch size 1 at low load has no wait)
and queueing at high load is exactly what the leader batch absorbs; a timer would only add latency.

| writers | mode | before pushes/s | after pushes/s | before p99 s | after p99 s |
|---|---|---|---|---|---|
| 64 | zones | 20.8 | 98.4 | 3.2 | 0.58 |
| 256 | zones | 21.5 | 214.3 | 12.7 | 1.4 |
| 512 | zones | 19.4 | 157.5 | 33.0 | 6.3 |
| 1024 | zones | 19.2 | 279.9 | 54.2 | 6.9 |
| 64 | branches | 22.6* | 47.4 | 2.9* | 2.5 |
| 256 | branches | 21.0 | 347.6 | 16.7 | 1.3 |
| 512 | branches | 21.0 | 157.6 | 29.9 | 6.2 |
| 1024 | branches | 19.1 | 266.3 | 57.4 | 7.4 |

All cells are single runs. That is 10 to 17 times more pushes per second and 5 to 8 times lower p99 on real S3 at 256 to 1024 writers. The
64-writer rows are short (2 pushes each, mostly connection setup and ramp), so the after-numbers there are noisy; the 512 rows
look low because the 4 driver processes and the server share 16 cores (a tail of slow writers sets the wall time). The local-disk
control (fsync on every manifest write, so batching saves fsyncs) moved the same way:

| writers | zones before pushes/s | after | before p99 s | after p99 s |
|---|---|---|---|---|
| 64 | 288 | 1,794 | 0.31 | 0.05 |
| 256 | 182 | 654 | 2.6 | 1.1 |
| 1024 | 138 | 930 | 13.2 | 2.6 |

The disk "before" rows ran on a different c6i.4xlarge with the same harness, bucket-less, earlier the same day (the benchmark
box was swapped under me mid-run), so they are an indication, not a paired comparison.

Where it saturates now: not measured to a clean knee. Rows above show 215 to 350 pushes/s at 256 to 1024 writers on real S3
with the driver sharing the box; the single manifest CAS is no longer the visible limit there (server 7 CPU seconds,
132 to 149 MiB RSS). The remaining serial costs are one S3 PUT per batch (about 50 ms: bound 20 batches/s) times the batch size
and the manifest size (108 KB at 1024 zones, about 106 bytes per zone-branch, re-encoded, re-validated and PUT per batch).
Because manifest bytes grow linearly with zones, the batch PUT cost grows too; the per-zone sharded manifest
(`docs/manifest-scaling.md`, feature `sharded`, still off) becomes interesting when a single repository passes roughly
10^5 branches (about 10 MB manifest), not before, and that point was not run.

## Finding 2: one hot branch is a retry storm, not CAS fan-in

All writers on one branch can land only one push per round trip by construction; the cost is that each loser refetches,
rebuilds and retries, so requests grow with N squared and the server burns CPU (66 s of server CPU for 675 pushes at 256 writers).

| backend | writers | client retry | acked pushes/s | p50 s | p99 s | conflicts | note |
|---|---|---|---|---|---|---|---|
| disk | 64 | old | 23.4 | 0.02 | 8.0 | 2,750 | |
| disk | 256 | old | 6.3 | 0.2 | 88 | 77,639 | 2 writers gave up after 400 tries |
| disk | 512, 1024 | old | 0 in 240 s | | | | server burned 205 to 214 CPU s |
| S3 | 64 | old | 1.4 | 1.1 | 133 | 6,026 | |
| S3 | 256 | old | 0 in 240 s | | | | |
| S3 | 64 | batch + decorrelated jitter, cap 400 ms | 6.4 | 0.15 | 29 | 4,124 | 4.6x |
| S3 | 256 | same | 6.7 | 0.15 | 110 | 64,266 | stays alive; old client got 0 |
| S3 | 512 | same | 1.3 (205 of 1,536 in 240 s) | | 110 | 51,020 | |
| disk | 256 | same | 9.0 | 0.25 | 83 | 61,873 | |

The median push is fast (about 150 ms) while the tail is 30 to 110 s: starvation, not slowness. Decorrelated jitter on the
client retry and the batch commit keep 64 to 256 writers alive on one branch where the old client collapsed on S3 and
at 512 on disk, but **one hot branch above about 64 writers is not a supported workload**: use per-writer branches or zones
and integrate with a merge train. A server-side per-branch ticket queue (so losers wait instead of polling) is the next
step and is not built; it is not sharding and would not change the manifest.

## Retry jitter in the publish path (`crates/vcs-remote/src/protocol.rs`)

The CAS-loss backoff changed from full-jitter exponential (`uniform[0, 4 ms << losses)`, which draws near-zero waits and re-collides
with the herd) to decorrelated jitter, `uniform[4 ms, min(250 ms, 3 x previous wait)]`, still bounded and derived from the txid and
clock. Effect on the measured paths: none visible, because disjoint writers saw 0 conflicts in every cell (the server absorbs the
stale writes); it matters for direct-to-store publishers (no server), which this run did not exercise. The same-branch
numbers above come from `writerbench --decorrelated`, which applies the same rule to the `Diverged` loop in the driver.

## Correctness

* `cargo test -p vcs-server -p vcs-remote`: all pass (including `scale_groupcommit`, the lock scenarios, GC and peer tests).
* Soak (`scripts/soak/soak.py run --kind vcs --backend local --n 60 --minutes 6 --chaos kill9 --clientkill 0.02`, remote gc every
  20 s, new server binary): 177 `kill -9` of the server, 2,254 acknowledged files, **0 missing, 0 wrong content**, fresh-clone
  `fsck` ok, copy-of-store clone `fsck` ok, remote gc dry run clean, server alive at the end, 0 bad audits. Local disk backend,
  not S3 (the soak harness's S3 mode targets a local RustFS).
* Acked pushes are never lost: a write is acknowledged only after the single conditional PUT that contains it succeeded; every
  write in a failed PUT is answered with the error, never 200.

## Not run (honest list)

* Several `vcs-server` replicas behind `--upstream` for writes: a peer forwards writes to the origin and only caches reads, so
  write throughput is the origin's; not measured.
* git (local bare repo over ssh) and walgit at these writer counts: the earlier 8/32/128 comparison is in
  `highload-matrix.md`; not repeated here.
* S3 SlowDown/503 counts (see Method), gc running under the 1024-writer load, writers beyond 1024, and more than one run per cell.
* Sharded manifests (not built, by decision).
