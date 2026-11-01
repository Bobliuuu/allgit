# Measured: AWS S3 (us-east-1), 2026-11-01

First real-cloud measurement for the storage research (issue #187). Tool: `s3bench` (crates/vcs-bench, `--features s3-tls`), data: `data/s3-aws-us-east-1.json`.

**Setup.** Bucket created for the run in a private AWS account (temporary assumed-role credentials supplied by the owner), us-east-1, public access blocked, standard storage class, **deleted afterwards** (verified: no `allgit-bench` bucket remained). Client: the project's own `S3Backend` (path-style, SigV4, https via rustls) run from the author's Mac over a home internet connection. All latencies are therefore **client-side latencies from that Mac to us-east-1**, not in-region numbers. One run, one trial per cell (n = 20 to 40 requests per cell), so these are indicative, not a statistical comparison. Budget guard: 3,000 requests / 250 MB; actual 2,718 requests, 162.6 MB down, 23.6 MB up.

**Cost of the run (arithmetic on list prices, billing not checked):** about 2 US cents (roughly 330 write-class requests, 2,400 read-class requests, 162.6 MB egress at the internet-egress rate).

## Contract checks (all against real S3)

| check | result |
|---|---|
| create-only PUT (`If-None-Match: *`) creates a new key | pass |
| second create-only PUT on the same key is refused, value unchanged | pass |
| CAS with the current ETag (`If-Match`) succeeds | pass |
| CAS with a stale ETag is refused | pass |
| CAS on a missing key is refused | pass |
| exact range read; range past the end returns empty | pass |
| list under a prefix | pass |
| 8 concurrent writers CAS the same key, 5 rounds: exactly one winner every round | pass ([1,1,1,1,1]) |
| **ETag A-B-A**: write A, then B, then A again | **the ETag after the second A equals the first A's ETag** |
| a writer holding the first ETag then wins a CAS after A-B-A | **yes (stale tag accepted)** |

The last two are the important finding. On S3 the ETag of a simple PUT is the content MD5, so identical bytes reuse the ETag. A CAS keyed on ETag alone is exposed to the classic ABA problem if a manifest could ever return to byte-identical content. The allgit manifest embeds a monotonically increasing generation, so its bytes never repeat and the hazard cannot occur today. That is a property the format must keep (it is now tested on real S3 by this tool, and any metadata redesign must preserve it or add an epoch).

## Latency, client to us-east-1 (ms)

| operation | 1 KiB p50 / p95 | 64 KiB p50 / p95 | 1 MiB p50 / p95 |
|---|---|---|---|
| `put_if_absent` | 159 / 174 | 241 / 276 | 458 / 1193 |
| `get` | 156 / 183 | 193 / 215 | 328 / 345 |
| `head` | 153 / 167 | 150 / 164 | 152 / 194 |
| `get_range` (512 B) | 156 / 174 | 152 / 168 | 156 / 168 |
| refused create-only PUT | 146 / 158 | 150 / 168 | 149 / 179 |
| manifest-style CAS (8 KiB, sequential) | 176 / 212 (p99 270) | | |

Concurrent GET of one 64 KiB object: 1 thread 5.3 req/s, 4 threads 20.6, 16 threads 78.9, 32 threads 152.9 (throughput scales almost linearly with threads, i.e. the cost is per-request latency, not bandwidth or server throttling at this scale).

## What this tells us

- **Every request costs about 150 ms regardless of size or kind** (HEAD, 512-byte range, refused PUT all about 150 ms). From this client that is dominated by connection setup plus round trip; it is consistent with the audit's hypothesis F17 that the S3 backend opens a new connection per request (`max_idle_connections(0)`), but this run did not isolate that: a pooled-connection run from the same client is needed (roadmap #243). Until then treat "about 150 ms per request" as the observed per-request floor from this client.
- **Request count therefore dominates time**, which supports the remote-pack work (a one-file publish went from about 5,000 requests to 6 in the in-memory counting harness): at 150 ms each, 5,000 sequential requests is about 12 minutes, 6 is about 1 second. These are arithmetic on measured per-request latency, not a measured publish.
- **Single-manifest CAS is about 176 ms p50 sequentially**, and 8 racing writers always produced exactly one winner, so the protocol's linearization point behaves as modelled on real S3. Sustained commit rate for one writer is therefore bounded near 5 to 6 commits per second from this client; contention retries add more.
- **Throughput scales with concurrency**, so parallel ranged GETs are worthwhile for clones (consistent with `fetch_many`).

## Not measured / limits

- One run from one residential network location; no in-region (EC2) run, so real production latency would be much lower.
- Not measured: multipart upload, throttling behaviour (503 SlowDown) at high rates, other regions, S3 Express, cross-region, KMS-encrypted objects (their ETag is not an MD5), lifecycle behaviour, long-running GC deletions.
- Cloudflare R2, GCS and B2 are still unmeasured (R2 needs its own credentials; see the provider matrix for documented differences, e.g. the 1 write per second per key limit on R2 and GCS).
