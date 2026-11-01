# Security review (October 2026)

Scope in priority order: vcs-server, client credentials, working-tree safety, decoding of remote data, the Claude
assistant adapter, supply chain. Method: read the code, wrote a failing test for each suspected problem, ran it on the
Linux build box, fixed what failed. One small PR per finding. Companion to the standing threat model in
[security.md](security.md).

Every PR except two has a test that failed before the fix: #275 is configuration (the failing `cargo deny` output is the evidence) and the folding part of #273 (finding 12) has a unit test written together with the fix and not run red. "Verified" means observed, not just read.

## Findings

| # | Severity | Area | Finding | Status |
|---|---|---|---|---|
| 1 | High | working tree | `.vcs` aliases on folding filesystems | PR #273 |
| 2 | Medium | server | lock record can be re-stamped in another principal's name | PR #264 |
| 3 | Medium | server | `next_token` can be exhausted by one write | PR #279 |
| 4 | Medium | store | chunk manifest `total_len` sizes an allocation | PR #265 |
| 5 | Medium | server | DELETE is not in the audit log | PR #260 |
| 6 | Medium | server | manifest key look-alikes skip manifest validation (case-insensitive or Windows backends) | PR #261 |
| 7 | Low | server | control characters in keys (list output injection) | PR #261 |
| 8 | Low | s3 | `S3Config` derives `Debug` with the secret key and session token | PR #274 |
| 9 | Low | s3 | S3 client follows redirects, replaying `x-amz-security-token` | PR #274 |
| 10 | Low | client | quote/backslash in credential host reaches `security -i` | PR #280 |
| 11 | Low | supply chain | `cargo deny check licenses` fails on webpki-roots | PR #275 |
| 12 | Info | working tree | folding of `ß`, `ſ`, `ς` missing from collision check | PR #273 |
| 13 | Medium | client | `VCS_TOKEN` is sent to every `http://` remote | accepted, see below |
| 14 | Low | server | no throttling of failed authentication, no per-address connection cap | accepted |
| 15 | Info | repo config | `merge.assist`, `core.editor`, bisect run commands from `.vcs/config` | accepted |

### 1. `.vcs` aliases (High, verified on macOS APFS)

`validate_component` compared with `eq_ignore_ascii_case(".vcs")`. On this Mac's APFS volume `ls ".vcſ/config"` (U+017F,
LATIN SMALL LETTER LONG S) opens `.vcs/config`. A tree containing a directory `.vcſ` with a `config` file therefore
overwrites repository metadata on checkout. Because `.vcs/config` can hold `merge.assist` or `core.editor`, that is code
execution from a crafted repository. Fix: refuse every spelling that APFS, HFS+ or NTFS resolve to `.vcs`: NFKC plus
lowercase (long s, fullwidth), HFS+ ignorable code points (U+200B..200F, U+202A..202E, U+206A..206F, U+FEFF), trailing
dots and spaces, `:stream` suffixes, `VCS~N` short names. The symlink-target check shares the test. Tests:
`vcs-objects/tests/security_names.rs`. Names that merely look similar (`.vcsignore`, `vcs`, `résumé.txt`) stay valid.
Existing repositories that contain such a name will fail to decode that tree; none is expected.

### 2. Lock holder re-stamp (Medium)

The validator allowed a changed lock record whose holder was neither the writer nor the old holder only when the old
record was expired, and the exception let any write principal renew alice's expired lock under alice's name with a fresh
server-computed lease. A changed record now must name the writer. Test: `vcs-server/tests/security_locks.rs`.

Probed and found sound (no change): `time_hw` cannot be ratcheted forward because any write with lock state sets
`time_hw` to `max(server, stored)`; breaking a live foreign lock needs admin; an unchanged record is skipped, so a
principal cannot disturb locks it did not touch.

### 3. `next_token` exhaustion (Medium)

