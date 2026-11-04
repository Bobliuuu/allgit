# Deploying vcs-server

Self-contained recipe for running `vcs-server` for a team. Nothing here touches a cloud account: you bring a Linux
host (or container runtime) and, optionally, an S3-compatible bucket. Files: [`deploy/Dockerfile`](../deploy/Dockerfile),
[`deploy/vcs-server.service`](../deploy/vcs-server.service), [`deploy/tokens.example`](../deploy/tokens.example),
[`deploy/policy.example`](../deploy/policy.example). Protocol and validation details are in [server.md](server.md).

**By default the server speaks plain HTTP.** Tokens travel in clear text. Bind to loopback or a private network, or build with
`--features tls` and pass `--tls-cert/--tls-key` (see "TLS" below). Do not expose a plain-HTTP port to the internet.

## 1. Choose a backend

| Flag | Where the data lives | Notes |
| --- | --- | --- |
| `--root /var/lib/vcs/data` (or `--backend /path`) | local directory (`FsBackend`) | one server process per directory; fine for a single host |
| `--backend 's3://bucket/prefix?endpoint=URL&region=R'` | S3-compatible bucket | refuses at start-up if the store ignores conditional writes (`If-None-Match`, `If-Match`); credentials only from `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` (+ `AWS_SESSION_TOKEN`), never the URL |

With S3 you may run several server processes against one bucket; set `--skew-epsilon-ms` (below).

### S3 with a sole manifest writer

The manifest is the only mutable object and the commit point of every publish; its integrity rests on a compare-and-swap
that only a validating writer should perform. With `vcs-server` in front of the bucket, give **clients no bucket access at
all** (they speak HTTP to the server). If you also hand direct `s3://` credentials to some clients (read-mostly CI, say),
they must not be able to write `<prefix>manifest` by any route: single PUT, multipart, or delete.

Bucket policy (no secrets; replace `BUCKET`, `PREFIX` and the server role ARN):

```json
{"Version":"2012-10-17","Statement":[{
  "Sid":"OnlyTheServerWritesTheManifest","Effect":"Deny",
  "NotPrincipal":{"AWS":"arn:aws:iam::ACCOUNT:role/vcs-server"},
  "Action":["s3:PutObject","s3:DeleteObject","s3:AbortMultipartUpload"],
  "Resource":["arn:aws:s3:::BUCKET/PREFIX/manifest"]}]}
```

