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
