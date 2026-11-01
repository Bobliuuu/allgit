# Bundles: offline transport

A bundle is one file that carries objects and branch heads between repositories with no server and no shared
filesystem (USB stick, email, air-gapped network).

```
vcs bundle create <file> [zone | zone/branch | root-branch ...] [--since <rev>]
vcs bundle verify <file>            # needs no repository; inside one it also checks prerequisites
vcs clone <bundle> <dir>            # new repository from a complete bundle
vcs fetch <bundle>                  # import objects, set remote-tracking heads (local branches untouched)
```

With no selector every branch of every zone is bundled. `zone` selects all branches of that zone, `zone/branch` one
branch, a bare name that is not a zone selects that branch of the root zone. `--since <rev>` leaves out everything
reachable from `<rev>` (resolved in each selected zone where it resolves; `HEAD~1`, a tag, a branch or a commit
prefix) and records that commit as a **prerequisite**: the receiver must already have it. Such an incremental bundle
cannot be cloned, only fetched.

## File format (version 1)

All integers are little endian.

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | magic `VCSBNDL\n` |
| 8 | 4 | bundle version (1) |
| 12 | 4 | header length `H` (at most 1 MiB) |
| 16 | `H` | header, UTF-8 text, one tab-separated record per line |
| 16+`H` | `P` | pack, in the existing `VCSPACK1` format (docs/object-model.md), including its own BLAKE3 trailer |
| end-32 | 32 | BLAKE3 of every preceding byte |

Header records: `pack-format vcspack1`, `pack-bytes P`, `objects N` (each exactly once), then any number of
`zone <id> <path> <label>` (label may be empty; the root zone has an empty path), `head <zone> <branch> <commit-hex>`
and `prereq <commit-hex>`. Lines starting `x-` are reserved for extensions and ignored; any other unknown line is an
error, so a future incompatible change bumps the version instead of being half understood. The header is therefore
readable with `head -c 400 file | strings`. The pack is the repository's existing pack format: a v2-style pack does
not exist in this tree yet, and the `pack-format` record is how it will be announced when it does.

## Reading untrusted files

Order of checks (all before any repository write):

1. Magic, then total length against the minimum, then the **whole-file BLAKE3** is streamed (64 KiB buffer) and compared
   with the trailer. Truncation or any flipped bit stops here.
2. Version, header length (at most 1 MiB and inside the file), UTF-8, every record parsed with the repository's own name
   validators; duplicates and undefined zones rejected; `P` must equal the real file size minus preface and trailer.
3. Pack scan: count equals the header's `N`; each object length is checked against 64 MiB **and** against the bytes
   actually left in the file before a buffer is allocated, so allocation is bounded by the file's real size, never by a
   length field; the strict canonical decoder runs on every object; ids are recomputed (a bundle cannot name an object
   by a wrong id); a repeated id is an error; the pack's own checksum must match.
4. Graph: every head is a commit of its declared zone present in the bundle (or equal to a prerequisite); walking its
   parents, every commit belongs to that zone and to no other zone, and every referenced tree, blob and chunk is in the
   bundle or, when applying, already in the receiving store.
5. When applying: every prerequisite is a commit the repository has; zone definitions agree with the local ones (same
   path for an existing id, no overlap for a new one).

Memory is one small node per bundled object (id, child ids), so it grows linearly with the bundle, never faster.
`verify` without a repository can only accept dangling references when the bundle lists prerequisites.

## Applying: all zones or none

`fetch` and `clone` hold the repository lock and write in this order: objects (then `sync`), a `Transaction` object that
records the ref updates (the same record `publish` uses; garbage collection treats stored transactions as roots), the
zone definitions, and finally **one** atomic write of `remote-refs`, which carries every zone's new head. Remote-tracking
refs are a single file replaced by rename, so a reader sees all of the bundle's heads or none of them. A crash earlier
leaves extra, unreferenced objects (collected by `gc`) and at most newly defined empty zones; running the command again
completes it. `crates/vcs-repo/tests/bundle.rs` injects a crash at every state-file write of a three-zone apply and
checks that no ref moved, `fsck` is clean, and the retry succeeds. `clone` additionally sets local branches and checks
out `main` (else the first branch) of each zone.

## Limits and non-goals

- Tags and locks are not carried; bundle heads only.
- `create` builds the pack in memory, so its memory use is the bundle size. `verify` and `fetch` stream.
- A sparse clone cannot bundle objects it does not have (it says which is missing).
- No signatures: the hash detects corruption, not a malicious author. Check the source out of band.
- Fuzz-style coverage: the tests flip, overwrite, grow and delete bytes at random offsets, with and without recomputing
  the trailer so the structural checks (not just the hash) are exercised, and assert no panic and that a failed apply
  changes nothing. This is a deterministic seeded loop, not a coverage-guided fuzzer.
