# vcs-server: validating writer (HTTP)

`crates/vcs-server` implements the server from [research/server.md](research/server.md) and the enforcement
half of [locks.md](locks.md). It fronts a `Backend` (a local directory via `FsBackend` in the shipped binary;
any `Backend` through the library) and exposes the backend operations over HTTP/1.1, with bearer-token
authentication, roles, a per-zone write policy and server-side validation of every manifest update.

```
vcs-server hash-token <token>                      # prints the hash to put in the tokens file
vcs-server --root <dir> --tokens <file> [--listen 127.0.0.1:8480] [--tls-cert <pem> --tls-key <pem> [--tls-client-ca <pem>]]
                  [--audit-log <file> [--audit-key <file>]] [--rate-limit <burst>,<per-sec>] [--max-connections <n>]
                  [--max-conns-per-ip <n>] [--auth-fail-limit <burst>,<per-sec>]
vcs-server audit verify <file> [--key <keyfile>]
vcs-server --backend 's3://bucket/prefix?endpoint=URL' --tokens <file> [--policy <file>] [--max-lease-secs N] [--skew-epsilon-ms N]
VCS_TOKEN=<token> VCS_PRINCIPAL=<name> vcs clone http://host:port ...
```

## TLS

Build with `--features tls` (rustls, ring provider) and pass `--tls-cert <pem> --tls-key <pem>` to serve HTTPS.
Without the feature the server speaks **plain HTTP**: bearer tokens and data cross the wire in clear text, so use
loopback, a private network or a TLS-terminating proxy. Certificate and hostname verification are always on on the
client side; there is no insecure switch.

**CLI.** The `vcs` CLI opens `https://host:port` remotes when it is built with `--features tls` (`cargo build -p vcs-cli
--features tls`; this also enables `https://` S3 endpoints). It is **opt-in**: the default build has no TLS dependencies
and builds offline, and the test `s3_https_endpoint_without_tls_feature_names_the_feature` checks that a default build
says which feature is missing. A default build given an `https://` vcs-server remote fails with the same hint. Trust:

* default: the built-in web roots;
* a private CA: `vcs --ca-pem ca.pem <command>` (per invocation), env `VCS_CAINFO`, or the config key `remote.cainfo`
  (`vcs config set remote.cainfo /abs/path/ca.pem`, repo or `--user`). This replaces the web roots with exactly that
  bundle. There is one remote per repository, so the key is `remote.cainfo`, not per remote name. Note that `vcs clone
  --ca-pem` applies only to that invocation: set `remote.cainfo` in the new clone for later commands.
* client certificate (mutual TLS): `remote.sslcert` + `remote.sslkey` (env `VCS_CLIENT_CERT`, `VCS_CLIENT_KEY`); a CA
  bundle must be configured too.

Credentials are keyed by `host:port` regardless of scheme, so a token saved by `vcs login http://h:1` is also offered to
`https://h:1`.

**Server.** `HttpBackend::with_ca(url, token, Some(ca_pem))` / `with_tls(.., Some((cert, key)))` is the library entry
point. Tested with locally generated CAs and leaf certificates (rcgen): trusted CA works, other CA fails, wrong hostname
fails, plain HTTP to the TLS port gets no service, a stalled handshake is cut off, the real `vcs` binary does clone,
publish, fetch, lock/unlock and `auth status` against the real `vcs-server` over HTTPS and fails closed with the wrong CA.

**Mutual TLS (optional).** `--tls-client-ca <pem>` makes the server require a client certificate chaining to that CA;
the handshake is refused otherwise. The certificate only admits the connection: the bearer token is still required and is
what names the principal (no mapping from certificate subject to principal). There is no "certificate or token" mode and
no revocation list: to cut off a client certificate, rotate the client CA or the leaf (and reload, below).

**Reload (unix).** `kill -HUP <pid>` re-reads the tokens file, the policy file and the TLS certificate/key (and client
CA). Each is replaced only if it parses; a broken file keeps the running value and the reason goes to stderr. New TLS
connections use the new certificate, established connections keep theirs. Tested by sending the signal to the real
binary (certificate swap, broken policy ignored, policy widened and narrowed, token revoked). Without a signal the tokens
file is still polled once a second. Windows: no signal handler, restart instead.