`next_token` may not decrease, and nothing bounded how far it could rise, so one write principal could set it to
`u64::MAX`. After that every acquisition overflows (panic in a debug build, wrapped and reused fencing tokens in release).
There is no recovery path because it cannot decrease. The server now rejects an advance of more than 65536 in one write.
Test: `vcs-server/tests/security_tokens.rs`. Lock tokens at or above `next_token` were already rejected by
`Manifest::decode`.

### 4. Allocation sized by a chunk manifest (Medium)

`ObjectStore::get_content` did `Vec::with_capacity(total_len)`. `total_len` only has to equal the sum of the chunk
lengths and each chunk may claim 64 KiB, so a manifest of N bytes can claim about 2000 N bytes. A 2 MB manifest claimed
3.9 GB (measured with a counting allocator); a larger one aborts the process before any chunk is fetched. The reservation
is now capped at 16 MiB and grows as verified chunks arrive. Test: `vcs-store/tests/security_alloc.rs`.

### 5. DELETE not audited (Medium)

The audit log recorded only `PUT`. DELETE (admin-only garbage collection) left no trace in the tamper-evident log, and
refused DELETEs were invisible. Both are recorded now. Test: `vcs-server/tests/security_audit.rs`.

### 6, 7. Key hygiene (Medium / Low)

`validate_key` rejected only NUL, so a key with a newline forged extra lines in `GET /v1/list` output; it now rejects
all control characters. The server treated only the exact key `manifest` as the manifest. On a case-folding or Windows
filesystem backend `MANIFEST`, `manifest.`, `manifest ` and `manifest::$DATA` are the same file, so a write principal
could replace the manifest without the lock, zone and policy validation. The server now refuses those spellings. On the
default Linux filesystem backend and the S3 backend this was not exploitable; verified only at the HTTP layer, not on a
Windows or macOS server. Tests: `vcs-server/tests/security_keys.rs`.

### 8, 9. S3 credentials (Low)

`#[derive(Debug)]` on `S3Config` printed the secret key and session token. Replaced with a redacting impl. ureq strips
`Authorization` when it follows a redirect to another host (verified for `HttpBackend`, see `security_redirect.rs`) but
not custom headers, so a hostile or compromised S3 endpoint could redirect a signed request and receive
`x-amz-security-token`. The S3 agents now use `redirects(0)`. Tests: `vcs-store-s3/tests/security.rs`.

### 10. Credential host quoting (Low)

The macOS keychain helper builds `add-generic-password -a "<host>" ...` for `security -i`. The host comes from the remote
URL and only whitespace, control characters and `=` were refused. Quote, apostrophe and backslash are refused now.

### 11. cargo-deny licenses (Low)

See the supply-chain section. PR #275 adds a per-crate exception rather than widening the allow list.

### 13. `VCS_TOKEN` goes to every http remote (Medium, accepted for now)

`VCS_TOKEN` takes precedence over the credential helper and is not tied to a host. A repository whose `.vcs/remote`
points at an attacker's server (the file is local, so this needs the user to add or accept that remote) receives the
token on the first request. Mitigations considered: bind the variable to a host (`VCS_TOKEN_HOST`), or prefer the
helper whenever the remote is not the host named at login. Not done because it changes documented behaviour; recommend
using `vcs login` (host-scoped) instead of the environment variable, and tokens with the narrowest role and zones.
Related: the client speaks plain `http://` unless built with the `tls` feature; bearer tokens then cross the network in
clear text. The README already says to use loopback, a private network or a TLS proxy.

### 14. Availability limits (Low, accepted)

Failed authentication is not throttled and connections are capped globally (256), not per address, so one host can hold
all slots for the 10 s header deadline repeatedly. Tokens must be high-entropy random strings (the table stores BLAKE3
hashes and compares in constant time over all entries). Run behind a proxy that does per-address limits.

### 15. Commands from repo config (Info, accepted)

