# Manifest scaling (issue #252)

The authoritative remote state is one object, `manifest`, replaced by compare-and-swap on every publish and
downloaded by every clone and fetch. This chapter records what it costs as a repository grows, what was changed,
and the sharding decision demanded by the roadmap gate. Raw data: `docs/research/storage/data/manifest/`.
Harness: `crates/vcs-bench/src/bin/manifestbench.rs` (`sizes`, `publish`, `fetch`, `shardpublish`).

## Method and honesty notes

* Synthetic manifests: `zone-NNNNN` zones, branches `main` and `feature/topic-N`, one tag per zone, locks on a tenth of
  the zones, object ids are blake3 hashes (incompressible, like real ones).
* Sizes and CPU: AWS instance b (8 vCPU), load1 about 5-6 from another agent's benchmark, so NOT the quiet machine the
  brief asks for. Median of 21 runs (5 at 10,000 zones). CPU numbers are single-threaded wall time of `encode` and
  `decode` (decode includes name validation, ordering checks and every limit).
* Publish latency, CAS conflicts, throughput: the Lambda box (30 cores, load1 19-40 throughout, shared with many agents)
  against servers started for this run on loopback: RustFS 1.0.4 (`127.0.0.1:19300`) and moto 5.2.5 (a single-threaded
  Python test double, so its throughput ceiling is the server, not the protocol). **Each cell is ONE run**, not a
  median; 20 publishes per writer (6 at 10,000 zones). Treat differences under about 30 percent as noise. These are
  local-disk, loopback numbers and say nothing about AWS S3, R2 or GCS (see provider-comparison.md, LOCAL).
* Every writer publishes one commit to its own zone, so all writers are logically disjoint; every CAS loss is pure
  contention on the single key, resolved by re-validating only the writer's own `old` heads.
* The "no backoff" sweep was run before the backoff change, the "backoff" sweep after; the code is otherwise the same.
* Not run: real cloud object stores, R2/GCS (compared by reasoning only), more than 16 publishers, branches/zone other
  than 5 in the concurrency sweeps, multi-zone transactions in the sharded benchmark.

## Results

### Sizes and CPU (AWS b, 8 vCPU, load1 about 5-6, medians of 21 runs, 5 at 10k zones)

| zones | br/zone | tags | locks | applied | v1-4 bytes | v5 bytes | v5/v1-4 | decode+validate v1-4 us | v5 us | encode v1-4 us | v5 us |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 10 | 1 | 0 | 0 | 0 | 802 | 533 | 66% | 5 | 10 | 1 | 2 |
| 10 | 1 | 10 | 1 | 0 | 1,334 | 1,012 | 76% | 10 | 18 | 1 | 3 |
| 10 | 1 | 10 | 1 | 4096 | 132,407 | 132,085 | 100% | 28 | 34 | 12 | 17 |
| 10 | 5 | 0 | 0 | 0 | 3,162 | 2,073 | 66% | 23 | 29 | 2 | 4 |
| 10 | 5 | 10 | 1 | 0 | 3,694 | 2,552 | 69% | 27 | 36 | 2 | 5 |
| 10 | 5 | 10 | 1 | 4096 | 134,767 | 133,625 | 99% | 49 | 58 | 15 | 22 |
| 100 | 1 | 0 | 0 | 0 | 7,912 | 4,880 | 62% | 66 | 115 | 4 | 13 |
| 100 | 1 | 100 | 10 | 0 | 13,187 | 9,526 | 72% | 127 | 197 | 6 | 25 |
| 100 | 1 | 100 | 10 | 4096 | 144,260 | 140,599 | 97% | 145 | 216 | 19 | 39 |
| 100 | 5 | 0 | 0 | 0 | 31,513 | 20,280 | 64% | 282 | 364 | 12 | 36 |
| 100 | 5 | 100 | 10 | 0 | 36,788 | 24,926 | 68% | 362 | 442 | 15 | 48 |
| 100 | 5 | 100 | 10 | 4096 | 167,861 | 155,999 | 93% | 373 | 481 | 32 | 64 |
| 1000 | 1 | 0 | 0 | 0 | 79,014 | 48,379 | 61% | 805 | 1,308 | 37 | 149 |
| 1000 | 1 | 1000 | 100 | 0 | 131,810 | 94,804 | 72% | 1,452 | 2,186 | 65 | 253 |
| 1000 | 1 | 1000 | 100 | 4096 | 262,883 | 225,877 | 86% | 1,496 | 2,241 | 84 | 284 |
| 1000 | 5 | 0 | 0 | 0 | 315,014 | 202,379 | 64% | 3,085 | 4,022 | 129 | 349 |
| 1000 | 5 | 1000 | 100 | 0 | 367,810 | 248,804 | 68% | 3,815 | 4,901 | 143 | 486 |
| 1000 | 5 | 1000 | 100 | 4096 | 498,883 | 379,877 | 76% | 3,894 | 4,992 | 158 | 482 |
| 10000 | 1 | 0 | 0 | 0 | 790,014 | 483,376 | 61% | 9,855 | 14,974 | 392 | 1,501 |
| 10000 | 1 | 10000 | 1000 | 0 | 1,319,785 | 949,374 | 72% | 16,434 | 23,998 | 792 | 2,863 |
| 10000 | 1 | 10000 | 1000 | 4096 | 1,450,858 | 1,080,447 | 74% | 16,597 | 24,423 | 866 | 3,208 |
| 10000 | 5 | 0 | 0 | 0 | 3,150,015 | 2,023,376 | 64% | 39,018 | 47,394 | 2,702 | 5,964 |
| 10000 | 5 | 10000 | 1000 | 0 | 3,679,786 | 2,489,374 | 68% | 43,815 | 59,214 | 4,583 | 10,298 |
| 10000 | 5 | 10000 | 1000 | 4096 | 3,810,859 | 2,620,447 | 69% | 45,335 | 57,381 | 3,045 | 8,955 |