Not done: certificate revocation, OCSP, ACME, a TLS version/cipher knob (rustls defaults), and the connection-cap refusal
on a TLS port just drops the socket.

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

`GET /healthz` and `GET /readyz` (no auth, constant bodies; see [deploy.md](deploy.md#operating-health-metrics-logs-shutdown)), `GET /metrics` (admin or metrics token), `GET /v1/whoami`, `GET|HEAD /v1/o/<key>[?offset=&len=]`, `PUT /v1/o/<key>` with
`If-None-Match: *` or `If-Match: "<tag>"` (unconditional PUT is refused, 428), `GET /v1/list?prefix=`.
Status mapping: 401 bad/missing/expired token, 429 rate limited (`Retry-After`), 408 request timed out, 413 body too
large, 431 headers too large, 503 too many connections, 403 role or zone policy, 409 `LockedBy`, 412 precondition failed, 422 invalid
manifest, 503 backend unavailable or circuit breaker open (both carry `Retry-After`). Every response has an `X-Request-Id`.

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

## Read policy (zone-level, partial by design)

Besides roles, `--policy` accepts `allow-read <principal|*> <zone|*> **` and `deny-read <principal|*> <zone|*> **`
(the glob must be `**`; path-level read rules are rejected at load). A non-admin principal with any `allow-read` rule can
read exactly the zones some `allow-read` matches; otherwise it can read every zone except those a `deny-read` names;
`deny-read` wins. Admins are exempt. Read rules never grant writes.

**Shelves** (`shelves/<principal>/...`, docs/shelves.md) are private to their owner and admins. Only the owner (or an admin) can create or
delete them; `shelve-read <reader|*> <owner|*> **` lets another principal read (not write) the owner's shelves. That grant covers
the whole shelf and is not narrowed by `allow-read` / `deny-read`.

**How it is enforced.** Objects are content-addressed and shared, so an id carries no zone. The server walks, per zone,
everything reachable from that zone's branch heads and tags (commits, trees, chunk manifests; plain blobs are leaves,
checked with a 1-byte ranged read) and keeps the id sets in memory, extending them when the manifest changes. For a
restricted principal:

* `GET`/`HEAD`/range of `objects/...` is answered `404` (the same answer as for a missing object, so `HEAD` is no
  existence oracle) when the object is reachable from a zone it may not read and from none it may read. Content shared
  with a readable zone stays readable: the principal could read those bytes anyway.
* Only canonical lowercase `objects/<2 hex>/<62 hex>` keys and `manifest` are readable. Packs (they mix zones), `tx/`,
  `tomb/`, `gc/`, upper-case spellings and any other key are `404`. `GET /v1/list?prefix=packs/` is an empty list; every
  other `list` is `403`, because a listing enumerates object ids of all zones. Percent-encoded separators, `.`/`..`
  segments, double slashes and trailing slashes were tested and do not get around this.

**What cannot be hidden (threat model).**

* The manifest is served to everyone who can read at all: it names every zone, branch, tag, lock, zone prefix and the
  head object ids of the zones a token may not read. Zone existence and head ids are therefore public to any
  authenticated token. Hiding them would need a per-principal filtered manifest, which would break the compare-and-swap
  publish of write tokens; not done.
* Snapshot, transaction and operation objects, and objects not yet reachable from any zone (uploaded but not published),
  are in no zone set and are readable by id. Their ids are not listable by restricted tokens, and a 256-bit id cannot be
  guessed, but a snapshot object read by id names the head commits of all zones it covers.
* Anyone with a write token can probe whether a given content exists by uploading it (`If-None-Match: *` answers `412`
  for an object that is already there, in any zone). That is inherent in deduplicated storage.
* Packs are not readable by restricted tokens. A repository whose objects exist only inside remote packs (no loose
  copies) therefore cannot be cloned by a restricted token.
* Sizes and timing: no padding, no constant-time lookups.
* Cost: the first restricted read after a restart walks the whole history of every zone (one request per commit/tree and
  one 1-byte ranged read per file entry), and the id sets cost 32 bytes per reachable object of RAM. The sets only grow
  while the process lives, so an object removed from a zone's history stays attributed to that zone until restart. Not
  measured at scale.
* This is zone-level access control for a trusted server operator model. It is not a confidentiality boundary against
  someone who already holds the denied content or the denied ids.

Tested (`crates/vcs-server/tests/readpolicy.rs`, `sighup.rs`): own-zone reads, denied get/head/range, shared content,
new history published after the index was built, policy reload, admin exemption, and adversarial requests (encoded
separators, upper-case ids, `..`, trailing slash, `packs/`, `tx/`, list prefixes, half ranges, PUT/DELETE).

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

**Forced updates and deletions.** A publish that replaces a branch head with a commit that does not descend from
it, or removes a branch, is allowed (it is always a compare-and-swap on the old head, so a stale writer fails) but the
server records it on its own audit line: op `FORCE` (key `zone/branch <old full id> -> <new full id>`) or
`DELETE-BRANCH`, before the generic `PUT manifest` line. `FORCE?` means the ancestry walk hit its bound (100000
commits) and descent could not be established. The old head also stays a garbage-collection root while the
transaction is in the manifest's applied window. The policy rule `protect-branch <zone|*> <branch-glob>` refuses
both (403, admins included); `require-signed` and locks are checked on the forced head exactly as on any other.

**Rate limiting** (`--rate-limit <burst>,<per-sec>`): a token bucket per authenticated principal, applied to every
authenticated request, 429 with `Retry-After` (whole seconds, at least 1) when empty. Off unless configured; there
are no built-in numbers, and none have been measured. State is per process, so N replicas allow N times the rate.
No separate class for lock operations yet, and failed authentication is not limited (nothing is keyed on it).
`HttpBackend` reports 429 as `Unavailable`.

**Limits.** `max_connections` (default 256; extra connections get 503 with `Retry-After` on plain HTTP, are dropped
on TLS), `max_body` (1 GiB, non-manifest), `max_manifest_body` (32 MiB), `max_header_bytes` (32 KiB total request
line and headers, at most 100 headers; 431), `header_timeout` (10 s overall to receive the headers, including the TLS
handshake), `body_timeout` (600 s overall for the body and again for the response), `io_timeout` (30 s per read/write).
The overall deadlines are what stop slowloris-style trickling; per-read timeouts alone would not. Bodies are read
incrementally, so announcing a huge `Content-Length` and stalling costs no memory. Malformed request lines, header
names, duplicate or non-numeric `Content-Length`, NUL bytes and non-UTF-8 headers are 400. Defaults are unmeasured
guesses, not tuned values; the binary exposes `--max-connections`, `--max-body`, `--max-conns-per-ip` and
`--auth-fail-limit`.

**Abuse limits (opt-in, security review finding 14).** `--auth-fail-limit <burst>,<per-sec>` keeps a leaky counter
per client address: every failed authentication (401) adds one, it drains at `per-sec`, and at `burst` the address
gets 429 with `Retry-After` before its token is even looked at (so guessing cannot continue, and a valid token from a
blocked address is also refused until the counter drains). `/healthz` and `/readyz` are exempt. `--max-conns-per-ip <n>`
refuses (503, or a drop under TLS) connections beyond `n` from one address. Both are off by default because behind a
reverse proxy every client shares the proxy's address: enable them only where the server sees real client addresses.
Idle cost is one relaxed atomic load per request when no failure has been recorded.

## Not done (known gaps)

Certificate revocation and subject-to-principal mapping for mTLS; path-level read rules (impossible on content-addressed
objects, see Read policy); a filtered manifest per principal; a rate-limit class for lock ops; `GET /v1/audit`,
create-only audit objects and policy hash in audit (#127); lease expiry times are still computed by the client (the
server bounds only `time_hw`); one request per connection and one thread per connection (bounded by `max_connections`);
OIDC; Windows signal reload.

Deployment (systemd, Docker, S3 backend, path policy, require-lock, replicas): see [deploy.md](deploy.md).