`credential.helper = !command` is refused from repo config. `merge.assist`, `core.editor` and bisect commands are not
(they run commands the user configured or typed). `.vcs/config` is not part of the object graph, so a clone cannot set
it; finding 1 was the way to reach it from a repository, and is closed. Consider applying the credential-helper rule to
`merge.assist` and `core.editor` for defence in depth.

## Reviewed and found sound (no change)

* Token comparison: BLAKE3 hash of the presented token, constant-time compare against every entry, expiry checked after.
* HTTP framing: one request per connection, `Transfer-Encoding` refused, duplicate or malformed `Content-Length`
  refused, header size and count caps, header/body deadlines with a shared overall deadline (slowloris), NUL and
  control characters in names refused, absolute-path targets only.
* Rate limit: keyed by the authenticated principal, never by a header; unauthenticated requests do not consume buckets.
* Object keys: `..`, `.`, empty and backslash components refused; `objects/` bodies must hash to their key.
* DELETE: admin only, never the manifest.
* TLS: rustls with default protocol versions, no client-auth bypass, client verification always on (CA bundle optional,
  never disabled).
* Audit chain: detects edits, reordering, insertion and deletion in the middle; truncating the newest records is not
  detectable by design (documented in `audit.rs`; anchor the head hash elsewhere).
* Working tree writes: preflight refuses writing through symlinked ancestors, untracked overwrites and file/dir
  conflicts; writes go to a temp name and `rename`, so hardlinks and existing links are replaced, not written through;
  directory walks skip FIFOs, sockets and devices; symlink targets must stay inside the worktree; case-fold and
  normalization collisions are refused.
* Claude assistant adapter: output only becomes a proposal under `.vcs/proposals/`; applying needs an explicit accept.
  The API key travels only in `x-api-key`, is scrubbed from errors, `redirects(0)`, URL must be https or loopback http.
  Responses and inputs are size capped, proposals must preserve the non-conflicting hunks. Prompt injection from file
  contents can at worst produce a bad proposal that the user sees as a diff.
* Credential file store: created with mode 0600 via `create_new`, mode checked on the opened descriptor (no
  check-then-open race), tokens refused if they contain whitespace or control characters, token never in argv or logs.

## Supply chain

Run on the box: `cargo audit` 0.22.2 (1395 advisories, 193 crates): no vulnerabilities, no unmaintained or yanked
warnings. `cargo deny` 0.20.2: advisories ok, bans ok, sources ok; licenses failed only for `webpki-roots` (CDLA-
Permissive-2.0, `tls` feature); fixed by PR #275, afterwards all four checks pass. Duplicate crate versions
(`cpufeatures`, `getrandom`, `syn`, `windows-sys`) are warnings only. Open dependabot branches (sha2, criterion,
tree-sitter) were not evaluated.

## Not covered

* `feat/pack-v2` does not exist on the remote, so pack v2 / zstd decompression bombs were not reviewed. Require a bound
  on decompressed size (declared and actual) and a ratio cap when it lands, and fuzz it.
* Windows: reserved device names (`CON`, `NUL`, `AUX`, `COM1`), 260-character path limits and ADS on non-`.vcs` names are
  not handled by name validation. Windows is not a supported platform yet; do not claim it before this is done.
* `import-git`, `import-p4` and the git remote helper were only checked through the shared name validation (which
  now rejects the aliases in finding 1); their parsers were not fuzzed or re-read.
* `stash`, `rebase` and `merge` write paths were assumed to share the checkout preflight; not individually tested for
  symlink races.
* Local TOCTOU attacks (a hostile process racing the preflight checks) and anything needing a local attacker with write
  access to the worktree.
* Server key-file permissions (tokens file, audit key), log rotation, and the S3 backend behaviour of a hostile bucket.
* The gap between the CAS read and write for non-manifest keys, and arbitrary free-form keys a write principal may
  create (`tx/...`): intentional, not reviewed for quota.
* `cargo clippy --workspace --all-targets` currently fails in `vcs-bench` on `main` (unrelated to this review); the
  new tests were checked with rustfmt and compile cleanly.
