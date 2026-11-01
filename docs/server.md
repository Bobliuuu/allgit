# vcs-server: validating writer (HTTP)

`crates/vcs-server` implements the server from [research/server.md](research/server.md) and the enforcement
half of [locks.md](locks.md). It fronts a `Backend` (a local directory via `FsBackend` in the shipped binary;
any `Backend` through the library) and exposes the backend operations over HTTP/1.1, with bearer-token
authentication, roles, a per-zone write policy and server-side validation of every manifest update.

```
vcs-server hash-token <token>                      # prints the hash to put in the tokens file
vcs-server --root <dir> --tokens <file> [--listen 127.0.0.1:8480] [--tls-cert <pem> --tls-key <pem>]
                  [--audit-log <file> [--audit-key <file>]] [--rate-limit <burst>,<per-sec>] [--max-connections <n>]
vcs-server audit verify <file> [--key <keyfile>]
vcs-server --backend 's3://bucket/prefix?endpoint=URL' --tokens <file> [--policy <file>] [--max-lease-secs N] [--skew-epsilon-ms N]
VCS_TOKEN=<token> VCS_PRINCIPAL=<name> vcs clone http://host:port ...
```

## TLS

Build with `--features tls` (rustls, ring provider) and pass `--tls-cert <pem> --tls-key <pem>` to serve HTTPS.
Without the feature the server speaks **plain HTTP**: bearer tokens and data cross the wire in clear text, so use
loopback, a private network or a TLS-terminating proxy. `HttpBackend::new("https://...")` works only in a `tls`
build; `HttpBackend::with_ca(url, token, Some(ca_pem))` trusts exactly the given CA bundle, otherwise the built-in web
roots are used. Certificate and hostname verification are always on; there is no insecure switch. Tested with a
locally generated self-signed certificate (trusted CA works, other CA fails, wrong hostname fails, plain HTTP to the
TLS port gets no service, a stalled handshake is cut off). Not done: client certificates (mTLS), certificate reload
without restart, TLS support in the `vcs` CLI (`vcs` still only opens `http://` remotes; a flag/env for the CA bundle
and a `tls` feature passthrough are not wired), and the connection-cap refusal on a TLS port just drops the socket.

## Tokens file

