# Peering: scaling reads (and routing writes) across sites

How a fleet of `vcs-server`s serves many clients in many places. Short version: **one
authoritative store decides every write; any number of read-through peers in front of it serve the
immutable bytes from local disk.** Scope here is topology and replication. Single-server limits are
in [manifest-scaling.md](manifest-scaling.md) and push/clone throughput of one server in the
benchmark docs; they are not repeated.

## Why this works: what is immutable and what is not

| Key | Mutable? | What a peer may do |
|---|---|---|
| `objects/..`, `packs/pack-<h>.idx`, `packs/pack-<h>.pack` (whole or ranges) | No, content addressed | cache forever, serve without asking upstream |
| `manifest` | Yes (compare-and-swap) | revalidate with `If-None-Match` (a 304 has no body); never decide a CAS |
| `LIST packs/` | Yes | same freshness rule as the manifest |
| locks, shelves, anything else | Yes | pass through |

The rule: *only the manifest (and the listing) needs freshness, and
only the origin decides writes.*

## Topology options that exist

1. **Clients straight to one server** (`vcs-server --root|--backend`). Reads and writes hit one
   process; objects live in a directory or an S3 bucket.
2. **Several servers on one shared store** (same `--backend s3://...`). Stateless servers, the
   store's `If-Match` is the only arbiter. Scales request handling, not store bandwidth.
3. **Server disk cache** (`--cache-dir`, `DiskCacheBackend`): survives restarts, serves warm
   clones from disk, asks the store only for manifest and listing. Same host as the store or not.
4. **Client caches** (`ClientCache`, `MemoBackend`, `IdxCacheBackend`): per-user. Not shared.
5. **Mirrors**: `vcs publish --mirror` copies a repository to a second remote (disaster recovery or
   a read-only secondary, refreshed by the publisher). Asynchronous, whole-repo.
6. **New: read-through peers** (`vcs-server --upstream <url>`): a tier of servers that hold a disk
   cache of the origin and forward everything authoritative. They stack.

```
client --> edge peer --> regional peer --> origin (--root / --backend s3://)
```

## Running a peer

```
vcs-server --upstream https://origin.example --upstream-token @/etc/vcs/peer-token \
           --cache-dir /var/cache/vcs --cache-size 200000000000 \
           --tokens /etc/vcs/tokens --listen 0.0.0.0:8480 \
           [--upstream-manifest-ttl 0] [--upstream-stale-if-error 0] [--upstream-ca ca.pem]
```

* Clients authenticate to the peer with the peer's own `--tokens`; the peer authenticates to its
  upstream with `--upstream-token` (or `VCS_UPSTREAM_TOKEN`; a leading `@` reads a file; never
  printed). The peer's token needs the **write** role upstream, because it forwards publishes.
  Policy, locks and zone scopes are enforced at the peer for its clients and again at the origin for
  the peer's identity, so give peers a policy at least as strict as the origin's. Audit at the
  origin names the peer, not the end user; keep a peer audit log (`--audit-log`) for per-user
  attribution.
* `--upstream` replaces `--root/--backend`; `--cache-dir` is required.
* The origin and every peer speak the same protocol, so a peer can be another peer's upstream.

### Freshness knobs

* `--upstream-manifest-ttl <secs>` (default **0**): with 0, every manifest read and listing is
  revalidated upstream with a conditional GET (no body when unchanged), so a peer is never
  staler than the origin was at the start of the request. With N seconds, a copy confirmed less
  than N seconds ago is served without asking. A publish made at the origin or another peer is then
  visible here within N seconds; **a publish made through this peer is visible immediately** (the
  peer replaces its manifest with the origin's answer and drops its listing, and a rejected CAS
  drops the cache).
* `--upstream-stale-if-error <secs>` (default **0 = never**): if upstream is unreachable, serve a
  manifest/listing last confirmed at most this long ago. The state is old but consistent: packs are
  immutable, so everything it references that was cached is still readable. Off by default
  because it is a policy choice.

### Concurrency and failure behaviour

* Identical concurrent misses (same pack range, index, object, listing, manifest revalidation) are
  **coalesced**: one upstream request, the rest wait for its result. A flight is only joined by
  requests that start after the last write through the peer finished.
* After an upstream failure the peer fails uncached upstream calls immediately for 2 s instead
  of waiting for each timeout; cache hits are never blocked. (The server's own backend circuit
  breaker is disabled in peer mode for this reason: it would also refuse cache hits.)
* Origin down: reads of cached immutable bytes work; the manifest/listing work only within
  `--upstream-stale-if-error`; uncached reads and **all writes fail with 503** and nothing is
  acknowledged or stored locally, so there is nothing to lose and the client retries later.

### Integrity

Bytes from upstream are checked before they are cached or served: an object against its id, an index
against its own checksum and its pack name, a whole pack (`GET`, as small packs are fetched) against
its trailer and name. A mismatch is a 502-class error and nothing is cached. Ranges of a large pack
cannot be checked alone; clients hash-verify every object they decode, exactly as against any
other store, and the disk cache re-verifies its own entry checksum on every read. The manifest is
trusted from the authenticated upstream (use TLS between tiers: `--upstream https://`, `--upstream-ca`).

## Tests

* `crates/vcs-server/tests/peer.rs`: coalescing (64 concurrent misses = 1 upstream fetch), no stale
  manifest after a push through the peer even with a 1 h ttl, racing readers and pushes, corrupt
  upstream bytes refused and not cached, origin outage, a 3-tier chain.
* `crates/vcs-cli/tests/peering_e2e.rs`: real processes, real `vcs`: client -> edge -> regional ->
  origin, publish through the edge, publish at the origin seen through both peers, origin killed,
  warm clone still works, publish fails cleanly.

## Load test

`scripts/bench_peering.py` (results: see the section added below once measured).

## What this does not do (yet)

* No push-based invalidation: freshness is pull (conditional GET / ttl). Fine for the manifest, which
  is one small key; it is why `ttl=0` still costs one tiny request per read at the origin.
* No eviction coordination or cache warming; a cold peer's first readers pay the origin once per
  object (coalesced).
* `exists` probes (used by a publish to find what upstream already has) are forwarded, not answered
  from cache. Write-heavy sites should publish to a nearby peer, but the bytes still cross to the
  origin, because writes are write-through.
* Writes are not scaled out: one CAS point per repository (the origin's store). Multi-region
  *writes* to different repositories scale by running separate origins; one repository stays
  single-writer-linearizable by design.
* Per-user identity is not forwarded to the origin (it sees the peer).
* No sharded fleet: a peer caches whatever its clients ask for, bounded by `--cache-size`.
