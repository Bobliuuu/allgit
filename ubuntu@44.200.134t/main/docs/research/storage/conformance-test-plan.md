# Backend conformance and contract test plan, per provider

Issue #181, epic #190. This is a plan plus operator commands. **Nothing here was run against any provider and no
billable command is to be run by an agent**; an operator with approved credentials runs the cloud rows. Local rows
(filesystem, in-memory, local MinIO/RustFS/Ceph containers) are free.

Trait under test: `vcs_remote::Backend` (`crates/vcs-remote/src/backend.rs`, revision `cd0fd09`): `get`, `get_range`,
`exists`, `put_if_absent`, `put_if_match`, `list`. Existing coverage: `backend::conformance(&dyn Backend)` (one
sequential function covering create-only, range edge cases, stale CAS, CAS on missing key, list, key validation),
`crates/vcs-store-s3/tests/s3.rs` (fake HTTP server tests, always run; MinIO tests gated on `VCS_TEST_S3_ENDPOINT`),
`S3Backend::connect` probe (create-only twice, stale If-Match, valid If-Match), `crates/vcs-remote/tests/model*.rs`
(model checking over an in-memory backend).

## 1. Tiers

| Tier | Purpose | Cost | Runs |
|---|---|---|---|
| T0 | Existing `conformance()` plus `connect` probe | a few dozen requests | every provider |
| T1 | Proposed contract tests below (concurrency, ABA, pagination, sizes, key space) | under 3,000 PUT, 6,000 GET (see 4) | every provider |
| T2 | Provider-specific probes (documented quirks in provider-comparison.md) | handful of requests | per provider |
| T3 | Soak: sustained manifest CAS at increasing rate to find the throttle point | bounded by `--max-requests`, operator sets | R2, GCS, S3 only, operator approved |

T1 and T2 are not implemented yet. Proposal: a new integration test file `crates/vcs-store-s3/tests/contract.rs`
(gated by the same `VCS_TEST_S3_*` variables plus `VCS_TEST_S3_REGION`, which does not exist yet: the gated helper
currently leaves `region` at its default `us-east-1`, which is wrong for R2 (`auto`) and B2/regional AWS endpoints) and a
reusable `contract(&dyn Backend)` function in `vcs-remote::backend` next to `conformance`, so the in-memory and
filesystem backends run the same cases for free.

## 2. Proposed T1 contract cases

Each case lists the expected outcome per the trait docs; a provider failing a MUST case is not usable as
authoritative.

