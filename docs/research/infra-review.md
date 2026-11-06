# Infra and ops review: failure modes at scale (2026-11-06)

Scope: `deploy/`, `docs/{deploy,operations,server,s3,peering,security,security-review,known-limits}.md`, `.github/`,
`deny.toml`, `crates/{vcs-server,vcs-store-s3,vcs-remote,vcs-watchd}`. Method: read the code and docs, measured what was cheap on
a Mac (nothing launched in any cloud), checked S3 behaviour against AWS documentation (sources at the end). "Measured" means
observed by me in this session; "read" means from the code; "not tested" is said where it applies.

## Ranked findings

| # | Severity | Area | Finding | Status |
|---|---|---|---|---|
| 1 | High | S3 / AWS | Credentials are static env vars read once. No IMDS/IRSA/ECS/SSO provider chain, no reload: role credentials expire and the server then fails every backend call | issue filed |
| 2 | High | upgrade | No minimum-reader marker. The manifest silently switches to the v5 layout above a size threshold and `migrate-sharded` is one-way; an older server cannot read it, so binary rollback = outage | docs + issue filed |
| 3 | High | scale | Thread-per-connection with a 256 cap: 100k concurrent clients are not servable by one process; health probes are refused when full | partly fixed, issue filed |
| 4 | High | S3 / build | `vcs-server --features tls` did not enable `vcs-store-s3/tls`, so the TLS build (and the Docker image, which had no TLS at all) refused every `https://` S3 endpoint, i.e. all real AWS S3 | fixed |
| 5 | High | defaults | `deploy.md` installed `deploy/tokens.example` as the live tokens file; its hashes are public and include an admin | fixed (server refuses them) |
| 6 | Medium | scale | Refusing a connection blocked the single accept thread for 5 ms each | fixed |
| 7 | Medium | DR | Backup/restore is prose only; restoring an old manifest after remote GC can reference deleted objects; no tool, no rehearsal | docs + issue filed |
| 8 | Medium | S3 | One 30 s whole-request timeout; multipart off by default and never validated on real S3; single PUT is capped at 5 GiB; SlowDown handled only by 2 server retries | issue filed |
| 9 | Medium | CI/supply chain | Release matrix used the retired `macos-13` label; no server binary or image is released; no provenance/SBOM; actions pinned by tag; Docker bases unpinned; audit hid failures; no MSRV, fuzz or sanitizer job | partly fixed, issue filed |
| 10 | Medium | observability | No S3-level metrics (per-op counts, 503 SlowDown, 409 conflicts), no group-commit/CAS-loss, in-flight-bytes or thread gauges | issue filed |
| 11 | Medium | S3 policy | Documented manifest guard used `NotPrincipal`+`Deny`, which AWS recommends against | fixed (docs) |
| 12 | Low | systemd | No `LimitNOFILE`/`TasksMax`; thin sandbox | fixed (not run under systemd here) |
| 13 | Low | TLS | TLS is opt-in (build feature and flags); default is plain HTTP on loopback. Sensible default bind, but nothing makes a non-loopback plain-HTTP bind loud | note |

## 1. S3 credentials (High)

`backend_url::s3_config` takes `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN` once; `S3Backend` signs with
them for its life (read: `crates/vcs-server/src/backend_url.rs`, `crates/vcs-store-s3/src/lib.rs` `send_inner`). SIGHUP reloads
tokens, policy and TLS, not credentials. On EC2/ECS/EKS the natural credential is a role, whose keys last 1 to 12 hours. The
server would answer 403/`Other` for every backend call after expiry and the breaker does not count `Other`, so it stays "ready"
while broken. Fix: implement the credential chain (env, container credentials URI, IMDSv2, web identity/IRSA, SSO cache) with
refresh before expiry, or reload on SIGHUP as a stopgap; make `/readyz` fail on persistent 403. Until then the docs (added
to `deploy.md`) say to use a long-lived IAM user key limited by the least-privilege policy, rotated by restart.

## 2. Format gates, upgrade and rollback (High)

`docs/v1.md` already admits "no minimum-reader marker". What that means operationally: `Manifest::encode` switches to v5 once the
v1-v4 encoding is large (`manifest.rs`), `migrate-sharded` is one-way, pack v3/v4 are unreadable to old binaries ("bad index header"),
and the journal makes an older `vcs` rewriting `refs` unsafe. A rolling upgrade is fine; a rollback of the server binary after the
first large publish fails closed (decode error, 5xx) for every client. Fix: a `format` object (or manifest field) naming the
minimum reader version, checked at connect by clients and the server so an old binary refuses with a clear message instead of a
decode error, plus an upgrade runbook (upgrade one replica, keep the pre-upgrade manifest version). Documented now in `deploy.md`.

