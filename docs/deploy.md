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

Rotate by replacing the hash and restarting; the file is read at start.

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
* Reads are by role only (objects are content-addressed, not listable per zone).
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
sudo install -m 0644 deploy/vcs-server.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now vcs-server
curl -s http://127.0.0.1:8480/healthz      # ok
```

For S3 put `AWS_ACCESS_KEY_ID=` / `AWS_SECRET_ACCESS_KEY=` in `/etc/vcs/server.env` (mode 0600) and swap `--root` for
`--backend` in the unit.

## 4b. Docker

```sh
docker build -f deploy/Dockerfile -t vcs-server .
docker run -d --name vcs -p 127.0.0.1:8480:8480 -v vcs-data:/data \
  -v $PWD/tokens:/etc/vcs/tokens:ro vcs-server
```

The image runs as an unprivileged user and listens on 0.0.0.0 inside the container; publish the port on loopback or a
private interface only. (The Dockerfile and unit file were written for this document but not built or started in CI.)

## TLS, audit log, rate limit

The server can terminate TLS itself when built with `cargo build --release -p vcs-server --features tls` and started
with `--tls-cert <pem> --tls-key <pem>`; otherwise put nginx, Caddy or a load balancer in front and keep the server on
loopback (`reverse_proxy 127.0.0.1:8480`). **The shipped `vcs` CLI only opens `http://` remotes today**, so an
`https://` front end is reachable by other HTTP clients but not yet by `vcs remote set`: until client-side HTTPS
lands, run the CLI against a private network or an SSH/WireGuard tunnel to the plain port, and do not tell users the
link is encrypted unless it is. Audit log (`--audit-log`, `--audit-key`) and rate limiting (`--rate-limit
<burst>,<per-sec>`, `--max-connections`) are documented in [server.md](server.md); add them to `ExecStart` as needed.

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
no longer distributed, so any S3-compatible server that honours conditional writes works, e.g.
`pip install 'moto[server]' && moto_server -p 9000` (this is what the maintainers' CI box used; the bucket is created by the test).
