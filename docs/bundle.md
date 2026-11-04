# Bundles: offline transport

A bundle is one file that carries objects and branch heads between repositories with no server and no shared
filesystem (USB stick, email, air-gapped network).

```
vcs bundle create <file> [zone | zone/branch | root-branch ...] [--since <rev>]
vcs bundle verify <file>            # default, ~1 ms: header, refs, lengths, prerequisites only (like git)
vcs bundle verify --full <file>     # hash the whole file, decode every object, check the graph
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
| 16+`H` | `P` | pack, `VCSPACK2` (zstd-capable) as written by `create`; `VCSPACK1` is also read (docs/object-model.md); includes its own BLAKE3 trailer |
| end-32 | 32 | BLAKE3 of every preceding byte |

Header records: `pack-format vcspack2` (or `vcspack1`), `pack-bytes P`, `objects N` (each exactly once), then any number of
`zone <id> <path> <label>` (label may be empty; the root zone has an empty path), `head <zone> <branch> <commit-hex>`
`prereq <commit-hex>`, and a last line `x-hdr-blake3 <hex>`: BLAKE3 of every header byte before it (bundles written before it existed lack it). Lines starting `x-` are reserved for extensions and ignored; any other unknown line is an
error, so a future incompatible change bumps the version instead of being half understood. The header is therefore
readable with `head -c 400 file | strings`. The `pack-format` record selects the pack version; v2 entries carry a codec byte and a decoded size is capped at 64 MiB before any decompression.

## Quick and full verify

`vcs bundle verify --full` runs every check in the next section, and that is what `fetch` and `clone` always run: the
receiver hashes the whole file and every object before it writes anything, so a corrupt pack body can never be applied
however the bundle was verified. `vcs bundle verify` (the default, formerly `--quick`, which is still accepted) reads a few hundred bytes however big the bundle is (git's own
`bundle verify` does not hash the pack either). It checks the magic, version and header length, the header against its
`x-hdr-blake3` line (so a changed head, zone or length is caught), the length equation `16 + H + P + 32 == file size`
(truncation or appended bytes), the pack's magic and that its object count equals the header's, and, when the bundle
lists prerequisites, that a repository here has them. It does **not** hash the pack or the trailer: a flipped byte inside
the pack body or the trailer passes the default verify (use `--full`). A bundle written before the header hash existed has nothing cheap to check
the header against, so the default hashes the whole file for it instead.

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
