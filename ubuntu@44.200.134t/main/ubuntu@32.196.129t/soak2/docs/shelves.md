# Shelves

A shelf is a private, named copy of your uncommitted (or unpublished) work, stored on the remote so that you
can pick it up on another machine or hand it to a teammate, **without publishing a commit and without
touching the manifest**. Shelving therefore has no effect on the compare-and-swap publish protocol, on
branches, or on what `fetch` and `clone` see. The idea is Perforce's `p4 shelve`.

```text
vcs shelve [-m msg] [--zone Z] [--expires 30d] [name]    park the changes of the working tree
vcs shelves [--all]                                       list your shelves (--all: everyone's you may read)
vcs unshelve [--drop] <principal/name>                    apply a shelf to the working tree (--drop: delete it after a clean apply)
vcs shelve --delete <name | principal/name>               remove a shelf
```

`shelve` does not change your working tree or index (unlike `stash push`). `unshelve` works like
`stash pop`: it three-way merges (base = the commit the shelf was made on, ours = your zone head,
theirs = the shelved content), stages the result and, on conflicts, leaves marker files that show up as
unstaged changes. It refuses to run over a zone with uncommitted changes. The shelf stays on the remote
until its owner (or an admin) deletes it or it expires.

## What is stored

Under `shelves/<principal>/<name>/` on the remote:

| key | content |
| --- | --- |
| `pack`, `idx` | an ordinary pack (v2) and its index with the objects of the shelf: per zone with changes an ephemeral commit (parent: the zone head the work was based on, never added to any branch) plus every tree and blob the remote does not already have |
| `meta` | a small text file: `vcs-shelf 1`, principal, name, message, author, created time, optional expiry, per-zone `(zone, branch, base, commit)`, the `base` commits, the object count and the pack checksum |

All three are written with create-only PUTs, `pack` and `idx` first and `meta` last. A shelf exists exactly
when its `meta` exists. A shelve that crashes leaves debris (a pack without a meta) but never a half-visible
shelf, and the name cannot be silently overwritten: shelving to an existing name fails.

If a zone's base commit is on the remote, everything reachable from it is left out of the pack and the base
commit is recorded in `meta` (`base` lines). If it is not (unpublished commits), the pack carries the whole
history it needs and `base` is empty. `--zone Z` shelves only one zone.

## Who may do what (`vcs-server`)

* Only the **owner** (the token's principal name; the CLI asks `/v1/whoami`) or an **admin** can create or
  delete keys under `shelves/<principal>/`. The key must be exactly `shelves/<principal>/<name>/{meta,pack,idx}`,
  keys are create-only (`If-None-Match: *`), and a `meta` is parsed by the server and must name the principal and
  shelf of its key. A read-only token cannot shelve.
* Shelves are **private by default**: another principal gets a 404 for the keys and does not see them in
  listings. A policy rule opens them up, per owner:

  ```text
  shelve-read <reader|*> <owner|*> **
  ```

  `shelve-read bob alice **` lets bob read (never write or delete) alice's shelves; `shelve-read * alice **` lets
  everyone. The owner takes the place of the zone field and the glob must be `**`. This is a grant on the whole
  shelf: a shelf's pack can hold objects of any zone, so a `shelve-read` grant is **not** narrowed by
  `allow-read` / `deny-read`. Policies with only read-type rules (including `shelve-read`) do not switch writes to
  deny-by-default.
* On a directory or S3 remote there is no server: anyone who can read the directory or bucket can read shelves,
  and the principal is `VCS_PRINCIPAL` (else the author email, with unusual characters replaced by `_`). Use
  vcs-server when privacy matters.

## Integrity

`unshelve` downloads `idx` and `pack` and checks, before anything is stored: the index structure, that it agrees
with `meta` (length, object count, checksum), the pack trailer and BLAKE3 checksum, and that **every object
hashes to its id**. It then checks that each shelved commit exists, belongs to its zone and has the recorded
parent. Objects it needs from the shared store (the base commits, if your repository lacks them) are fetched
with the usual hash-verified fetch. A corrupted shelf is refused with `invalid shelf: ...` and nothing is
applied.

## Garbage collection

`vcs gc --remote` (docs/gc.md) knows about shelves:

* **Shelves are roots while they exist.** A shelf's pack lives outside `objects/` and `packs/`, so a sweep
  never touches it. What it needs from the shared store are its `base` commits, and those (with everything
  below them) are roots of every sweep, first look and remark, for as long as the shelf's `meta` exists. Without
  this, a branch that moved on and a sweep after the grace period would delete the history a shelf was built on.
  Deleting or expiring the shelf releases them at the next sweep.
* **Concurrent shelve and sweep.** `shelve` performs the publisher's handshake for its `base` commits on
  GC-capable backends (directory, in-memory): touch each, check that no `tomb/` entry exists, check each still
  exists, check it all happened within the publisher hold time, and only then write `meta`. Together with the
  sweeper's "tombstone, then read roots" order this is the same Dekker argument as for publish: either the sweeper's
  remark sees the new `meta` and keeps the base, or the shelver sees the tombstone (or the missing object) and
  aborts, removing its debris. A shelf is never left pointing at objects a sweep took. On `vcs-server` and S3
  remotes (which cannot run the handshake: `supports_gc` is false there) the sweep itself runs on the storage
  side and reads the same `shelves/` metas.
* **Expiry.** `--expires 90m|12h|30d|2w` stores an expiry in `meta` (backend clock when the backend has one, else
  yours). A non-dry-run `vcs gc --remote` deletes expired shelves (`meta` first) and does not count them as roots;
  `--dry-run` lists them. Nothing else ever expires a shelf.
* An unparsable `meta` makes `vcs gc --remote` refuse to run ("remote is damaged") instead of guessing which
  objects it protects. The server refuses to store such a `meta`, so this needs someone writing to the storage
  directly.

Tests: `crates/vcs-remote/tests/shelf_gc.rs` runs the real sweeper against shelves, including a sweep whose
hook creates a shelf between its first look and its remark, a shelve that meets a tombstone, and mutation tests
(`GcQuirks::sweeper_ignores_shelves`) that fail if shelf roots are ignored.

## Limits

* Untracked files are not shelved (as with stash): `vcs add` them first.
* A shelf can pin its base commits indefinitely; use `--expires` and delete shelves you no longer need. An admin
  can delete anyone's shelf.
* Pack-size on download is capped at 4 GiB and a single object at 64 MiB.
* `vcs unshelve --drop <shelf>` deletes the shelf after a clean apply (a conflicted apply keeps it). Deleting is subject to the usual rules: on `vcs-server` only the owner or an admin may delete, so `--drop` of someone else's shelf applies and then fails with 403.
* There are no per-principal shelf quotas yet (a shelf is capped at 4 GiB; use `--expires`). Quotas need a server-side count of a principal's shelves and bytes and an owner decision on the limits.
* Tested against a local S3 server (RustFS 1.0.4): `crates/vcs-cli/tests/shelve.rs` `shelve_and_unshelve_over_s3` and `shelves_on_vcs_server_over_s3_are_private_until_a_shelve_read_rule`, gated on `VCS_TEST_S3_ENDPOINT`.