### Concurrent publishers, single manifest (5 branches/zone, 1 tag/zone; each cell one run)

| server | zones | writers | backoff | publishes/s | p50 ms | p95 ms | CAS lost | retries/publish | requests/publish |
|---|---|---|---|---|---|---|---|---|---|
| RustFS | 100 | 1 | no | 4.1 | 196.41 | 547.46 | 0.0% | 0.0 | 6.05 |
| RustFS | 100 | 1 | yes | 24.1 | 39.85 | 49.89 | 0.0% | 0.0 | 6.05 |
| RustFS | 100 | 2 | no | 8.8 | 141.72 | 756.59 | 0.0% | 0.0 | 6.05 |
| RustFS | 100 | 2 | yes | 34.4 | 54.65 | 76.1 | 0.0% | 0.0 | 6.05 |
| RustFS | 100 | 4 | no | 64.4 | 57.03 | 88.68 | 11.1% | 0.125 | 6.3 |
| RustFS | 100 | 4 | yes | 56.6 | 60.64 | 95.14 | 20.8% | 0.263 | 6.58 |
| RustFS | 100 | 8 | no | 21.4 | 229.22 | 1114.27 | 81.7% | 4.456 | 14.96 |
| RustFS | 100 | 8 | yes | 22.8 | 143.6 | 1079.13 | 64.0% | 1.775 | 9.6 |
| RustFS | 100 | 16 | no | 4.5 | 2247.9 | 10530.15 | 92.6% | 12.531 | 31.11 |
| RustFS | 100 | 16 | yes | 12.1 | 422.55 | 5003.88 | 82.7% | 4.778 | 15.61 |
| RustFS | 1,000 | 1 | no | 11.8 | 66.28 | 148.07 | 0.0% | 0.0 | 6.05 |
| RustFS | 1,000 | 1 | yes | 12.6 | 75.38 | 104.5 | 0.0% | 0.0 | 6.05 |
| RustFS | 1,000 | 2 | no | 32.0 | 60.12 | 71.17 | 2.4% | 0.025 | 6.1 |
| RustFS | 1,000 | 2 | yes | 20.4 | 82.82 | 156.54 | 4.8% | 0.05 | 6.15 |
| RustFS | 1,000 | 4 | no | 26.8 | 97.36 | 265.47 | 60.8% | 1.55 | 9.15 |
| RustFS | 1,000 | 4 | yes | 15.7 | 153.99 | 627.1 | 50.6% | 1.025 | 8.1 |
| RustFS | 1,000 | 8 | no | 9.5 | 385.37 | 3168.33 | 81.9% | 4.531 | 15.11 |
| RustFS | 1,000 | 8 | yes | 16.9 | 167.16 | 1684.73 | 70.6% | 2.4 | 10.85 |
| RustFS | 1,000 | 16 | no | 6.9 | 1175.62 | 7742.6 | 92.6% | 12.444 | 30.94 |
| RustFS | 1,000 | 16 | yes | 12.1 | 476.99 | 4578.49 | 85.3% | 5.787 | 17.62 |
| RustFS | 10,000 | 1 | no | 6.5 | 134.2 | 241.65 | 0.0% | 0.0 | 6.17 |
| RustFS | 10,000 | 1 | yes | 4.9 | 202.33 | 274.42 | 0.0% | 0.0 | 6.17 |
| RustFS | 10,000 | 2 | no | 8.1 | 225.16 | 340.43 | 47.8% | 0.917 | 8.0 |
| RustFS | 10,000 | 2 | yes | 7.0 | 270.93 | 284.74 | 47.8% | 0.917 | 8.0 |
| RustFS | 10,000 | 4 | no | 8.2 | 351.08 | 846.3 | 71.1% | 2.458 | 11.08 |
| RustFS | 10,000 | 4 | yes | 6.5 | 398.03 | 1293.51 | 67.6% | 2.083 | 10.33 |
| RustFS | 10,000 | 8 | no | 7.2 | 649.92 | 2237.98 | 84.6% | 5.5 | 17.17 |
| RustFS | 10,000 | 8 | yes | 7.1 | 467.07 | 2503.87 | 80.4% | 4.104 | 14.38 |
| RustFS | 10,000 | 16 | no | 6.2 | 1489.99 | 5089.33 | 91.7% | 10.99 | 28.15 |
| RustFS | 10,000 | 16 | yes | 6.4 | 1284.96 | 5647.19 | 89.3% | 8.344 | 22.85 |
| moto | 100 | 1 | no | 43.1 | 20.85 | 25.5 | 0.0% | 0.0 | 6.05 |
| moto | 100 | 1 | yes | 42.0 | 23.64 | 26.54 | 0.0% | 0.0 | 6.05 |
| moto | 100 | 2 | no | 44.7 | 41.08 | 60.39 | 2.4% | 0.025 | 6.1 |
| moto | 100 | 2 | yes | 48.4 | 40.43 | 45.9 | 2.4% | 0.025 | 6.1 |
| moto | 100 | 4 | no | 36.6 | 85.57 | 147.58 | 41.6% | 0.713 | 7.47 |
| moto | 100 | 4 | yes | 44.5 | 74.11 | 182.32 | 21.6% | 0.275 | 6.6 |
| moto | 100 | 8 | no | 18.8 | 222.6 | 1021.33 | 66.2% | 1.962 | 9.97 |
| moto | 100 | 8 | yes | 39.1 | 119.51 | 439.37 | 42.7% | 0.744 | 7.54 |
| moto | 100 | 16 | no | 9.6 | 948.79 | 5135.69 | 83.2% | 4.966 | 15.98 |
| moto | 100 | 16 | yes | 22.6 | 338.33 | 2393.89 | 74.8% | 2.962 | 11.97 |
| moto | 1,000 | 1 | no | 29.3 | 30.88 | 47.08 | 0.0% | 0.0 | 6.05 |
| moto | 1,000 | 1 | yes | 30.5 | 31.18 | 41.69 | 0.0% | 0.0 | 6.05 |
| moto | 1,000 | 2 | no | 39.1 | 46.76 | 71.1 | 13.0% | 0.15 | 6.35 |
| moto | 1,000 | 2 | yes | 29.7 | 49.07 | 122.16 | 13.0% | 0.15 | 6.35 |
| moto | 1,000 | 4 | no | 25.6 | 122.97 | 366.63 | 61.0% | 1.562 | 9.18 |
| moto | 1,000 | 4 | yes | 29.4 | 84.37 | 396.28 | 49.0% | 0.963 | 7.97 |
| moto | 1,000 | 8 | no | 24.1 | 227.92 | 785.97 | 71.4% | 2.5 | 11.05 |
| moto | 1,000 | 8 | yes | 18.1 | 254.56 | 1224.06 | 67.9% | 2.112 | 10.28 |
| moto | 1,000 | 16 | no | 12.4 | 790.82 | 3646.41 | 85.3% | 5.791 | 17.63 |
| moto | 1,000 | 16 | yes | 18.0 | 436.35 | 2425.88 | 78.9% | 3.734 | 13.52 |
| moto | 10,000 | 1 | no | 7.8 | 112.32 | 215.88 | 0.0% | 0.0 | 6.17 |
| moto | 10,000 | 1 | yes | 8.1 | 107.83 | 198.79 | 0.0% | 0.0 | 6.17 |
| moto | 10,000 | 2 | no | 6.1 | 229.94 | 502.17 | 45.5% | 0.833 | 7.83 |
| moto | 10,000 | 2 | yes | 8.3 | 215.68 | 333.1 | 47.8% | 0.917 | 8.0 |
| moto | 10,000 | 4 | no | 5.9 | 353.49 | 1171.23 | 67.1% | 2.042 | 10.25 |
| moto | 10,000 | 4 | yes | 8.3 | 248.23 | 1056.94 | 67.6% | 2.083 | 10.33 |
| moto | 10,000 | 8 | no | 6.5 | 840.06 | 2471.37 | 84.1% | 5.271 | 16.71 |
| moto | 10,000 | 8 | yes | 7.8 | 563.03 | 2321.99 | 79.6% | 3.896 | 13.96 |
| moto | 10,000 | 16 | no | 6.7 | 1185.88 | 5513.5 | 88.9% | 8.021 | 22.21 |
| moto | 10,000 | 16 | yes | 7.9 | 870.28 | 5506.52 | 85.5% | 5.885 | 17.94 |

