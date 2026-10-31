# vcs-server: validating writer (HTTP)

`crates/vcs-server` implements the server from [research/server.md](research/server.md) and the enforcement
half of [locks.md](locks.md). It fronts a `Backend` (a local directory via `FsBackend` in the shipped binary;
any `Backend` through the library) and exposes the backend operations over HTTP/1.1, with bearer-token
authentication, roles, a per-zone write policy and server-side validation of every manifest update.

```
vcs-server hash-token <token>                      # prints the hash to put in the tokens file
vcs-server --root <dir> --tokens <file> [--listen 127.0.0.1:8480]
VCS_TOKEN=<token> VCS_PRINCIPAL=<name> vcs clone http://host:port ...
```

## Security caveat: no TLS

**The server speaks plain HTTP.** Bearer tokens and all data cross the wire in clear text. Run it on loopback or a
private network, and put a TLS-terminating proxy in front for anything else. `HttpBackend` accepts `http://` only.

## Tokens file

One line per token: `<principal> <read|write|admin> <zones|*> <blake3-hex-of-token>`; `#` comments. Only hashes are
stored. Roles: `read` (get/range/exists/list), `write` (plus puts; manifest changes only in the listed zones), `admin`
(all zones, and may break other principals' locks). The file is read at start; restart to change it.

## API

`GET /healthz` (no auth), `GET /v1/whoami`, `GET|HEAD /v1/o/<key>[?offset=&len=]`, `PUT /v1/o/<key>` with
`If-None-Match: *` or `If-Match: "<tag>"` (unconditional PUT is refused, 428), `GET /v1/list?prefix=`.
Status mapping: 401 bad/missing token, 403 role or zone policy, 409 `LockedBy`, 412 precondition failed, 422 invalid
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

## Not done (known gaps)

TLS; token rotation without restart; audit records, rate limits, require-lock policy (#127, #128); checking that
`objects/` uploads hash to their key; lease expiry times are still computed by the client (the server bounds only
`time_hw`); one connection per request, one thread per connection, no concurrency cap; OIDC; object-level read ACLs.