(`NotPrincipal` with `Deny` is AWS syntax; MinIO/RustFS-style stores that lack it can use `Principal:"*"` and a separate
allow for the server's credential, or an IAM policy on the client user. Verify with the self-check below, not by reading the policy.)

**Startup self-check.** Give the server the *client* credential in `VCS_CLIENT_ACCESS_KEY_ID` /
`VCS_CLIENT_SECRET_ACCESS_KEY` (optional `VCS_CLIENT_SESSION_TOKEN`) and add `--check-client-cannot-write-manifest`.
Before serving, the server uses that credential to attempt a PUT guarded by an `If-Match` ETag that cannot exist (an authorised
writer gets 412/404 and nothing is written) and a CreateMultipartUpload on the manifest key (aborted at once if accepted).
Anything but 401/403 on either route means the credential can write the manifest and the server exits with status 2. An
unexpected status (missing bucket, 5xx) is inconclusive and also refuses to start. The probe never changes the manifest.

Tested: `crates/vcs-store-s3/tests/manifest_denied.rs` (fake-server cases for denied / allowed / inconclusive, and
`bucket_policy_denies_manifest_writes_on_a_real_s3_server` against a real S3-compatible server: allowed without the policy,
denied with it, other keys still writable) and `crates/vcs-cli/tests/server_selfcheck.rs` (the binary exits 2 without the
policy, starts with it). Run so far on RustFS 1.0.4 only (single node, root credential with the bucket policy as the deny);
not on AWS S3, MinIO or R2, whose policy evaluation for the account owner differs. Re-run them with `VCS_TEST_S3_ENDPOINT`
against your provider before relying on the policy.

## 2. Tokens

```sh
token=$(head -c 32 /dev/urandom | base64 | tr -d '=+/')
vcs-server hash-token "$token"      # put this hash in the tokens file
```

Format (see `deploy/tokens.example`): `<principal> <read|write|admin> <zones|*> <blake3-hex>`. Hand each user their token
out of band. Users set:

```sh
export VCS_TOKEN=<token>            # sent as `Authorization: Bearer`; never stored in .vcs/
export VCS_PRINCIPAL=<principal>    # must equal the server-side principal name (lock holder identity)
vcs remote set http://vcs.internal:8480
```

Rotate by replacing the hash: the file is polled once a second, and `kill -HUP <pid>` reloads tokens, policy and the TLS
certificate at once (a file that fails to parse is ignored and the old value stays).

## 3. Path policy and locks

`--policy <file>` loads per-zone path-glob rules (`deploy/policy.example`), **deny by default**:

```
allow alice  *    **            # principal zone glob
deny  *      *    secrets/**    # deny beats allow
require-lock * art **/*.psd     # change only while holding a lock on that exact path
```

* Without `--policy` the only zone restriction is the zone list in the tokens file.
* With a policy, a write principal changes a path only if an `allow` matches and no `deny` does; it also needs an
  `allow` rule for a zone to move its branches or define it. Admins skip allow/deny but not `require-lock`.
* Reads are by role unless you add read rules: `allow-read rita art **` / `deny-read * secret **` limit a non-admin token
  to whole zones. This hides object bodies of other zones, not the manifest or object ids; read the threat model in
  [server.md](server.md#read-policy-zone-level-partial-by-design) before relying on it. Path-level read rules do not exist.
* Refusals: 403 (policy/role), 428 `LockRequired`, 409 `LockedBy`, 422 invalid (e.g. lease over the cap).

**Lease expiry is server-computed.** The client's `expires_at` only says how long a lease it wants
(`expires_at - time_hw`); the server restarts it at its own clock, stores `acquired_at`/`expires_at`/`time_hw` from that
clock, and refuses leases over `--max-lease-secs` (default 7 days). A client clock more than 60 s ahead is refused (422).

**Replicas and skew.** With several servers on one bucket, clocks differ. A lock stays live until
`expires_at + --skew-epsilon-ms`, so a fast replica cannot hand a lock to someone else before the holder has had the full
lease. Use at least twice the largest clock offset you expect between replicas (run NTP/chrony; 1000-2000 ms is typical).
The trade-off is that an expired lock blocks others for up to epsilon longer. The default is 0 (single server).
The model checker (`crates/vcs-remote/tests/model_locks.rs`) covers this with replica clock offsets and mutation checks.

## 4a. systemd

```sh
sudo useradd --system --home-dir /var/lib/vcs --shell /usr/sbin/nologin vcs
sudo install -m 0755 target/release/vcs-server /usr/local/bin/vcs-server
sudo install -d -o vcs -g vcs /var/lib/vcs/data
sudo install -d /etc/vcs && sudo install -m 0640 -g vcs deploy/tokens.example /etc/vcs/tokens   # then edit
sudo install -m 0644 deploy/policy.example /etc/vcs/policy                                       # then edit
head -c 24 /dev/urandom | base64 | tr -d '=+/' | sudo install -m 0640 -g vcs /dev/stdin /etc/vcs/metrics-token   # >= 16 chars; the unit reads it
sudo install -m 0644 deploy/vcs-server.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now vcs-server
curl -s http://127.0.0.1:8480/readyz       # ready
```

The unit needs `/bin/kill` for `ExecReload` (package `procps`; present on any normal host, missing from minimal container images) and
the `/etc/vcs/metrics-token` file (the server refuses to start with a token under 16 characters; drop `--metrics-token` from
`ExecStart` if you do not scrape metrics). Both were found by running it, see "Verified" below.

For S3 put `AWS_ACCESS_KEY_ID=` / `AWS_SECRET_ACCESS_KEY=` in `/etc/vcs/server.env` (mode 0600) and swap `--root` for
`--backend` in the unit.

## 4b. Docker

```sh
docker build -f deploy/Dockerfile -t vcs-server .
docker run -d --name vcs -p 127.0.0.1:8480:8480 -v vcs-data:/data \
  -v $PWD/tokens:/etc/vcs/tokens:ro vcs-server
```

The image runs as an unprivileged user (uid 10001) and listens on 0.0.0.0 inside the container; publish the port on loopback or a
private interface only. `.dockerignore` keeps `target*`, `.git`, `fuzz` and `docs/img` out of the build context.

## Verified: Docker image, systemd unit, MinIO (issue #216)

Run on a Mac with Docker Desktop 28.4 (linux/arm64), nothing outside the machine. Reproduce with the commands below; the tests
that take part are `crates/vcs-cli/tests/server_e2e.rs::cli_workflow_against_an_external_server` (the full CLI workflow: init, publish,
clone, locks, a refused publish, fetch, sync, 401/403 checks, token never in `.vcs/`) and
`crates/vcs-server/tests/external.rs` (`conformance` and `conformance_gc` over HTTP). Both are skipped unless
`VCS_TEST_SERVER_URL` points at a running server whose tokens file lists `alice` and `bob` (write), `rita` (read) and `root`
(admin) with the tokens `tok-alice`, `tok-bob`, `tok-rita`, `tok-root` (`vcs-server hash-token tok-alice` gives the hash).

| Check | Result |
|---|---|
| `docker build -f deploy/Dockerfile .` | builds from a clean cache; image 140 MB (debian bookworm-slim + one 2.5 MB static-ish binary) |
| container, directory backend (`--root /data`, volume) | runs as uid 10001; `HEALTHCHECK` goes `healthy` (it uses `/readyz`); e2e workflow, `conformance`, `conformance_gc`: pass |
| container, S3 backend against MinIO (`--backend 's3://vcs-test/PREFIX?endpoint=http://host.docker.internal:9000'`, keys in `AWS_*`) | starts (the conditional-write probe passes); e2e workflow and `conformance_gc`: pass; `conformance`: pass after the fix below |
| HTTPS in the CLI client (default build, `tls` feature) | `cargo test -p vcs-cli --test server_tls`: clone, publish, fetch, lock over `https://` against `vcs-server --tls-cert --tls-key` with a generated CA, wrong CA refused, certificate reload on SIGHUP, mutual TLS: 3 of 3 pass |
| systemd unit on a Debian 12 host with systemd as PID 1 (a privileged container, binary taken from the image) | `systemd-analyze verify` clean; `Type=notify` reaches `active` (READY=1 received); main process runs as `vcs`; `/readyz` 200; e2e and conformance pass through it; `systemctl reload` keeps it active; `systemctl stop` logs `draining`, `shutdown complete` and ends `Result=success`, exit status 0, in 0.15 s |

Problems found by doing this and fixed here: **a server on an S3 backend never answered a conditional GET with 304** (an S3 ETag keeps its
quotes, the comparison did not; every conditional read sent the whole body, found when `conformance` ran against the image on MinIO); the unit's `ExecReload=/bin/kill` fails on a host without `procps`; the unit's
`--metrics-token @/etc/vcs/metrics-token` file was never created by the instructions (the server then exits 2 with
`--metrics-token must be at least 16 characters`); no `.dockerignore` (the build context copied a checked-in `target-main/`).
Not tested: a real VM boot, SELinux/AppArmor profiles, `--features tls` in the image (the Dockerfile builds the default
plain-HTTP server), a bare-metal systemd's cgroup limits (the sandbox directives `ProtectSystem=strict`, `SystemCallFilter`,
`MemoryDenyWriteExecute` did apply inside the container host and the server ran under them).

```sh
docker build -f deploy/Dockerfile -t vcs-server .
# tokens: alice/bob write, rita read, root admin, tokens tok-<name>
for n in alice bob rita root; do echo "$n $( [ $n = rita ] && echo read || { [ $n = root ] && echo admin || echo write; } ) * $(docker run --rm vcs-server hash-token tok-$n)"; done > /tmp/tokens
docker run -d --name vcs-fs -p 127.0.0.1:8480:8480 -v /tmp/tokens:/etc/vcs/tokens:ro vcs-server
VCS_TEST_SERVER_URL=http://127.0.0.1:8480 cargo test -p vcs-cli --test server_e2e external   # empty remote first
VCS_TEST_SERVER_URL=http://127.0.0.1:8480 cargo test -p vcs-server --test external
```

### MinIO (the #232 leftover) and what it showed

MinIO no longer publishes `minio/minio` on Docker Hub or Quay. The run used `bitnamilegacy/minio:latest` (MinIO
`DEVELOPMENT.2025-05-24T17-08-30Z`, root credential `minioadmin`) with `VCS_TEST_S3_ENDPOINT=http://127.0.0.1:9000`:

* `-p vcs-store-s3 --test gc_s3`: `s3_conformance_gc` and the sweeper scenario pass (the #232 item).
* `-p vcs-store-s3 --test s3`: 17 of 17 pass after two fixes. (1) **This MinIO creates a missing key on `PUT If-Match: <anything>`**
  (AWS answers 404), which broke `conformance` ("CAS on missing key fails"). `S3Backend::connect` now probes for it and, only for
  such a provider, `put_if_match` looks the key up first. That is not atomic against a concurrent create of the same key, which is
  harmless for the manifest (it is never deleted and recreated); stale-ETag CAS on an existing key is atomic and was always right.
  (2) Two assertions in `minio_publish_fetch_and_sparse_request_counts` had gone stale (full fetch makes fewer requests than a
  sparse one now, and test blobs compress); they compare bytes only.
* `-p vcs-store-s3 --test manifest_denied`: `bucket_policy_denies_manifest_writes_on_a_real_s3_server` **fails on MinIO**: the root
  credential is not subject to bucket policy, so the probe correctly reports `Allowed`. The self-check did its job; to test the
  policy on MinIO give the server a non-root user. Still verified on RustFS 1.0.4 only.
* `-p vcs-cli --test server_e2e` with the S3 variable set: directory, S3 and external-server workflows pass.

## TLS, audit log, rate limit

The server can terminate TLS itself when built with `cargo build --release -p vcs-server --features tls` and started
with `--tls-cert <pem> --tls-key <pem>`; otherwise put nginx, Caddy or a load balancer in front and keep the server on
loopback (`reverse_proxy 127.0.0.1:8480`). The `vcs` CLI opens `https://` remotes in the default build
(`tls` is on by default; `--no-default-features --features watcher` gives a plain-HTTP-only build that says so):

```sh
vcs config set --user remote.cainfo /etc/ssl/private-ca.pem   # only for a private CA; omit for a public certificate
vcs remote set https://vcs.example.com:8480
```

Certificates are always verified. After renewing the certificate files run `systemctl reload vcs-server` (add
`ExecReload=/bin/kill -HUP $MAINPID` to the unit) or `kill -HUP`; no restart, no dropped connections. For mutual TLS add
`--tls-client-ca <pem>` on the server and set `remote.sslcert`/`remote.sslkey` on clients; the token is still required.
Do not tell users the link is encrypted unless it is: a CLI built with `--no-default-features` refuses an `https://` remote (it says so) instead of silently using plain HTTP.
Audit log (`--audit-log`, `--audit-key`) and rate limiting (`--rate-limit <burst>,<per-sec>`, `--max-connections`) are
documented in [server.md](server.md); add them to `ExecStart` as needed.

## Operating: health, metrics, logs, shutdown

**Config file.** `vcs-server --config /etc/vcs/server.toml` reads flat `key = value` lines (strings and numbers; keys are
the long flag names, see `deploy/server.toml.example`). Flags override the file. A bad file stops startup with
`server.toml:LINE: message` and exit status 2; unknown keys, duplicates, tables and arrays are errors.

**Probes** (no token, constant bodies, nothing about the backend leaks):
`GET /healthz` is liveness (always 200 while the process answers). `GET /readyz` is 200 `ready` when a HEAD of the manifest
key succeeds on the backend (an absent manifest counts as reachable), else 503 `not ready`; the result is cached for 2 s, so
probes cannot load the backend, and an open circuit breaker reports not ready without calling it. The Docker image's
`HEALTHCHECK` uses `/readyz`.

**Metrics.** `GET /metrics` returns Prometheus text (version 0.0.4) for an admin token, or for the token given with
`--metrics-token <token|@file>` (at least 16 characters; it can read nothing else, other roles get 403). Families:
`vcs_server_requests_total{method,status,role}`, `vcs_server_request_duration_seconds` (histogram),
`vcs_server_request_bytes_total`, `vcs_server_response_bytes_total`, `vcs_server_auth_failures_total`,
`vcs_server_rate_limited_total`, `vcs_server_lock_conflicts_total`, `vcs_server_audit_appends_total`,
`vcs_server_audit_errors_total`, `vcs_server_backend_errors_total{kind=unavailable|other|timeout|circuit_open}`,
`vcs_server_backend_retries_total`, `vcs_server_backend_breaker_trips_total`, `vcs_server_backend_circuit_open`,
`vcs_server_open_connections`, `vcs_server_uptime_seconds`, `vcs_server_build_info{version,tls}`. Counters are per process
and reset on restart (use `rate()`/`increase()`). `deploy/prometheus.yml`, `deploy/vcs-server.rules.yml` (alerts) and
`deploy/grafana-dashboard.json` are samples; the alert thresholds are starting points, not tuned values.

**Logs.** One line per event on stderr: `--log-format json|text` (default text), `--log-level error|warn|info|debug`
(default info; else `VCS_SERVER_LOG` or `RUST_LOG`, e.g. `warn,vcs_server=debug`). Each request produces one `request` line
with `request_id`, method, path (no query string), status, principal name, role, `dur_ms`, `bytes_in`, `bytes_out`. The same
id is returned as `X-Request-Id`; a client-supplied id of up to 64 characters from `[A-Za-z0-9._-]` is reused. Tokens and the
`Authorization` header are never logged (a test greps captured logs for every token used). Under systemd read them with
`journalctl -u vcs-server -o cat | jq .`.

**Shutdown.** SIGTERM and SIGINT stop accepting (the socket closes), let in-flight requests finish for up to
`--shutdown-grace <secs>` (default 30; fractions allowed), fsync the audit log and exit 0, also when the grace expires with
requests still running (logged as a warning). SIGHUP still reloads tokens, policy and the certificate. With
`Type=notify` (the shipped unit) the server sends `READY=1` after binding and `STOPPING=1` on SIGTERM using
`$NOTIFY_SOCKET`; no libsystemd. Keep `TimeoutStopSec` above the grace.

**Backend resilience.** Transient backend errors (`Unavailable`: S3 5xx, timeouts, I/O) are retried `--backend-retries`
times (default 2) with jittered exponential backoff (50 ms base, 1 s cap). `--backend-timeout <secs>` (default 60, 0 = none)
is the backend budget of one request: no attempt or backoff starts past it. It cannot interrupt a single call that is stuck
inside the backend, which is bounded only by that backend's own I/O timeouts. After `--breaker-threshold` consecutive
failed backend operations (default 5, 0 = off) the breaker opens for `--breaker-cooldown` seconds (default 5): backend routes
answer 503 with `Retry-After` immediately, `/readyz` fails, then one probe call decides whether it closes. Retried writes are
safe because every protocol step is idempotent; the worst case of a write that did apply but whose reply was lost is a
spurious 412, which clients already handle by re-reading. Errors the backend answered deliberately (`Other`) are not retried and do not
count towards the breaker.

## Backups and operations

* Directory backend: stop writes or snapshot the filesystem; everything is under `--root`. The `manifest` file is the
  single linearization point; objects are immutable.
* S3 backend: enable bucket versioning if you want point-in-time recovery of `manifest`.
* The server is stateless between requests; restarting loses nothing.
* `curl -H "Authorization: Bearer $VCS_TOKEN" http://host:8480/v1/whoami` shows how the server sees you.

## Testing against an S3-compatible store

`VCS_TEST_S3_ENDPOINT=http://127.0.0.1:9000 cargo test -p vcs-cli --test server_e2e` runs the CLI workflow against
`vcs-server --backend s3://...` (and `-p vcs-store-s3 --test s3` runs the backend conformance suite). Credentials default
to `minioadmin`/`minioadmin`; override with `VCS_TEST_S3_ACCESS_KEY`/`VCS_TEST_S3_SECRET_KEY`. MinIO's public binaries are
no longer distributed as binaries or as the `minio/minio` image (see "Verified" for a working image), so any S3-compatible server that honours conditional writes works, e.g.
`pip install 'moto[server]' && moto_server -p 9000` (this is what the maintainers' CI box used; the bucket is created by the test).
