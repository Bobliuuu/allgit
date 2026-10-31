# Benchmark methodology: metrics, measurement protocol, scoring

Issue #181, epic #190. Status: v1 (2026-10-31). Nothing in this document is a measurement; it defines what the later
benchmark issues (#182 to #187) must measure and how results are compared. Companion files:
[cost-model.md](cost-model.md), [provider-comparison.md](provider-comparison.md),
[conformance-test-plan.md](conformance-test-plan.md), scripts `scripts/storage_score.py` and
`scripts/storage_cost_model.py`.

## 1. Principles

1. No single "score". A design that minimises bytes can be slower; one that minimises GETs can transfer more.
   Results are reported as Pareto frontiers per (workload, cache state). The scoring script never weights objectives.
2. Correctness is a gate, not an objective. A design that fails any correctness check is listed as DISQUALIFIED and is
   excluded from frontier computation.
3. Every number carries: command, dataset seed, machine, tool versions, cache state, number of trials, spread.
4. Negative results are recorded in the same JSON/CSV as positive ones (a design that loses is still a row).
5. CONFIRMED means measured by a committed, re-runnable command. Anything else is HYPOTHESIS.

## 2. Metric definitions

Notation: `L` = logical bytes (sum of uncompressed, un-chunked blob/tree/commit sizes for the dataset's reachable
objects). `P` = physical bytes at rest in the backend (sum of object sizes as the backend bills them, before provider
replication or erasure-coding overhead, including indexes, manifests, headers and garbage not yet collected).

### 2.1 Storage efficiency

| Metric | Definition | Unit |
|---|---|---|
| `stored_bytes` (P) | Total bytes at rest after the workload, measured by listing the backend (`list` + sizes) or `du` for the local store. Includes unreachable garbage. | bytes |
| `space_amp` | `P / L`. Below 1 means savings. | ratio |
| `dedup_ratio` | `L_unique_chunks / L_total_chunks` where chunk identity is the content id; reported only for chunked designs. | ratio |
| `compression_ratio` | `bytes_after_codec / bytes_before_codec` over the same payload; codec, level and block size reported with it. | ratio |
| `meta_overhead` | `(P - payload_bytes) / P` where payload is compressed chunk/blob data; covers indexes, pack headers, manifests, tree objects. | ratio |
| `object_count` | Number of backend objects. Drives request cost for full scans and per-object billing minima. | count |
| `small_object_fraction` | Fraction of objects under the provider's minimum billable size (128 KiB for S3 IA, see cost-model.md). | ratio |
| `write_amp` | Total bytes PUT to the backend over the whole run (ingest + compaction + GC rewrite) divided by `L_ingested`. | ratio |
| `garbage_bytes` | Bytes at rest that are unreachable from any ref after the run, before GC. | bytes |

### 2.2 Performance

Latency is measured per operation class and reported as p50, p95, p99, max over all operations of all trials, and
also as the median across trials of each trial's p95 (to expose between-trial spread).

| Metric | Definition |
|---|---|
| `lat_<op>_p50/p95/p99/max_ms` | Wall time of one logical operation. Ops: `commit_small` (1 to 10 files changed), `commit_large`, `clone_full`, `clone_sparse(zone set)`, `checkout`, `fetch_incremental`, `log_path` (historical lookup of one path), `blame_like`, `gc_dry_run`. |
| `ttfb_first_file_ms` | Time from command start to first requested file readable (sparse workspaces). |
| `throughput_mib_s` | Logical bytes moved per second for bulk ops (`clone_full`, `push_full`). |
| `cpu_user_s`, `cpu_sys_s` | `getrusage(RUSAGE_CHILDREN)` of the benchmark process tree, per op. Compression/chunking CPU is reported separately when the prototype exposes it. |
| `peak_rss_mib` | Peak resident set (`/usr/bin/time -v` "Maximum resident set size", or `VmHWM`). |
| `disk_read_bytes`, `disk_write_bytes`, `disk_ops` | From `/proc/<pid>/io` (`read_bytes`, `write_bytes`, `syscr`/`syscw`) on Linux; cache-state aware (see 3.2). |
| `net_rx_bytes`, `net_tx_bytes` | Bytes on the wire to the backend. For local MinIO use the counting backend (below); for filesystem backends 0. |

### 2.3 Cloud efficiency

Request counters come from a counting wrapper around `Backend` (the existing `S3Backend::requests()` and
`bytes_read()` give totals; a per-method counter is a follow-up for #182).

| Metric | Definition |
|---|---|
| `gets`, `range_gets`, `puts`, `lists`, `heads`, `deletes` | Requests by type. A ranged GET counts as one GET for billing (every provider in cost-model.md bills per request). |
| `put_if_absent_conflicts`, `cas_retries` | Number of 412/409 responses on conditional writes; indicates commit contention. |
| `bytes_downloaded`, `bytes_uploaded` | Payload bytes (headers excluded). |
| `bytes_overfetched` | `bytes_downloaded - bytes_needed`, where needed is the sum of bytes the logical op actually consumed. Measures range-read/pack granularity waste. |
| `critical_path_rtts` | Number of sequential (dependent) round trips for the op. A design with fewer, larger requests can still lose if they are sequential. |
| `egress_bytes` | Bytes leaving the provider network (clients outside it). |
| `monthly_cost_usd` | From `scripts/storage_cost_model.py` for a stated workload (stored bytes, requests per month, egress). Always reported with provider/class; never summed across providers. |
| `cost_per_op_usd` | Request and egress cost of one op (clone, commit) under a stated provider/class. |

### 2.4 Correctness (gates)

| Check | Pass condition |
|---|---|
| `roundtrip` | Every object read back from a fresh process hashes to its id (BLAKE3) and tree walk reproduces the source tree byte for byte. |
| `fsck` | `vcs fsck`-equivalent finds no missing or corrupt reachable objects after the run, after compaction, and after GC. |
| `crash_atomicity` | Under crash injection at every I/O step the repo is either at the old or new commit, never mixed (existing harness; `cd0fd09` "crash injection"). |
| `multizone_atomic` | The model checker (about 1.7M schedules at `cd0fd09`) still passes, and fails when a safety check is removed (mutation check), for any protocol change. |
| `cas_linearizable` | N concurrent `put_if_match` / `put_if_absent` writers on one key: exactly one winner per round, no lost update (see conformance-test-plan.md). |
| `gc_safety` | No reachable object is ever deleted while concurrent readers/writers run (GC experiments, #186). |

A design failing a gate is recorded with `"correct": false` in the benchmark JSON.

## 3. Measurement protocol

### 3.1 Environment record (required for every result file)

`machine`: hostname alias, CPU model (`lscpu`), core count, RAM, kernel (`uname -r`), filesystem and mount options of
the data dir, storage device (`lsblk -d -o NAME,ROTA,MODEL`), governor, other load (`uptime`, `top -bn1` head), Rust
version (`rustc -V`), git revision of the repo and the prototype, compiler flags/profile, tool versions (MinIO, zstd, etc),
dataset id and generator seed (`datagen`). Reject runs where 1-minute load average exceeds 10% of cores at start.
The Mac is not a measurement machine (noisy, small disk); results come from the 30-core Linux box (`lambda-final`).

### 3.2 Cache states (definitions, all three reported)

* cold: fresh process; client object cache, directory cache and commit-graph removed; OS page cache dropped
  (`sync; echo 3 > /proc/sys/vm/drop_caches`, needs root; if unavailable use `posix_fadvise(DONTNEED)` per file or
  a fresh tmpfs/loop mount and say so); backend server restarted for local MinIO (server-side cache cold).
* warm: the same op run again immediately after a cold run with caches left in place; OS page cache populated.
* partly warm: client cache pre-populated with a stated fraction (25/50/75%) of the objects the op needs, selected
  by a seeded RNG, OS cache dropped. Reported as a curve over the fractions, not one point.

### 3.3 Trials, statistics

* Minimum 10 trials per (design, workload, cache) cell; 5 for operations over 10 minutes, flagged as such.
* One discarded warm-up run for warm cells only. Cold cells discard nothing.
* Report median, min, max and the relative spread `(max-min)/median`. Where 10 or more trials exist, also report a
  95% bootstrap confidence interval of the median (10,000 resamples, fixed seed). `storage_score.py` currently
  reports median and relative spread only; the bootstrap CI is computed by the harness (#182) and stored as extra
  metric fields. [Not yet implemented here.]
* Two designs are "different" on a metric only when their trial ranges do not overlap; the frontier table prints
  the spread next to each median so ties inside noise are visible. The scoring script itself compares medians.
* Trial order is randomised (interleaved across designs) to avoid drift bias.
* Deterministic metrics (bytes at rest, request counts) are reported from a single run but verified identical on a
  repeat; if not identical, they are treated as noisy metrics.
* Never drop outliers. A max that is 10x the median is a finding (tail latency), not noise.

### 3.4 Datasets and workloads

Datasets and workload definitions are owned by #182 (`workloads.md`, `datagen`). The methodology requires at least:
source-code-like trees (many small text files), mixed binaries, large ML-checkpoint-like blobs, and a history with
at least 1000 commits. Each result names dataset id + seed + size. Workload scenarios per cache state: full clone,
sparse clone of 1 / 5 / 20% of zones, incremental fetch of 1 / 100 / 1000 commits, small commit, historical path
lookup at depth 10 / 100 / 1000, and concurrent committers (2, 8, 32) on the same zone set.

### 3.5 Provider-backed runs

Local filesystem and local MinIO are free and run by default. Real cloud runs need explicit spend approval (#187);
the cost model gives the pre-run estimate that goes in the approval request, and the actual provider bill is
recorded next to it.

## 4. Benchmark result schema (`allgit-bench/v1`)

```json
{
  "schema": "allgit-bench/v1",
  "machine": { "alias": "lambda-final", "cpu": "...", "kernel": "...", "rustc": "...", "load_at_start": 0.4 },
  "command": "full command line that regenerates this file",
  "dataset": { "id": "src-10k", "seed": 42 },
  "results": [
    { "design": "baseline-loose",
      "workload": "clone_sparse_5pct",
      "cache": "cold",
      "correct": true,
      "trials": 10,
      "metrics": { "stored_bytes": 123456789,
                   "gets": 412,
                   "lat_clone_p95_ms": [812, 790, 835, "...one value per trial..."] } }
  ]
}
```

A metric is either one number or an array of per-trial values (median taken by the scoring script). Extra keys are
allowed. Small files (under 1 MiB) go under `docs/research/storage/data/`; large raw output stays on the box and is
described in the result's `raw_location` field. A synthetic file for testing the scripts lives at
`data/synthetic-example-bench.json` and is marked `"synthetic": true`; it contains no measurements.

## 5. Scoring: Pareto frontier tables

```
python3 scripts/storage_score.py data/<bench>.json --objectives stored_bytes,gets,lat_clone_p95_ms
python3 scripts/storage_score.py a.json b.json --objectives stored_bytes,monthly_cost_usd --price-class s3_standard
python3 scripts/test_storage_score.py        # unit tests (hand-made example)
```

* Groups: by default one table per (workload, cache). Designs inside a group are compared only with each other.
* Dominance: A dominates B iff A is no worse on every chosen objective and strictly better on at least one
  (`--maximize name,...` flips the direction for higher-is-better metrics). Equal points do not dominate each other.
* The tool prints, per design, "yes" (on the frontier) or "dominated by <designs>"; incorrect designs are
  DISQUALIFIED. Choose objectives deliberately: adding an objective can only add designs to the frontier, never
  remove them, so a frontier over many objectives is permissive. Publish at least the standard objective sets:
  1. cost frontier: `stored_bytes`, `gets`, `bytes_downloaded` (or `monthly_cost_usd` for a named provider class)
  2. latency frontier: `lat_<op>_p95_ms`, `cpu_user_s`, `peak_rss_mib`
  3. read-path frontier: `critical_path_rtts`, `bytes_overfetched`, `gets`
* The architecture decision (#188) must cite the table(s) and must state where the chosen design sits dominated or
  worse than baseline/Git/walgit on some objective.

## 6. What this methodology deliberately does not do

* No weighted composite score (any weights would be an unevidenced opinion).
* No extrapolation from 1k-10k file Mac runs to repository scale; scale claims need runs at the stated scale.
* No cross-provider latency comparison from single-region runs of one client location; provider latency depends on
  client location and is reported with it.

## Sources

Internal: `crates/vcs-remote/src/backend.rs` and `crates/vcs-store-s3/src/lib.rs` at revision `cd0fd09`
(read 2026-10-31 for the `Backend` trait and counters). External provider facts used here are cited in
[provider-comparison.md](provider-comparison.md) and [cost-model.md](cost-model.md).