### Sharded prototype, same workload (single-zone transactions, 4 object PUTs included)

| server | zones | writers | publishes/s | p50 ms | p95 ms | CAS lost | requests/publish |
|---|---|---|---|---|---|---|---|
| RustFS | 100 | 1 | 7.5 | 122.69 | 159.18 | 0.0% | 12.0 |
| RustFS | 100 | 2 | 13.5 | 126.38 | 190.48 | 0.0% | 12.0 |
| RustFS | 100 | 4 | 17.8 | 145.92 | 462.41 | 0.0% | 12.0 |
| RustFS | 100 | 8 | 27.3 | 168.15 | 1016.36 | 0.0% | 12.0 |
| RustFS | 100 | 16 | 58.1 | 222.8 | 457.96 | 0.0% | 12.0 |
| RustFS | 10,000 | 1 | 5.1 | 204.83 | 218.13 | 0.0% | 12.0 |
| RustFS | 10,000 | 2 | 9.5 | 170.66 | 191.0 | 0.0% | 12.0 |
| RustFS | 10,000 | 4 | 16.0 | 246.01 | 308.61 | 0.0% | 12.0 |
| RustFS | 10,000 | 8 | 41.5 | 189.71 | 222.68 | 0.0% | 12.0 |
| RustFS | 10,000 | 16 | 70.0 | 214.49 | 296.36 | 0.0% | 12.0 |
| moto | 100 | 1 | 15.1 | 64.15 | 81.67 | 0.0% | 12.0 |
| moto | 100 | 2 | 15.4 | 102.73 | 235.62 | 0.0% | 12.0 |
| moto | 100 | 4 | 23.5 | 168.43 | 181.02 | 0.0% | 12.0 |
| moto | 100 | 8 | 23.6 | 332.28 | 375.02 | 0.0% | 12.0 |
| moto | 100 | 16 | 23.4 | 653.67 | 800.14 | 0.0% | 12.0 |
| moto | 10,000 | 1 | 16.8 | 63.61 | 74.86 | 0.0% | 12.0 |
| moto | 10,000 | 2 | 21.9 | 92.06 | 102.52 | 0.0% | 12.0 |
| moto | 10,000 | 4 | 23.7 | 168.79 | 175.4 | 0.0% | 12.0 |
| moto | 10,000 | 8 | 20.9 | 356.22 | 478.24 | 0.0% | 12.0 |
| moto | 10,000 | 16 | 18.8 | 843.32 | 1151.93 | 0.0% | 12.0 |