## 3. 100k concurrent clients and idle sockets (High)

Read: `Server::start` accepts on one thread and spawns one OS thread per connection; `max_connections` defaults to 256; each
connection holds its slot for up to `header_timeout` (10 s) even when idle, then `keepalive_idle` (5 s) between requests.

Measured on a Mac (release build, directory backend, loopback, one process, `ulimit -n 20000`):

| Case | Result |
|---|---|
| 400 idle sockets, cap 256 | 260 threads, 9 MB RSS; 144 immediately answered 503 |
| 3000 idle sockets, cap 4000 | 3004 threads, **82 MB RSS (about 27 KB per idle connection)**; all dropped after the 10 s header deadline; `/healthz` 200 |
| 3000 idle sockets, cap 256, before the fix | opening them took **13.4 s** (0.4 s unloaded): the accept thread spent 5 ms per refusal; 2488 of 2744 refusals delivered; `/healthz` while saturated: **503** |
| same, after the fix | 0.4 s; 2744 of 2744 refusals delivered; `/healthz` still 503 (the cap is global) |

Extrapolation (arithmetic, not measured on Linux): 100k idle connections would be about 2.7 GB of resident memory and 100k
threads. That does not fit the defaults of a stock Linux host: `vm.max_map_count` 65530 (two mappings per thread stack, so roughly
32k threads), `kernel.pid_max` 32768 on older kernels, systemd `TasksMax` (15 percent of pid max by default) and a 1024 soft
`RLIMIT_NOFILE` (the shipped unit had no `LimitNOFILE`). So the real ceiling is the cap you configure plus those limits, and
beyond a few thousand the design needs an event loop for idle/header phases. Consequences at 100k clients:

* A fleet-wide wake-up (CI storm, morning login) gets 503 + `Retry-After: 1` for everything past the cap, which is the right
  failure, but the clients' retry has jitter only if they implement it (`RetryBackend` does, 100 ms base, 3 retries).
* `/healthz` and `/readyz` share the cap, so a saturated node fails its own probes and an orchestrator kills or removes it
  (a retry storm becomes an outage). Fix: reserve slots for loopback/probe paths, or serve probes on a second listener.
* 256 idle sockets from one host deny service for 10 s repeatedly (security-review finding 14, accepted); `--max-conns-per-ip` is
  off by default and useless behind a proxy.
* Behind a proxy/NLB, terminate and multiplex there: the proxy holds the 100k client sockets and the server sees at most
  `max_connections` upstream ones. This is the practical answer today and should be the documented topology.
* Un-ranged `GET` of a pack is buffered whole (`known-limits.md`); memory is connections x largest pack, so the cap must also be
  sized in bytes: 256 x 256 MiB packs is 64 GiB.

Fixed here: `refuse()` no longer waits 5 ms on the accept thread (non-blocking drain of what already arrived). Filed: event-loop
or bounded-pool front end, reserved probe capacity, byte-sized admission.

## 4. S3 limits, throttling, contention, cost

Facts from AWS: at least 3,500 PUT/COPY/POST/DELETE and 5,500 GET/HEAD per second per partitioned prefix; S3 scales by
partitioning on sustained load and returns temporary 503 Slow Down during that; AWS advises backoff, gradual ramp and more
prefixes. Conditional writes: first writer wins, later ones get 412; a 409 can come back for concurrent requests (and for
a delete racing a conditional write); a conditional `PutObject` may simply be retried after 409, while a
`CompleteMultipartUpload` must be restarted from `CreateMultipartUpload`; a concurrent delete gives 404 on `If-Match`.

What the code does:

* **Key layout is fine.** Objects are `objects/<2 hex>/<62 hex>` (256 prefixes, uniformly hashed), packs are content named. Only
  `manifest`, `packs/` listing, `gc/lock`, `gc/clock` and `tx/` are hot spots. The 3,500/s per-prefix write budget therefore
  binds on the **single `manifest` key** and on the `packs/` prefix, not on objects. `docs/research/real-s3.md` drove 3,902 PUT/s
  of 256-byte objects from one prefix for 15 s with zero 503s, so Slow Down has not been observed at this project's scale; the
  many-writers run did not capture the counter (`many-writers-s3.md` says so).
