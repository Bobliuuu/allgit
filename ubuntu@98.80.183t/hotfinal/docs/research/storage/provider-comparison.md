# Provider capability matrix

Issue #181, epic #190. Compiled 2026-10-31 from provider documentation fetched that day. Tags: **[V]** stated by the
provider's own documentation page cited in the Sources section; **[3P]** third-party report, not provider
documentation; **[U]** unverified (not found in the pages fetched, or only recalled); **[H]** HYPOTHESIS about
consequences for allgit. No capability was tested against a live provider: nothing here is a measurement.

What allgit requires of an authoritative backend (`crates/vcs-remote/src/backend.rs`, revision `cd0fd09`): read-after-write
for new keys, range reads, create-only PUT (`If-None-Match: *`), compare-and-swap PUT keyed on an opaque tag
(S3: `If-Match: <ETag>`). `S3Backend::connect` (`crates/vcs-store-s3/src/lib.rs`) probes these four behaviours and
refuses a provider that fails; it uses path-style SigV4 over HTTP(S) and takes the version tag from the response `ETag`
header verbatim.

## 1. Matrix

| Capability | AWS S3 | Cloudflare R2 | Google Cloud Storage | Backblaze B2 | MinIO | RustFS | Ceph RGW |
|---|---|---|---|---|---|---|---|
| Create-only PUT (`If-None-Match: *`) | [V] PutObject, CompleteMultipartUpload, CopyObject; 412 on exists, 409 on concurrent delete | [V] PutObject supported, evaluated atomically against committed state; exactly one concurrent `If-None-Match: *` writer succeeds, others 412 | [V] via `x-goog-if-generation-match: 0` (not `If-None-Match`); 412 on exists | [U] not documented; [3P] July 2025 Proxmox patch says B2 rejects the header on PUT | [V] AIStor docs list If-None-Match on PutObject, CopyObject, CreateMultipartUpload; OSS server behaviour [U] (repo archived) | [3P] one downstream CI says works on RustFS 1.0.0; own docs [U] | [U] PutObject docs list no If-None-Match (checked); release support [U] |
| Compare-and-swap PUT (`If-Match: <ETag>`) | [V] PutObject, CompleteMultipartUpload, CopyObject; 412 on mismatch; 404 if key missing/delete marker; 409 on conflict | [V] PutObject `If-Match` supported, atomic | [V] CAS is by generation: `x-goog-if-generation-match: <gen>`; `If-Match`/`If-None-Match` documented for GET/HEAD only, not PUT; preconditions unsupported in XML multipart (400) | [U] | [V] AIStor docs list If-Match on PutObject, CopyObject, DeleteObject; OSS [U] | [U] (DELETE If-Match is parsed but not enforced per RustFS discussion #2602 [3P]) | [U] |
| Tag semantics | ETag; = content MD5 for plain objects [U, not in fetched pages]; so rewriting identical bytes leaves ETag unchanged (ABA) [H] | ETag; derivation [U] | generation number, unique per write (monotonic per object name) [V: documented as the version identifier] | [U] | ETag [U] | [U] | ETag [U] |
| Consistency | [V] strong read-after-write for PUT/DELETE and LIST, HEAD/metadata; bucket config eventual | [V] strongly consistent read, metadata, list; relaxed only through cached custom domains (not S3 API / Workers bindings) | [V] strong read-after-write, after-delete, listing; exceptions: public cached objects (up to 60 min default), access-change propagation (~1 min), recreated bucket (minutes) | [U] not stated in fetched pages | [V] "strict read-after-write and list-after-write" on xfs/zfs/btrfs; not guaranteed on NFS; ext4 discouraged (distributed README) | [U] | [U] |
| Multipart upload | [V] 5 MiB to 5 GiB parts (no minimum for last), 10,000 parts, max 48.8 TiB | [V] 10,000 parts, 5 GiB single PUT, 4.995 TiB multipart; min part size [U] | [V] XML API: 10,000 parts, 5 MiB to 5 GiB, last part no minimum; max object 5 TiB; also compose of up to 32 sources | [V] "large files" 5 MB to 10 TB, parts >= 5 MB except last, <= 5 GB; max part count not stated [U] | [U] limits not on fetched page; AIStor notes ListMultipartUploads needs exact name prefix | [U] page lists only upload/download | [V] "Supported" in feature table; limits [U] |
| Range reads | [U] not in pages fetched (universally documented in S3 API; verify) | [V] GetObject `Range` and `PartNumber` supported | [U] not in pages fetched | [V] range downloads exist (large-file page: byte-range download returns SHA1 `none`) | [U] | [U] | [V] `Range: bytes=a-b`, Content-Range only if requested |
| Min object size / billing | [V] Standard none; Standard-IA and Glacier IR 128 KB min billable, 30 d / 90 d min duration | [V] none stated (limits page lists no minimum); IA 30 d min duration | [V] Standard none; Nearline 30 d, Coldline 90 d, Archive 365 d min duration | [V] no minimum file-size or duration fees | N/A (self-hosted; disk billing) | N/A | N/A |
| Lifecycle | [V] expiration, transitions, abort incomplete MPU | [V] expiration, transition Standard to IA, abort incomplete MPU; max 1,000 rules; runs within ~24 h | [V] Object Lifecycle Management exists (early-delete charges not applied to lifecycle class changes) | [V] hide/delete rules via console, CLI, native API, max 100 rules, run daily; S3-API lifecycle support [U] ([3P] listed unsupported) | [V] AIStor: AbortIncompleteMultipartUpload not supported in PutBucketLifecycle; other rules [U] | [V] repo README lists ILM as Available | [V] "Bucket Lifecycle: Supported" |
| Throttling / limits | [V] at least 3,500 write and 5,500 read requests per second per partitioned prefix; 503 Slow Down while scaling | [V] 1 write per second per object key (429 above); `r2.dev` throttled; bucket management 50/s | [V] 1 write per second per object name; buckets start ~1,000 writes/s and ~5,000 reads/s then scale | [U] | capacity-bound | capacity-bound | capacity-bound |
| Read/write API path used by `S3Backend` | native | S3 API, path-style endpoint, region `auto` [U whether `us-east-1` accepted] | S3-interop (HMAC) XML endpoint; **CAS via `If-Match` is not documented**, so `connect` probe is expected to fail [H] | S3 API; probe expected to fail on `If-None-Match` [H, from 3P] | S3 API | S3 API | S3 API |
| Licence / status | proprietary service | proprietary service | proprietary service | proprietary service | AGPL-3.0; **GitHub repository archived 2026-04-25, README "NO LONGER MAINTAINED"**; community edition source only, binaries not updated; successor products AIStor Free/Enterprise [V] | Apache-2.0; no stable/beta label on README; features marked Available/Preview [V] | LGPL-2.1 (Ceph) [U]; part of Ceph |

## 2. Findings that matter for the design

1. **R2 and GCS limit writes to one per second per object key** [V]. If the authoritative manifest is one object whose
   CAS is the linearization point (baseline at `cd0fd09`), commit throughput on R2 and GCS is capped near 1 commit per
   second per manifest key, and faster writers receive 429 / throttling errors. S3's documented per-prefix rates do not state a
   per-key write cap [V: absence]. [H] consequence: metadata sharding or batching (#186) matters for R2/GCS and not for S3.
2. **GCS does not document ETag `If-Match` on PUT** [V]; its conditional-write primitive is the generation number
   (`x-goog-if-generation-match`) on the XML API and `ifGenerationMatch` on JSON. The existing `S3Backend` sends
   `If-Match: <ETag>`, so GCS needs either a distinct backend using generations or acceptance that `connect` refuses it. A generation is a
   stronger tag than an ETag (unique per write; no ABA). [H] The `Backend` trait's "opaque version tag" abstraction
   supports both (the tag would carry the generation).
3. **B2 conditional-write support is unverified and probably absent** [U/3P]. Until an operator probe shows otherwise, B2
   is object storage for immutable content-addressed blobs only, with the manifest CAS elsewhere. B2 is by far the cheapest
   store (cost-model.md), so the question is worth a one-request probe (see conformance-test-plan.md, B2 row).
4. **ETag-as-tag has an ABA hazard** if ETag is the content MD5: write A, then B, then A again yields the same ETag as
   the first A, so a stale `If-Match` would wrongly succeed. [H] allgit is safe only if every manifest write embeds a
   strictly increasing epoch or the previous manifest id, making identical bytes impossible. This must be checked in the
   manifest format (#186) and added as a contract test.
5. **MinIO's upstream repository is archived** (2026-04-25) [V]; its AIStor documentation (a different, commercial
   distribution) is where conditional-write support is documented. Using "MinIO" as the local test backend (as `docs/s3.md`
   does with `quay.io/minio/minio`) pins an unmaintained image; treat it as a test double, not as a reference for
   production. Reports of a 304-instead-of-412 quirk (minio/minio issue #19442) and of a conditional PUT passing without
   read quorum are [3P]/unresolved.
6. **RustFS and Ceph RGW conditional-write support is not established** by the sources found. Both must be probed with the
   `connect` probe before any benchmark claims depend on them. [H] Ceph RGW is likely to need a recent release; unverified.
7. Strong consistency is documented for S3, R2, GCS and MinIO (with filesystem conditions), which meets allgit's
   read-after-write requirement; B2, RustFS and Ceph RGW consistency are [U].

## 3. What is not covered

Erasure-coding/durability numbers, SLAs, availability, per-provider latency, request-rate ceilings for B2/MinIO/
RustFS/Ceph, signature versions beyond SigV4 (B2 accepts only SigV4 per its docs [V]), bucket-level object lock, S3
Express One Zone. Absent from the fetched pages does not mean absent from the product.

## LOCAL (not cloud): measured on one machine against self-hosted servers

Issues #242 and #187. Run 2026-10-31/11-01 with `s3bench` (this branch adds `--ca-pem`, `--create-bucket`, `--no-probe`). **Nothing in this section is a cloud measurement and none of it is a statement about AWS S3, R2, GCS or B2.** Every server ran on the same 30-core AMD EPYC 7J13 Linux box as the client, over loopback, with data on local disk: the latencies measure each server's local code path (and the client), not a network, a replicated cluster or a real object-store backend. They must not be compared with, or substituted for, the cloud numbers still owed by #187. Raw JSON: `docs/research/storage/data/s3-local/s3bench-local-<server>-<http|tls>.json`.

### What was tried, in order

| Candidate | Result |
| --- | --- |
| RustFS 1.0.4 release binary (x86_64-gnu zip, github.com/rustfs/rustfs) | WORKED. Single node, one data dir. |
| MinIO built from source: `go install github.com/minio/minio@latest` with Go 1.25.3 (no Go on the box; the official tarball was unpacked in `~/s3local`) | WORKED, version string `DEVELOPMENT.GOGET` (the archived 2026-04-25 upstream HEAD, not a tagged release). Single node, single drive. |
| Garage 2.1.0 static binary | RUNS, but REFUSED by the product: it ignores conditional headers (below). |
| SeaweedFS 4.51 `weed server -s3` | WORKED (needed `-volume.max=100` and a 64 MB volume size limit, or each new bucket failed with HTTP 500 "no writable volumes"). |
| moto 5.2.5 `moto_server` (pip, Python 3.10 venv) | WORKED. A Python test double, not a storage engine. |
| docker | not tried (permission denied on the box, per an earlier finding). |

TLS: a CA plus a leaf certificate (SAN `localhost` and `127.0.0.1`, `CA:FALSE`, serverAuth) was generated with openssl and the CA passed with `--ca-pem`. A single self-signed certificate that is also the trust root is rejected by the client (`CaUsedAsEndEntity`), so a separate CA is required. TLS ran on RustFS, MinIO, SeaweedFS and moto; the client without `--ca-pem` is refused (checked for RustFS). Garage was not run with TLS.

### Contract checks (`s3bench`, same 11 checks plus 8-writer CAS x5 rounds)

| Check | RustFS | MinIO (src) | SeaweedFS | moto | Garage |
| --- | --- | --- | --- | --- | --- |
| create-only PUT (`If-None-Match: *`) first / second refused | pass / pass | pass / pass | pass / pass | pass / pass | pass / **FAIL** (overwrites; value no longer v1) |
| CAS with current ETag | pass | pass | pass | pass | pass |
| CAS with stale ETag refused | pass | pass | pass | pass | **FAIL** |
| CAS on missing key refused | pass | pass | pass | pass | **FAIL** |
| ETag A-B-A: rewriting old content reuses the old ETag | **yes** (hazard present) | yes | yes | yes | yes |
| stale-tag CAS succeeds after A-B-A | yes | yes | yes | yes | yes |
| exact range read; range past end | pass; empty/none | pass; empty/none | pass; empty/none | pass; empty/none | pass; empty/none |
| list under prefix | pass | pass | pass | pass | pass |
| 8 concurrent CAS writers, winners per round (5 rounds) | 1,1,1,1,1 | 1,1,1,1,1 | 1,1,1,1,1 | 1,1,1,1,1 | **8,8,8,8,8** |
| passes `S3Backend::connect` probe | yes | yes | yes | yes | **no, refused** |

Same results on http and TLS for each server that ran both. Garage numbers were taken with `--no-probe`, which bypasses the product's refusal so the behaviour can be characterised; the product itself will not use Garage as an authoritative backend, and its latency row is for a backend that accepts every conditional write unconditionally, so it is not a usable comparison point.

The A-B-A result matches the documented hazard (ETag is the content MD5), so a manifest must still embed an epoch on every one of these servers. That is also what real S3 does for single-part PUTs, but this run does not test S3.

### Latency, p50 ms (loopback, one box; n=40 for 1 KiB/64 KiB, 20 for 1 MiB, 30 for the CAS loop)

| Server / transport | put_if_absent 1 KiB | get 64 KiB | head 64 KiB | get_range 512 B | put 1 MiB | manifest CAS 8 KiB | GET 64 KiB req/s, 1 thread / 32 threads |
| --- | --- | --- | --- | --- | --- | --- | --- |
| RustFS http | 45.8 (warm-up outlier, p95 77) | 0.68 | 0.36 | 0.42 | 21.9 | 9.6 | 2290 / 18067 |
| RustFS tls | 11.5 | 1.04 | 0.68 | 0.75 | 22.2 | 9.0 | 1056 / 13811 |
| MinIO http | 3.5 | 0.92 | 0.73 | 0.81 | 14.3 | 4.5 | 1192 / 13701 |
| MinIO tls | 3.7 | 1.40 | 1.07 | 1.26 | 11.2 | 4.4 | 716 / 6258 |
| SeaweedFS http | 1.5 | 0.87 | 0.57 | 0.77 | 12.2 | 1.6 | 815 / 12812 |
| SeaweedFS tls | 2.2 | 1.59 | 1.07 | 1.36 | 13.7 | 2.4 | 582 / 7494 |
| moto http | 3.2 | 3.07 | 3.02 | 3.14 | 8.2 | 3.2 | 306 / 317 |
| moto tls | 14.3 | 14.09 | 13.92 | 13.97 | 9.7 | 14.2 | 71 / 299 |

Each cell is a single run on a shared 30-core machine (other jobs may have been running); no repeats, so differences of tens of percent are not established. moto serialises requests in one Python process, which explains its flat concurrency scaling. The first run of the series (RustFS http) includes server warm-up.

### Emulator caveat: what differs from real S3

* **moto is an emulator written to mimic S3's API responses, not a storage system.** It keeps objects in process memory, has no durability, and its conditional-write handling is whatever moto implemented; passing here shows moto accepts the headers the client sends, not that S3 behaves the same under load or failure.
* **MinIO here is a source build of the archived upstream at `@latest`, single node, single drive.** Its erasure-coded and distributed modes (where its consistency guarantees come from) were not exercised.
* **RustFS 1.0.4 and SeaweedFS 4.51 are independent reimplementations of the S3 API**, single node. A third-party report cited in the Sources section says RustFS does not enforce `If-Match` on DELETE; `s3bench` does not test conditional DELETE, so that was not checked.
* **Garage** is an independent implementation that, in this version and configuration, does not implement conditional PUT at all.
* None of these servers has S3's multi-AZ replication, request throttling, 503 SlowDown behaviour, or network latency, so retry and tail-latency behaviour under those conditions is untested. "Exactly one CAS winner" here shows the servers serialise concurrent conditional PUTs on one node; a distributed deployment could behave differently.

### Gated conformance tests (`VCS_TEST_S3_ENDPOINT`, http only)

The gated tests have no CA-bundle option, so they were run over plain http only. `cargo test -p vcs-store-s3 --test s3`, `cargo test -p vcs-cli --test remote`, `cargo test -p vcs-cli --test server_e2e` (the S3-backed `vcs-server` end-to-end test):

| Server | vcs-store-s3 (11 tests) | vcs-cli remote (9) | vcs-server S3 e2e (2) |
| --- | --- | --- | --- |
| RustFS | 10 pass, 1 fail | 9 pass | 2 pass |
| MinIO (src) | 10 pass, 1 fail | 9 pass | 2 pass |
| SeaweedFS | 10 pass, 1 fail | 9 pass | 2 pass |
| moto | 10 pass, 1 fail | 9 pass | 2 pass |
| Garage | 5 pass; the 6 server-backed tests fail at `connect` (provider refused, as designed) | 8 pass, 1 fail (same reason) | 1 pass, 1 fail (same reason) |

The one failing test is the same on all four servers that work: `minio_publish_fetch_and_sparse_request_counts` fails the assertion `remote.requests() - r1 > sparse_reqs` (`crates/vcs-store-s3/tests/s3.rs:232`). Instrumented on each server the counts were identical: the sparse fetch issued 18 requests and the full fetch only 4 (bytes 3.6 KB vs 504 KB, so the byte assertions hold). Because it is identical across four independent servers this looks like a stale expectation in the test or a client behaviour change (the full fetch appears to reuse state from the sparse fetch), not a provider difference; it was not investigated or fixed here. Every other server-backed conformance test passed on RustFS, MinIO, SeaweedFS and moto, including the 1,040-key pagination test and 8-way concurrent publish.

### What this does and does not establish

Established (on loopback, single node): `s3bench` and the product's `S3Backend` work against four independent S3 implementations over http and (with a private CA) TLS; the probe correctly refuses a server that ignores conditional writes. Not established: any cloud provider's behaviour, WAN latency, cost, durability, multi-node consistency, or conditional DELETE. Issue #187's cloud run is still owed.

## Sources (accessed 2026-10-31; no revision identifier is exposed by these pages)

S3
* Conditional writes: https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html
* Consistency model (section "Amazon S3 data consistency model"): https://docs.aws.amazon.com/AmazonS3/latest/userguide/Welcome.html
* Request rates: https://docs.aws.amazon.com/AmazonS3/latest/userguide/optimizing-performance.html
* Multipart limits: https://docs.aws.amazon.com/AmazonS3/latest/userguide/qfacts.html
* Min billable size / durations: https://aws.amazon.com/s3/pricing/

R2
* S3 API compatibility (conditionals per operation): https://developers.cloudflare.com/r2/api/s3/api/
* S3 extensions (atomic conditional PUT, concurrent If-None-Match): https://developers.cloudflare.com/r2/api/s3/extensions/
* Consistency: https://developers.cloudflare.com/r2/reference/consistency/
* Limits: https://developers.cloudflare.com/r2/platform/limits/
* Lifecycle: https://developers.cloudflare.com/r2/buckets/object-lifecycles/
* Pricing: https://developers.cloudflare.com/r2/pricing/

GCS (the cloud.google.com URLs 301-redirect to docs.cloud.google.com)
* Request preconditions: https://docs.cloud.google.com/storage/docs/request-preconditions
* XML API headers: https://docs.cloud.google.com/storage/docs/xml-api/reference-headers (first 100,000 of 119,665 characters read)
* Consistency: https://docs.cloud.google.com/storage/docs/consistency
* Quotas and limits: https://docs.cloud.google.com/storage/quotas
* Pricing (min durations, lifecycle note): https://cloud.google.com/storage/pricing

B2
* S3-compatible API overview: https://www.backblaze.com/apidocs/introduction-to-the-s3-compatible-api and https://www.backblaze.com/docs/cloud-storage-s3-compatible-api
* Large files: https://www.backblaze.com/docs/cloud-storage-large-files
* Lifecycle rules: https://www.backblaze.com/docs/cloud-storage-lifecycle-rules
* Pricing/transactions: https://www.backblaze.com/cloud-storage/pricing , https://www.backblaze.com/cloud-storage/transaction-pricing
* [3P] Proxmox patch "s3 client: skip setting If-None-Match header for Backblaze B2" (2025-07): https://lists.proxmox.com/pipermail/pbs-devel/2025-July/014234.html (surfaced by web search; not opened)

MinIO
* Repository status: https://github.com/minio/minio (archived 2026-04-25)
* Distributed/consistency README: https://github.com/minio/minio/blob/master/docs/distributed/README.md
* AIStor S3 compatibility (conditional operations): https://docs.min.io/aistor/developers/s3-api-compatibility/
* [3P] https://github.com/minio/minio/issues/19442 (304 vs 412), via web search snippet only

RustFS
* https://github.com/rustfs/rustfs (README; no revision pinned)
* [3P] https://github.com/orgs/rustfs/discussions/2602 (DELETE If-Match not enforced) and cloud-cache-action v1.5.0 release notes (https://github.com/xSAVIKx/cloud-cache-action/releases), via web search snippets only

Ceph RGW
* S3 feature table: https://docs.ceph.com/en/latest/radosgw/s3/ (development version docs)
* Object operations: https://docs.ceph.com/en/latest/radosgw/s3/objectops/ (PutObject headers; Range)
* Bucket operations: https://docs.ceph.com/en/latest/radosgw/s3/bucketops/

Internal: `crates/vcs-remote/src/backend.rs`, `crates/vcs-store-s3/src/lib.rs`, `docs/s3.md` at revision `cd0fd09`.

Fetch method note: WebFetch returns a model-written summary of each page, not raw text; every [V] cell is therefore
"stated in a summary of the cited page" and should be re-read at the source before it supports a decision.