### Reading the manifest (clone/fetch), 4096 applied ids, no-backoff sweep

| server | zones | manifest bytes | full GET p50 ms | conditional (304) p50 ms |
|---|---|---|---|---|
| RustFS | 10 | 133,625 | 0.76 | 0.32 |
| RustFS | 100 | 155,999 | 3.01 | 0.6 |
| RustFS | 1,000 | 379,877 | 6.18 | 0.53 |
| RustFS | 10,000 | 2,620,447 | 46.62 | 0.63 |
| moto | 10 | 133,625 | 3.48 | 3.14 |
| moto | 100 | 155,999 | 3.85 | 3.19 |
| moto | 1,000 | 379,877 | 9.47 | 3.89 |
| moto | 10,000 | 2,620,447 | 49.06 | 3.72 |

(The v5 rows below 64 KiB are shown for the codec only; `Manifest::encode` keeps the v1-v4 bytes for them.)

## What the data says

1. **Bytes.** Object ids are 32 incompressible bytes each: 1.6 MB of the 3.7 MB at 10,000 zones x 5 branches, plus
   about 0.3 MB for the 10,000 tag ids and 4096 x 32 = 131 KB of applied ids. Names (zone, branch) are the rest. The v5
   layout (zone-grouped, prefix-compressed names) removes the names' redundancy: **62-72 percent of the v1-v4 size**
   without applied ids, 69 percent (2.62 MB vs 3.81 MB) at the 10k x 5 case with the full applied list. It does not
   change the order of growth: bytes are linear in refs (about 63 bytes per branch in v1-v4, about 40 in v5), so 1 MiB is reached at roughly 17,000 branches in v1-v4 and 26,000 in v5.