* **Manifest contention.** Every publish is one CAS PUT of an object that is 130 KB with a full applied window and up to MBs at
  10k zones. Same-key writes serialise, so throughput per repository is about 1 / PUT latency (tens of PUT/s at 20 to 50 ms)
  however many clients exist; losers get 412 (or 409, mapped to the same "lost, reload" answer: `put_if_match` returns `None` for
  412/409/404, which is the documented-correct handling for a plain PutObject). Group commit in the server merges stale writes
  for disjoint branches into one PUT, which is what makes 512 writers work against one server; two servers on one bucket
  contend again (`docs/scalability.md`). Manifest bytes also bound egress: N fetches x manifest size (4 MB x 100k = 400 GB, about
  $36 at $0.09/GB internet egress, in-region free). Mitigations that exist: sharded manifest (opt-in, one-way), peers with
  `--upstream-manifest-ttl`.
* **Retries.** `S3Backend` itself never retries 5xx/503; they surface as `Unavailable`. The server wraps it with 2 retries
  (50 ms base, 1 s cap) and a breaker (5 consecutive failures, 5 s cooldown); direct `s3://` clients use `RetryBackend` (3
  retries, 100 ms to 2 s). Both have jitter. Under a genuine Slow Down ramp-up the server returns 503 to clients after about 1 to
  2 s of backend retries and the breaker opens, which turns throttling into a fail-fast outage for all routes including cache hits.
  Fix: Slow Down-aware backoff (longer cap, honour `Retry-After` if present), and do not trip the breaker for 503 SlowDown.
* **Timeouts.** `S3Config.timeout` is 30 s as a ureq whole-request timeout. A 256 MiB pack PUT needs 70 Mbit/s to fit, and a
  large ranged GET shares the budget. Make it per-phase or scale with length.
* **Multipart.** Off unless `VCS_S3_MULTIPART_MB` is set, https only, and the code says it was never validated on a real
  service. Packs are cut at 256 MiB (`MAX_PACK_BYTES`) and compaction keeps packs at or under 512 MiB, below the 5 GiB single-PUT
  limit, so the default is safe; a large loose object is not cut (chunking applies to files). Multipart parts are 8 MiB minimum
  5 MiB; part count limit 10,000 is handled by `multipart_part_size`. `CompleteMultipartUpload` with `If-None-Match` requires
  re-initiating on 409 (AWS), which the code maps to `Ok(false)` ("exists"); that conflates a lost race with a delete race.
  Abandoned multipart uploads incur storage cost: add `AbortIncompleteMultipartUpload` lifecycle (docs now say nothing).
* **Consistency.** S3 has been strongly consistent for reads, lists and overwrites since Dec 2020, which the design relies on
  (`s3.md` lists "strong read-after-write for new keys"; the `connect` probe checks conditional writes only). `list(packs/)` is
  the pack catalogue; it is a full paginated list on each fetch/refresh, 1000 keys per page, capped at 10M keys, so a
  repository with 100k packs pays 100 LIST calls per cold fetch. Compaction keeps pack counts low (tiered), but there is no
  alert on pack count.
* **Cost per op** (us-east-1 list prices, rounded; verify current pricing): PUT/COPY/LIST $0.005 per 1,000, GET $0.0004 per
  1,000, DELETE free, egress $0.09/GB to the internet. A packed publish is 4 PUTs (pack, idx, tx, manifest) = $0.00002; 1M
  publishes is $20. A clone is a manifest GET, a LIST and a few ranged GETs: about $0.00001 plus egress. Loose-object publishes
  (legacy layout, 1 PUT per object) are the expensive path: 1M files = $5 per import. At 100k clients cost is dominated by
  egress and manifest bytes, not requests; GC `touch` is a CopyObject per kept object per sweep (a PUT-class charge).
  Standard bucket versioning of a frequently rewritten `manifest` multiplies stored versions (one per publish); expire
  noncurrent versions.

## 5. TLS and auth defaults

* Listen default `127.0.0.1:8480` (good). Plain HTTP unless built with `--features tls` and given `--tls-cert/--tls-key`;
  a non-loopback plain-HTTP bind logs `tls=no (use a TLS proxy beyond loopback)` but does not refuse. The Docker image listens
  on 0.0.0.0 and (before this review) had no TLS support at all. Image now builds with TLS.
* Tokens: BLAKE3 of a random token, constant time over all entries, expiry optional, polled reload every second plus SIGHUP.
  Unsalted fast hash is acceptable only for high-entropy tokens, which the docs require. No token-creation/expiry default; no
  `Cache-Control`/HSTS concerns (API only). mTLS, CRL, subject map exist; no OCSP, ACME or cipher knob.