One line per token: `<principal> <read|write|admin> <zones|*> <blake3-hex-of-token>`; `#` comments. Only hashes are
stored. Roles: `read` (get/range/exists/list), `write` (plus puts; manifest changes only in the listed zones), `admin`
(all zones, and may break other principals' locks). An optional fifth field `expires=<unix-seconds>` makes a token
stop working at that time (401 "token expired"; judged by the server clock).

**Reload without restart:** the binary polls the tokens file (mtime and size, once a second) and swaps in the new
table when it parses and is non-empty; a broken or empty file is ignored and the old table stays (a message goes to
stderr). Revocation is therefore "remove the line". Library users can call `Server::reload_tokens`. There is no
SIGHUP handler. Requests already in flight finish with the principal they authenticated as.

Token scopes are the role plus the `<zones>` column: a `write` token with `engine,assets` can change the manifest only
for those zones; `*` means every zone. A request outside the token's scope gets 403 and the client prints how to log in
with a different token.

## Client side

`vcs login http://host:port` verifies the token with `/v1/whoami` and saves it through the credential helper
(`credential.helper`: `file` = `~/.config/vcs/credentials` with mode 0600, `keychain` on macOS, `!command`, or a
program path). `VCS_TOKEN` overrides the helper. `vcs logout` forgets it, `vcs auth status` shows who the server thinks
you are. The helper protocol (`vcs credential get|store|erase`) is `key=value` lines (`host`, `token`) on stdin/stdout.
Tokens are never stored in `.vcs/remote` or printed in errors.

## API

`GET /healthz` (no auth), `GET /v1/whoami`, `GET|HEAD /v1/o/<key>[?offset=&len=]`, `PUT /v1/o/<key>` with
`If-None-Match: *` or `If-Match: "<tag>"` (unconditional PUT is refused, 428), `GET /v1/list?prefix=`.
Status mapping: 401 bad/missing/expired token, 429 rate limited (`Retry-After`), 408 request timed out, 413 body too
large, 431 headers too large, 503 too many connections, 403 role or zone policy, 409 `LockedBy`, 412 precondition failed, 422 invalid
manifest, 503 backend unavailable.

## What is validated on a write of the `manifest` key

The client still builds the new manifest with the normal publish protocol; the server re-checks it against the
manifest it replaces, using the **authenticated principal** (not `Transaction.author`):

* every changed branch head or zone definition must be in a zone the principal may write;
* a changed or added lock record must name the principal as holder; changing a live lock of someone else is `LockedBy`,
  removing it needs `admin`;
* `next_token` and `time_hw` may not decrease, and `time_hw` may not run ahead of the server clock by more than 60 s;
* the net changed paths of each changed head (`changed_paths`) must not include a path with a live lock held by
  someone else (`409 LockedBy`). Objects must already be uploaded (missing objects give 422).

Lock-holder names come from the client's publish, so the CLI's `VCS_PRINCIPAL` must equal the server-side principal
name, otherwise lock acquisition is refused as a holder mismatch.

## Hardening

**Object hashes.** A `PUT` to `objects/<2 hex>/<62 hex>` must carry a body whose content id (`id_of_encoded`, the
same function the client uses) equals the id in the key; a mismatch or a malformed `objects/` key is 422 and nothing
is stored. This checks identity only; it does not decode the object or check that its dependencies exist.

**Audit log** (`--audit-log <file>`, optional `--audit-key <keyfile>` with 64 hex characters). One JSON line per
authenticated `PUT` (accepted or rejected: principal, op, key, outcome = HTTP status, timestamp, previous hash, hash).
Reads, unauthenticated and 401 requests are not recorded. Each record's hash covers the previous hash, so editing,
deleting, inserting or reordering records in the middle breaks the chain; `vcs-server audit verify <file> [--key
<keyfile>]` prints the first bad line (exit 1) or the record count and head hash (exit 0). The server verifies an
existing log before appending and refuses to start on a bad one. Honest limits: dropping the newest records is not
detectable from the file alone (store the printed head hash elsewhere); without `--audit-key` anyone who can write
the file can recompute the whole chain; the key only helps if it is kept away from whoever can write the log. The
server opens the file append-only but cannot make it immutable: use `chattr +a`, WORM storage or a second sink. One
process per log file. If the append fails the client gets 503 even though the write may already have been applied
(the request is not rolled back). This is a different shape from the create-only `audit/<id>` objects and
`GET /v1/audit` sketched in #127: it covers PUTs only, has no txid, zone, path summary or policy hash.

**Rate limiting** (`--rate-limit <burst>,<per-sec>`): a token bucket per authenticated principal, applied to every
authenticated request, 429 with `Retry-After` (whole seconds, at least 1) when empty. Off unless configured; there
are no built-in numbers, and none have been measured. State is per process, so N replicas allow N times the rate.
No separate class for lock operations yet, and failed authentication is not limited (nothing is keyed on it).
`HttpBackend` reports 429 as `Unavailable`.

**Limits.** `max_connections` (default 256; extra connections get 503 with `Retry-After` on plain HTTP, are dropped
on TLS), `max_body` (256 MiB, non-manifest), `max_manifest_body` (32 MiB), `max_header_bytes` (32 KiB total request
line and headers, at most 100 headers; 431), `header_timeout` (10 s overall to receive the headers, including the TLS
handshake), `body_timeout` (600 s overall for the body and again for the response), `io_timeout` (30 s per read/write).
The overall deadlines are what stop slowloris-style trickling; per-read timeouts alone would not. Bodies are read
incrementally, so announcing a huge `Content-Length` and stalling costs no memory. Malformed request lines, header
names, duplicate or non-numeric `Content-Length`, NUL bytes and non-UTF-8 headers are 400. Defaults are unmeasured
guesses, not tuned values; the binary exposes only `--max-connections` and `--max-body`.

## Not done (known gaps)

mTLS and certificate reload; SIGHUP; the CLI cannot open `https://` remotes yet; a rate-limit class for lock ops;
`GET /v1/audit`, create-only audit objects and policy hash in audit (#127); the per-zone path-glob policy (#123);
require-lock policy; lease expiry times are still computed by the client (the server bounds only `time_hw`); one
request per connection and one thread per connection (bounded by `max_connections`); OIDC; object-level read ACLs.
`objects/` uploads hash to their key; lease expiry times are still computed by the client (the server bounds only
`time_hw`); one connection per request, one thread per connection, no concurrency cap; OIDC; object-level read ACLs.

Deployment (systemd, Docker, S3 backend, path policy, require-lock, replicas): see [deploy.md](deploy.md).