2. **CPU is as large as the bytes.** Decoding 3.7 MB takes about 44 ms (it validates every name); v5 decode is 20-100
   percent SLOWER (59 ms at that size) and encode about 2-4x slower, because it rebuilds strings. At 10,000 zones one decode is
   already as long as a local full GET (47 ms). So v5 is a bandwidth win and a CPU loss; it pays when the link is slower
   than about 100 MB/s. It is therefore only used above 64 KiB; smaller manifests keep v1-v4 bytes and stay readable by
   older binaries.
3. **Clone/fetch.** A conditional GET of an unchanged manifest costs one small request with no body (RustFS: 0.3-0.6 ms
   vs 0.8-47 ms for the full GET as the manifest grows; moto does not implement 304 and re-sends the body). This already
   exists (`refresh_manifest`, used by `vcs fetch`), so the "ETag cache" item needed no new code. A first clone must
   read the whole manifest once: 2.6 MB at 10k zones.
4. **Publish latency is dominated by contention, not size, at 8 or more writers.** At 100 zones the manifest is 37 KB
   and 16 disjoint writers still lose 75-92 percent of their CAS attempts, with p95 publish latency 2.4-10 s against
   0.03-0.5 s for one writer. Without backoff, losers retried immediately (4-12 retries per publish). Size adds to this:
   at 10,000 zones one publisher alone takes 108-202 ms p50 (decode + encode + 2.5 MB PUT) and two already lose 48
   percent.
5. **Backoff helps but does not remove it.** Randomized exponential wait after a lost CAS (this change) improved p95 and
   throughput at 8-16 writers in the single runs we have (RustFS, 100 zones, 16 writers: p95 10.5 s to 5.0 s, 4.5 to
   12.1 publishes/s; 1,000 zones, 8 writers: 3.2 s to 1.7 s), and changed little at 1-2 writers. At 16 writers 83-89
   percent of CAS attempts are still lost, because one key admits one winner per round trip.
6. **Sharded prototype.** With one CAS key per zone, disjoint writers never conflict (0 percent CAS lost in every cell).
   RustFS, 10,000 zones, 16 writers: 70 publishes/s, p95 296 ms, against 6.4/s and p95 5.6 s for the single manifest.
   The price: 12 requests per publish instead of 6 (two reads, prepare CAS, decision PUT, finalize CAS, plus the same 4
   object PUTs), so one writer is 2-3x SLOWER (RustFS 100 zones: p50 123 ms vs 40 ms).

## Conceptual comparison with R2 and GCS

R2 and GCS limit writes to one per second per object key (provider-comparison.md, rows "Throttling / limits", marked verified there). Every publish is a write of the single `manifest` key, so
those stores bound the whole repository to roughly one publish per second regardless of writer count, and a lost CAS
costs a full round trip plus a 429. With 16 writers the numbers above (6-23 publishes/s on a local server) are already
an order of magnitude more than that limit. Per-zone shards move the limit to one publish per second per zone, and the
decision record is create-only (one write per key). This is reasoning from the documented limit, not a measurement.

## Decisions

### Done (versioned, with legacy read)