* `--rate-limit`, `--auth-fail-limit`, `--max-conns-per-ip` are all off by default; behind a proxy per-IP is meaningless
  without PROXY protocol or `X-Forwarded-For` support (not implemented).
* Fixed: the example tokens (public, incl. admin `example-root`) are refused at start unless
  `VCS_SERVER_ALLOW_EXAMPLE_TOKENS` is set. Metrics are behind a token or admin, good.
* S3 guard policy: `NotPrincipal`+`Deny` replaced by `Principal:"*"`+`ArnNotEquals aws:PrincipalArn` (docs).

## 6. Backup, restore, disaster recovery

State is: immutable `objects/`, `packs/`, `tx/`, plus the mutable `manifest` (and `gc/*`, locks, shelves inside it or beside it).
Good: immutability makes backups cheap and consistent (copy all, the manifest last). Gaps:

* Docs say only "enable versioning". Restoring an older manifest is safe only if every object it references still exists; remote
  GC deletes unreachable objects, so a restore point older than the last sweep may be dangling. Versioning keeps bytes of
  deleted objects (delete markers) but nothing in the tool does the undelete. There is no `verify`/`restore` command, no
  restore drill, no documented RPO/RTO. `vcs publish --mirror` exists (async, whole repo) and is the only built-in DR.
* Directory backend: "stop writes or snapshot the filesystem" — a crash-consistent snapshot is fine because publishes order
  data before the manifest CAS (read: docs/remote-packs.md); not exercised as a restore test.
* The audit log is a single local file (`one process per log file`), truncation of the newest records is undetectable, and it is
  not replicated: loss of the host loses the attribution history (the `/v1/audit` endpoint is the intended second sink).