| # | Case | Assertion | MUST |
|---|---|---|---|
| C1 | Concurrent create race: N=16 threads `put_if_absent(k, payload_i)`, 50 rounds on fresh keys | exactly one `Ok(true)` per round; all others `Ok(false)`; final `get` equals the winner's payload | yes |
| C2 | Concurrent CAS race: N=16 threads read tag t, `put_if_match(k, new_i, t)` | exactly one `Some(tag)`; losers `None`; no lost update after 100 rounds (counter-increment loop reaches N x rounds) | yes |
| C3 | ABA: write A, record tag tA, write B, write A again, `put_if_match(k, C, tA)` | Report whether it succeeds. For ETag-as-MD5 providers it is expected to succeed (HYPOTHESIS); record as a property, and verify the manifest format makes identical-bytes rewrite impossible (epoch field) | informational + manifest check |
| C4 | Read-after-write on overwrite | after `put_if_match` returns, 200 immediate `get`s from 4 threads all return the new bytes | yes |
| C5 | Read-after-write for new key and `list` visibility | immediately after `put_if_absent` returns, `list(prefix)` contains the key (100 repetitions) | yes |
| C6 | Delete-then-create | after delete, `put_if_absent` returns `Ok(true)`; CAS on deleted key returns `None` (S3 returns 404, which `put_if_match` maps to `None`) | yes |
| C7 | Pagination | write 2,500 keys under one prefix; `list` returns all exactly once, sorted as the trait documents; run with 1,001 and 1,000 keys for page-boundary behaviour | yes |
| C8 | Key space | keys with `+`, `%`, space, unicode, `=`, `&`, 512-byte key, key differing only by case; round-trip `get`/`list` | yes |
| C9 | Sizes | 0 B, 1 B, 1 MiB, 8 MiB (single PUT), 100 MiB; ranged reads at 0, middle, last byte, `offset = len`, `offset > len`, `len = 0`, spanning the end; bytes and ETag stable across repeated reads | yes |
| C10 | Large single PUT at the provider's multipart threshold (S3/GCS/R2: 5 GiB single-PUT ceiling; do not run above 100 MiB without approval) | informational: record the largest single PUT accepted | no |
| C11 | Idempotent retry | repeat identical `put_if_absent` after a simulated lost response (fault-injecting wrapper returns `Unavailable` after the request was sent) | yes |
| C12 | Throttle behaviour | burst 200 `put_if_match` on one key from 8 threads; every result is `Ok(Some|None)` or `Err(Unavailable)`, never a misreported success; record 429/503 count | yes |
| C13 | Same-key write rate | 1 writer, `put_if_match` loop for 30 s; record achieved writes/s and any 429s. R2 and GCS document a 1 write/s/key limit | informational (drives #186 metadata sharding) |
| C14 | Conditional writes on multipart completion | out of scope: allgit's backend does not issue multipart (documented here because S3 supports it and GCS XML rejects preconditions in multipart) | no |
| C15 | Clock/ordering sanity | `Versioned.tag` of two successive versions differ whenever bytes differ | yes |

## 3. Per-provider run sheet

Variables read by the existing gated tests: `VCS_TEST_S3_ENDPOINT`, `VCS_TEST_S3_BUCKET` (default `vcs-test`),
`VCS_TEST_S3_ACCESS_KEY`, `VCS_TEST_S3_SECRET_KEY` (defaults `minioadmin`). Each test run uses a unique key prefix. HTTPS
endpoints need `--features tls`. Existing T0 command (any provider):

```
VCS_TEST_S3_ENDPOINT=<endpoint> VCS_TEST_S3_BUCKET=<bucket> VCS_TEST_S3_ACCESS_KEY=<key> VCS_TEST_S3_SECRET_KEY=<secret> \
  cargo test -p vcs-store-s3 --features tls --test s3 -- --nocapture
```

The same command runs T1 once `contract.rs` exists (`--test contract`). Run T0 first; if `connect` refuses the
provider, record that as the result (it is a finding, not a failure to fix) and stop for that provider.

### Local, free

| Backend | Setup | Expected (HYPOTHESIS until run) |
|---|---|---|
| Filesystem / in-memory | `cargo test -p vcs-remote` | pass T0/T1 by construction; this is the baseline for the cases |
| MinIO (test double; upstream archived) | `docker run -d --name vcs-minio -p 9000:9000 -e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin quay.io/minio/minio server /data` (from `docs/s3.md`); endpoint `http://127.0.0.1:9000`; record `docker image inspect` digest and `minio --version` | pass T0; C1/C2 are the interesting cases because of the reported read-quorum issue (single-node here, so the issue cannot be exercised; note this limit) |
| MinIO distributed (4 drives, 1 node) | 4 `--volume` mounts on the Linux box under `~/rt/<name>`; clean up after | exercise quorum paths; still not a substitute for multi-node failure |
| RustFS | run the project's container/binary per its README (image name and flags not verified here [U]); record version | unknown; T0 `connect` probe decides |
| Ceph RGW | single-node `ceph/demo`-style or `vstart.sh` cluster (procedure not verified here [U]); record `ceph --version` | `connect` probe decides; if RGW lacks conditional PutObject it fails as authoritative |

### Cloud, billable: operator only

Preconditions for every provider: a dedicated empty bucket used only for this test; a budget alarm at 1 USD; credentials
scoped to that bucket; a lifecycle/expiry rule on the test prefix (1 day) as safety net against leftovers; test deletes
its prefix on success. Worst-case spend per run is estimated in cost-model.md section 6 (S3 $0.14, GCS $0.16, R2
$0.03, B2 $0.01 for the T1 workload with one month of storage charged; real spend is smaller). T3 is not covered by that estimate: the operator sets
`--max-requests` from `scripts/storage_cost_model.py`.

| Provider | Endpoint (path-style) | Region / auth | Notes and expected result |
|---|---|---|---|
| AWS S3 | `https://s3.us-east-1.amazonaws.com` (or the bucket's region) | access key, SigV4; region must match the bucket (needs `VCS_TEST_S3_REGION`) | expected pass T0/T1 (conditional writes documented). Also run once from an EC2 instance in the same region to separate network from service latency |
| Cloudflare R2 | `https://<ACCOUNT_ID>.r2.cloudflarestorage.com` | R2 API token; region `auto` | expected pass T0; C13 expected to show 429 at about 1 write/s/key; also test `r2.dev` is NOT used |
| Google Cloud Storage (S3 interoperability) | `https://storage.googleapis.com` | HMAC key for a service account | expected `connect` refusal (ETag `If-Match` on PUT undocumented) [H]. If refused, T2: send `x-goog-if-generation-match: 0` and `x-goog-if-generation-match: <generation>` manually (curl) to confirm the generation-based primitive; record outcome and design a generation-tag backend only if the decision (#188) needs GCS |
| Backblaze B2 (S3 API) | `https://s3.<region>.backblazeb2.com` | application key; region string from the bucket endpoint | expected `connect` refusal on `If-None-Match` [H, from third party]. T2: one PUT with `If-None-Match: *` and one with `If-Match`, record status codes and bodies |

T2 probe, one provider at a time (cheap, a few requests; shown with AWS CLI v2 syntax):

```
aws s3api put-object --endpoint-url <endpoint> --bucket <bucket> --key probe/a --body f --if-none-match '*'   # expect 200
aws s3api put-object --endpoint-url <endpoint> --bucket <bucket> --key probe/a --body f --if-none-match '*'   # expect 412
aws s3api put-object --endpoint-url <endpoint> --bucket <bucket> --key probe/a --body f --if-match '"deadbeef"'   # expect 412
```

(The AWS CLI `--if-match`/`--if-none-match` flags exist only in recent CLI versions; if absent use `curl --aws-sigv4`.
CLI version support was not verified here.)

## 4. Recording results

Each provider run writes `docs/research/storage/data/conformance-<provider>-<date>.json`:
`{provider, endpoint_host, region, storage_class, date, client_location, backend_version, t0:{pass,connect_error},
t1:{C1:{pass,notes},...}, t2:{...}, observed_status_codes:{...}, cost_usd_actual}`. Failures are recorded, not hidden;
a MUST failure marks the provider "not authoritative-capable" in provider-comparison.md with the date and version. Update
the matrix cells from [V]/[U]/[H] to a CONFIRMED-by-test tag only from these files.

## 5. Gaps this plan exposes in the current code (HYPOTHESIS, to confirm)

* The gated tests hard-code region `us-east-1` and credentials defaults for MinIO; R2 and B2 need a region parameter.
* `conformance()` is sequential; it cannot detect a non-linearizable CAS. C1/C2 are the missing safety tests.
* No test fixes the ETag ABA behaviour (C3).
* `S3Backend` path-style addressing and verbatim ETag tag may not suit GCS generations (see provider-comparison.md).