* **Manifest v5**, written only when the v1-v4 encoding exceeds 64 KiB (`COMPACT_THRESHOLD`). Decoders read every
  version; v1-v4 bytes are unchanged for small repositories. Strict canonical form (ascending keys, maximal shared
  prefixes), so one manifest still has one encoding. Tested: roundtrip, truncation at every length, bit flips never
  accepted unless canonical, limits, and a model-checker scenario with a >64 KiB manifest (`model.rs`,
  `compact_manifest_survives_faults_at_every_step`: crash, lost CAS and lost ack at every step).
  Cost to know: older binaries cannot read a repository whose manifest crossed 64 KiB. Because the applied list alone is
  131 KB when full, a repository crosses it after about 2,000 publishes.
* **CAS backoff** in `Publisher::run` (not in `step`, so the model checkers are unaffected). Liveness only.
* **Applied-id window knob** `Publisher::with_applied_window(n)`; the default stays `MAX_APPLIED` = 4096 (131 KB).
  Not shrunk by default: see below.
* **ETag cache for clones/fetches**: already present, measured above.

### Applied-id window: why it is not shrunk by default

An applied id lets a publisher whose CAS acknowledgement was lost recognise its own transaction. Dropping an id is safe
only if no publisher retries that transaction afterwards. `tests/model_window.rs` states what a window of N costs:

* fewer than N other publishes in between: the retry is always `AlreadyApplied` (exactly once);
* N or more in between, disjoint branches: the retry is rejected `Diverged` (a spurious error; the effect is present
  once);
* N or more in between and the branch back at its old value (revert, ABA): the retry **applies the transaction again**.
  This is kept as a passing negative-result test.

A time-based window needs a trusted timestamp per entry (the manifest has none; clients' clocks are untrusted and a
skewed client could shrink everyone's window), costs bytes, and would still need the same bound. So N must exceed
(publishes per second the remote can absorb) x (longest a publisher keeps retrying: 8 retries x the request timeout).
For R2/GCS (about 1 write/s per key) 1024 would cover several minutes; for a fast S3-compatible server at 50/s it would
cover 20 s. That is a per-deployment choice, so it is a knob and not a new default. The saving is 32 bytes per entry:
at most 131 KB, 3.5 percent of the 10k x 5 manifest.

## The sharding gate

Roadmap gate: shard if the manifest exceeds 1 MiB, or CAS conflicts exceed 50 percent of p95 publish latency.

* Size: **triggers at roughly 17,000-26,000 refs** (10k zones x 5 branches is 2.5-3.7 MB). Reached by very large
  monorepo-style deployments only.
* Conflicts: **triggers at 8 or more concurrent publishers** on every server we measured (65-90 percent of CAS attempts
  lost; waiting for retries is most of the p95), also at 100 zones, i.e. with a small manifest.

The gate triggers, so a per-zone prototype was built: `crates/vcs-remote/src/shard.rs`, feature `sharded` (off by
default), following `tests/model_sharded.rs` rule for rule (Prepare in ascending zone order, Decide only after all
prepared with a create-only decision record, Finalize; foreign pending records resolved from the decision record, undecided
ones aborted with wound-wait; double-collect snapshot reads). `tests/sharded_impl.rs` covers the codec, divergence
aborting the whole transaction, a crash at every protocol point recovered both by re-running the txid and by another
transaction on the same zones, and a threaded stress with an atomic-pair invariant and a concurrent reader (40 repeated
runs green). It is **not** wired into `Publisher`, fetch, GC, locks, tags or the CLI.

**Decision: do not switch the default.** Reasons: one writer is 2-3x slower; the protocol would need decision-record GC,
a shard-aware remote GC (liveness roots are today the single manifest), locks that span zones (docs/locks.md), the
applied-id idempotency list per shard, and a migration of the manifest; none of that is modelled. Recommended path:

1. Ship v5 + backoff (this change) and recommend fewer than 8 concurrent publishers per repository meanwhile.
2. Model-check the missing pieces (decision GC, remote GC roots, locks across shards) in `model_sharded.rs` first.
3. Integrate behind `sharded` as a repository-level opt-in recorded in a small root manifest (zone list and format
   version), keeping the single-manifest mode as the default for repositories under 1 MiB and 8 publishers.
4. Re-measure on real S3 and R2 (cloud numbers are still owed by #187).