Done (#570): `vcs remote verify | backup | restore | safe-point`, GC safe point and backup hold, drill and cross-region recipe in
[../backup-restore.md](../backup-restore.md). Still open: audit-log replication.

## 7. Observability

Good: `/healthz`, `/readyz` (cached 2 s, breaker aware), Prometheus metrics with request/duration/bytes/auth/rate/breaker,
JSON logs with request ids, sample alerts and dashboard, graceful SIGTERM drain, `Type=notify`. Missing for the failure modes
above: backend request counts and status classes per S3 operation (so Slow Down and 409 are visible), manifest CAS
attempts/losses and group-commit merges (counters exist internally: `batches`, `merged_writes`), in-flight request-body bytes
(`inflight_body`) and refused-connection count, thread count, pack count / `list` latency, credential age/expiry, cache hit
ratio, `vcs_server_backend_errors_total{kind=other}` alert (403 from expired credentials lands there and is excluded from the
breaker). No tracing. Metrics reset on restart (documented).

## 8. Deploy scripts

There are no scripts, only a unit, a Dockerfile and examples (nothing to be idempotent). Review:

* Docker: runs as uid 10001, `HEALTHCHECK` on `/readyz`, `.dockerignore`, `--locked`. Gaps: builder and base images float (`rust:1`,
  `debian:bookworm-slim`, no digest), no `--read-only`/cap-drop guidance, no SBOM or signing, no image published by CI, runs
  as root during build (fine). Fixed: TLS feature.
* systemd: `EnvironmentFile=-/etc/vcs/server.env` (secrets in a 0600 file, not in the unit: good). Added `LimitNOFILE`,
  `TasksMax`, `CapabilityBoundingSet=`, `RestrictSUIDSGID`, `UMask=0077` etc. Not validated with `systemd-analyze` here (no systemd on
  this Mac); the earlier "Verified" section covers the old unit. `ReadWritePaths=/var/lib/vcs` does not cover `--cache-dir` or
  `--audit-log` outside it; documented operators must add paths.
* Secrets handling: `--metrics-token @file`, `--upstream-token @file`, S3 keys via env: good; but tokens passed as plain argv
  (`--metrics-token <token>`) would show in `ps`; the unit uses the `@file` form.
* IAM: no policy was shipped; least-privilege policy now in `deploy.md`.

## 9. CI and supply chain

Read: `ci.yml` (fmt, clippy, test on Linux+macOS, informational MinIO job, advisory Windows compile, `ci-ok` gate), `release.yml`,
`audit.yml`, `dependabot.yml` (cargo + actions weekly), `deny.toml` (advisories, licenses, bans, sources; unknown registry/git denied: good).

Gaps and what was done:

* **Release matrix used `macos-13`**, retired by GitHub in December 2025; that leg would never run. Switched to `macos-15-intel`
  (not run: needs a tag push).
* Release builds only `vcs` (CLI); `vcs-server` is not shipped and no image is published. Release is not reproducible-by-claim:
  floating `stable` toolchain, no `SOURCE_DATE_EPOCH`/`--remap-path-prefix`, no provenance attestation (`actions/attest-build-provenance`),
  no SBOM, `SHA256SUMS` is unsigned and produced by the same workflow. Filed (#571; now addressed: server tarballs, CycloneDX SBOMs, build-provenance attestations, cosign-signed `SHA256SUMS`, ghcr image, pinned release toolchain; see docs/deploy.md "Verify a release").
* Actions pinned by tag (`@v4`, `@v2.7.8`) not commit SHA; `dtolnay/rust-toolchain@stable`; `go install minio@latest` unpinned.
  Dependabot covers actions but cannot update unpinned `@stable`/`@latest`.
* Tests run without `--locked`. Fuzz: nothing in CI (19 targets, only manual campaigns). Sanitizers: none (ASan only inside
  libFuzzer runs; Miri, TSan, loom not used though the server is heavily threaded). MSRV 1.88 is declared (README, `rust-version`)
  but no job checks it. Platforms: Linux x86_64 and macOS arm64 tested; Linux aarch64, musl, Windows execute nowhere.
* `audit.yml` had `continue-on-error: true` always, so a scheduled run that finds a RUSTSEC advisory still showed green.
  Now advisory only on pull requests.
* Added: weekly `fuzz.yml` smoke (90 s per target, uploads crash inputs), advisory `msrv` job (1.88, `--locked`, also
  `vcs-server --features tls`). Neither has run in CI yet; MSRV could fail on the first run (README says 1.88 and 1.89 were checked).
* `cargo-deny` advisories already caught 2026 zstd advisories (commit 8733030), so the process works when someone looks; there is no
  required check.
* Stray artifact: the shared working tree has an untracked file named `<redacted-host>` at the repo root (looks like a
  mistyped scp target); not committed, worth deleting.

## 10. vcs-watchd

Local file-watcher for a developer machine (inotify/FSEvents), journals changes under `.vcs`, rotates the journal at 32 MiB,
rebuilds watches on overflow, stops when idle. Not a server component; failure mode is falling back to scanning. One watch per
directory on Linux means `fs.inotify.max_user_watches` (8192 on older kernels) on large trees; the code rebuilds on error but
there is no doc entry for the sysctl (not measured).

## Fixes made in this review

* `crates/vcs-server/src/server.rs`: non-blocking refusal (3000 refused sockets 13.4 s to 0.4 s, measured).
* `crates/vcs-server/Cargo.toml`: `tls` enables `vcs-store-s3/tls`; verified a TLS build now proceeds to connect to an
  `https://` S3 endpoint (before: refused at startup).
* `crates/vcs-server/src/main.rs`: refuse the public example tokens.
* `deploy/Dockerfile`, `deploy/vcs-server.service`, `docs/deploy.md`, `.github/workflows/{ci,release,audit,fuzz}.yml`.
* Tests: `cargo test -p vcs-server --test hardening` passes (10/10). The full workspace suite was not run (loaded laptop).

## Sources

* AWS, Performance design patterns for Amazon S3: https://docs.aws.amazon.com/AmazonS3/latest/userguide/optimizing-performance-design-patterns.html
* AWS re:Post, HTTP 500/503 from S3 (3,500 / 5,500 per prefix, Slow Down): https://repost.aws/knowledge-center/http-5xx-errors-s3
* AWS, Conditional writes (412, 409, 404 semantics; multipart restart): https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html
* AWS Storage Blog, multi-writer applications with conditional writes: https://aws.amazon.com/blogs/storage/building-multi-writer-applications-on-amazon-s3-using-native-controls/
* AWS IAM, NotPrincipal (use `aws:PrincipalArn` with `ArnNotEquals` instead): https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_policies_elements_notprincipal.html
* Project data: `docs/research/real-s3.md`, `docs/research/many-writers-s3.md`, `docs/scalability.md`, `docs/known-limits.md`, `docs/v1.md`.
* GitHub changelog, macOS 13 runner image closing down (retired 2025-12-04; `macos-15-intel` is the Intel replacement): https://github.blog/changelog/2025-09-19-github-actions-macos-13-runner-image-is-closing-down/
