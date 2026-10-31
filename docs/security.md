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
  `s3_list`). It is its own cargo workspace, so `cargo build --workspace` never builds it. Run, from `fuzz/`:
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

`pack_data` and `import_git` are slow per execution because each one builds files or a repository in a temp dir. These
are minute-long smoke runs, not a claim of coverage saturation; run longer before relying on them.

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
* Decoders not listed in the table above (merge-state files, the lock protocol messages) were reviewed for panics and
  unbounded allocation only by reading, not by fuzzing.
