# Security: trust boundaries and hardening

Status: **implemented** for the decoders listed below; the threat review is a living document.

## Trust boundaries

Bytes cross into the process from five places. Everything on the outside of these edges is treated as hostile; a decoder
must return an error for bad input and must never panic, loop forever, or allocate based on an unchecked length.

| Boundary | Entry point | Who controls the bytes |
|---|---|---|
| Object bytes (store, remote, pack) | `Object::decode` (`vcs-objects`) | anyone who can write to the object store or the bucket |
| Remote manifest | `Manifest::decode` (`vcs-remote`) | anyone with write access to the bucket |
| Pack index / pack data | `PackIndex::parse`, `Pack::open/read/verify` (`vcs-store`) | disk damage, or the bucket |
| Git import stream | `Repo::import_git` (`vcs-repo`) | whoever produced the `git fast-export` stream |
| S3 list responses | `parse_list_page` (`vcs-store-s3`) | the S3 endpoint (or a man in the middle on `http://`) |

Local state files (`zones`, `refs`, `workspace`, the index) use the same length-checked `Reader`, but they live inside the
user's own `.vcs` directory and are treated as trusted-but-possibly-corrupt, not as attacker input.

## Limits that decoders enforce

* **Counts** are bounded by the remaining input: `Reader::count(min_elem_size)` rejects any count that cannot fit in the
  bytes left, so `Vec::with_capacity(n)` is never driven by an unchecked number. Commits and operations accept at most
  64 parents; the manifest's applied-transaction list at most 4096 entries.
* **Strings** have per-field caps: names 255 bytes, zone names 64, branch names 200, paths 4096, commit/operation
  messages 1 MiB, signature name and email 256, lock reason 1024, lock holder 512.
* **Depth**: the object format has no recursion (trees reference children by id), so decoding cannot overflow the stack.
  Path depth is bounded by the 4096-byte path cap.
* **Canonical form**: varints must be minimal, lists must be strictly sorted (objects and manifest), trailing bytes are an
  error. A byte string that decodes always re-encodes to itself; tests assert this.
* **Pack index**: entry count times entry size must equal the body length exactly (checked multiplication), every entry
  must lie inside the pack after the header with checked addition, ids strictly sorted. A pack read allocates only for
  bytes actually read, not for the length the index claims.
* **Git import stream**: a line (command, header, or a line inside a delimited `data <<` block) is at most 64 KiB, a `data` block at most 1 GiB (the declared length
  is checked before reading and never used to preallocate). A commit that the strict object decoder would reject
  (oversized message or identity, more than 64 parents) is refused instead of stored. Paths that vcs cannot represent
  are skipped and reported.
* **S3 list responses**: a page body is at most 16 MiB, at most 100,000 pages and 10,000,000 keys per listing, a repeated
  continuation token is an error (no endless loop), and every returned key must lie under the requested prefix.

## Verification

* `crates/vcs-objects/tests/hardening.rs`, `crates/vcs-remote/tests/hardening.rs`, `crates/vcs-store/tests/hardening.rs`,
  `crates/vcs-repo/tests/hardening_import.rs` and the unit tests in `crates/vcs-store-s3/src/lib.rs` hold proptest
  round-trip and never-panic properties plus a deterministic mutation loop for each decoder. Each fixed crash has its own
  regression test named after the failure.
* `fuzz/` contains cargo-fuzz targets (`object_decode`, `manifest_decode`, `pack_index`, `pack_data`, `import_git`,
  `s3_list`, `p4_marshal`, `import_p4`, `remote_helper`, `http_request`, `audit_verify`, `config_parse`, `ignore_parse`,
  `index_decode`, `pack_fetch`, `commit_graph`, `state_files`, `policy_tokens`, `assist_response`). It is its own cargo workspace, so `cargo build --workspace` never builds it. Run, from `fuzz/`:
  `cargo +nightly fuzz run <target> -- -max_total_time=60`.

## Fuzz results

Run on a 30-core Linux box (rustc nightly 1.101.0, cargo-fuzz 0.13.2, libFuzzer, ASan), one process per target, from
`fuzz/`, with `cargo +nightly fuzz run <target> [seeds/<target>] -- -max_total_time=60 -rss_limit_mb=4096`. Starting from
an empty corpus (the `import_git` run used `fuzz/seeds/import_git`; note libFuzzer writes new inputs into that directory, so run it on a copy), final tree state:

| Target | Duration | Executions | Result |
|---|---|---|---|
| `object_decode` | 61 s | 4,379,191 | no crash |
| `manifest_decode` | 61 s | 3,014,220 | no crash |
| `pack_index` | 61 s | 3,443,715 | no crash |
| `pack_data` | 61 s | 17,713 | no crash |
| `import_git` | 61 s | 2,003 | no crash |
| `s3_list` | 61 s | 1,366,787 | no crash |

### Campaign (20 minutes per target)

Same box (30 cores, shared, load average about 40 during the run), rustc nightly, cargo-fuzz 0.13.2, ASan. Eight
single-process runs at a time (at most 8 workers), each started from a corpus seeded with real files (objects, index,
commit-graph, conflicts/rebase/stash/zones/refs/workspace files, manifest, pack and idx written by the CLI and
`vcs publish`; `git fast-export` streams; the p4 fixtures; an audit log written by `vcs-server`; sample S3 pages;
the repository `.gitignore`; example token and policy files). Command, per target:
`<target> -max_total_time=1200 -timeout=5 -rss_limit_mb=4096 -max_len=<4096, or 65536 for object_decode, pack_index,
pack_fetch, import_git, import_p4, remote_helper, audit_verify> -report_slow_units=1 corpus/<target>`. Release builds
with debug assertions and overflow checks (cargo-fuzz default).
`cov` is libFuzzer edge coverage, `ft` features, as printed at the end of the run.

| Target | Executions | cov | ft | Corpus | Result |
|---|---|---|---|---|---|
| `object_decode` | 52,834,447 | 1422 | 3440 | 788 | no crash |
| `manifest_decode` | 42,187,900 | 1040 | 2647 | 573 | no crash |
| `pack_index` | 49,019,784 | 204 | 510 | 74 | no crash |
| `pack_data` | 64,874 | 1003 | 2885 | 251 | no crash |
| `import_git` | 20,904 | 5105 | 13464 | 489 | no crash |
| `s3_list` | 4,011,716 | 205 | 1066 | 268 | no crash |
| `p4_marshal` | 37,774,280 | 231 | 1419 | 391 | no crash |
| `import_p4` | 52,472 | 2127 | 3769 | 397 | no crash |
| `remote_helper` | 18,989 | 4610 | 11732 | 473 | no crash |
| `http_request` | 22,782,259 | 571 | 2101 | 601 | no crash |
| `audit_verify` | 2,062,612 | 462 | 861 | 192 | no crash |
| `config_parse` | 12,907,837 | 413 | 1822 | 650 | no crash |
| `ignore_parse` | 2,119,730 | 508 | 3331 | 890 | no crash |
| `index_decode` (AGI2 and AGIX) | 36,951,440 | 770 | 2157 | 372 | no crash |
| `pack_fetch` (pack v1 index and trailer) | 22,618,501 | 573 | 814 | 51 | no crash |
| `commit_graph` | 11,594,658 | 364 | 1045 | 92 | no crash |
| `state_files` (conflicts, rebase, stash, zones, refs, workspace) | 154,996 | 1927 | 2835 | 353 | no crash |
| `policy_tokens` | 5,168,682 (after the fix; 3,866,021 before it crashed) | 938 | 4789 | 1133 | 1 finding, fixed |
| `assist_response` | 6,316,399 | 2941 | 12527 | 2511 | no crash |

Finding: `policy_tokens` hit a libFuzzer timeout on a policy line whose glob was about 60 repeated `*` (and the same
shape with many `**` segments): the recursive glob matcher was exponential. Fixed with an iterative matcher and a
dynamic-programming segment match, regression test `many_wildcards_do_not_blow_up` in `crates/vcs-server/src/glob.rs`.
`import_git`, `import_p4`, `pack_data`, `remote_helper` and `state_files` reported "slow units" of about 1 s; re-run alone
each took 0.1 to 0.9 s wall time including process start and the temp-repository setup the target does on every input, so
they are load and disk noise on the shared box, not a slow parser (no fix made).

`cargo fuzz -s undefined` does not exist in cargo-fuzz 0.13.2 (sanitizers: address, leak, memory, thread, none). Instead
`import_git` and `pack_fetch` were run a second time with `--careful -s none` (std built with debug assertions and extra
UB checks, 20 minutes each, same flags): `import_git` 32,555 executions (cov 4317), `pack_fetch` 83,892,258 executions
(cov 417), no crash. The memory and thread sanitizers were not run.

Not covered by the above: the `tls` feature of `vcs-server` (the HTTP request parser was fuzzed without it), signature
and live-network paths of the Claude assistant (only the pure response parsing and validation), and a 20-minute run is
still not a saturation claim (`pack_data`, `import_git`, `import_p4`, `remote_helper` and `state_files` run below 200
executions per second).

Findings fixed while doing this work, each with a regression test: pack index offset overflow (`off + len` wrapped), a
panic on a git identity line with `>` before `<`, unbounded line and `data` buffering in the import parser, import
storing commits the strict decoder rejects, a panic in S3 list handling for keys outside the prefix plus an endless loop on
a repeated continuation token, and the remote manifest accepting unsorted or duplicate keys and an empty v2 form (the
last one found by the `manifest_decode` fuzz target on its first run: `AGMF 02 01 00 00 00 00 00`).

## What is not defended

* **Resource exhaustion by honest-looking input.** A 1 GiB `data` block, a 10 GB pack or a million-entry tree is accepted
  if it is well formed; the caps above bound a single allocation, not the total work. Run imports of untrusted streams
  under an OS memory limit.
* **A malicious remote with write access** can delete or withhold objects and can roll the manifest back to an older
  valid generation. Hash verification proves an object is intact, not that it is the newest. Signing is out of scope here.
* **Confidentiality and transport.** `http://` S3 endpoints send signed requests in the clear; use `https://` (feature
  `tls`). Credentials come from the environment, so a process on the same machine can read them.
* **Local attackers** who can write into `.vcs` or the work tree, hostile filesystems (symlink races), and
  hash-collision attacks on BLAKE3 (assumed infeasible).
* **Timing and side channels**, and denial of service on the remote by a valid but abusive client.
* **Local state files** are checked for structure but not for quadratic work: a zones file with hundreds of thousands of
  entries makes the overlap check slow.
* Decoders not fuzzed (the lock protocol messages, the TLS path) were reviewed for panics and unbounded allocation only
  by reading.
