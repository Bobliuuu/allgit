# Upgrade and rollback runbook

Issue #567. A newer `vcs` or `vcs-server` can write encodings an older one cannot read. This page says which, how the
**minimum-reader marker** makes an older binary refuse cleanly, and how to upgrade and roll back without an outage.

## What can make a repository unreadable to an older binary

| Feature | Written when | Needs level |
|---|---|---|
| Manifest v1-v4, pack/index v1/v2 | always, by default | 1 (baseline) |
| Compact manifest v5 | the v1-v4 encoding would exceed 64 KiB (`COMPACT_THRESHOLD`), on the next publish | 2 |
| Pack/index v3 (and v4) | remote compaction (`vcs gc --remote --compact`, default `--pack-format v3`; or any publish with `VCS_PACK_FORMAT=v3`) | 2 |
| Sharded manifests | `vcs-server migrate-sharded` (one way) | 3 |

Local repositories have their own formats (`vcs repack --format`, docs/object-model.md); the marker covers the **remote**.

## The marker

One small object, key `format`, next to `manifest`: the minimum reader level plus the `vcs` version that last raised it.

* No marker means level 1. A repository that never needs a newer feature never gets one and stays byte-identical to what
  unmarked binaries wrote.
* A writer raises the level with compare-and-swap **before** the first write that needs it: before the manifest CAS that
  stores a v5 manifest, before uploading a v3 pack, before the sharded freeze. A crash in between leaves the level too high,
  never too low.
* `vcs` (publish, fetch, any manifest load), remote GC and compaction check it first, and `vcs-server` checks it at
  startup. A binary below the level stops with
  `this repository requires vcs format level 2 (raised by vcs 0.x) but this binary only supports level 1 ... Nothing was read
  or changed`, instead of a decode error ("unsupported version 5", "bad index header") or a write around data it cannot parse.
* The level never goes down by itself. Lowering is the explicit rollback step below, and it refuses while the data still
  needs the level.
* `vcs-server` rejects a malformed marker (422) and a write token cannot lower it (403); an admin token can.

Look at it: `vcs-server format --backend <dir|s3://...>` prints the marker and the level the stored data currently needs.

**Limit.** A binary built before this marker existed does not know it and is not protected. The first release containing the
marker is the floor for rollbacks: roll back to it or later, never to an older one, once any level-2 feature was used.

## Upgrade (rolling)

1. Back up: S3 versioning on the bucket, or a filesystem snapshot (docs/deploy.md, "Backups and operations"). Note the current
   `manifest` version id. That is your rollback point.
2. Upgrade **one** `vcs-server` replica. It reads level 1 fine and keeps writing v1-v4 until a feature needs more.
3. Watch it (`/metrics`, logs) through at least one publish and, if you use it, one compaction.
4. Upgrade the other replicas, then clients. Clients on the old floor release keep working until the first level-2 write; after
   that they get the clear refusal above and must upgrade.
5. Opt-in steps raise the level and are the points of no easy return: the first large publish (v5), the first compaction to
   v3, `migrate-sharded` (level 3). Do them only after every replica and the CI/automation clients run the new binary.
   Pass `--pack-format v2` to remote compaction (and do not set `VCS_PACK_FORMAT=v3`) to stay at level 1 for packs.

## Rollback

Case A, nothing level-2 or higher was written yet (`vcs-server format` shows "no marker" and data needs level 1): just swap
the binary back.

Case B, the level was raised. Swapping only the binary is not enough: the older binary refuses (marker) or cannot decode.

1. Stop writers (or put the server behind maintenance), so nothing writes the new format meanwhile.
2. **Packs.** If data needs level 2 because of v3 packs, re-compact to v2: `vcs gc --remote --compact --pack-format v2 --yes`
   (docs/gc.md). Check with `vcs-server format --backend ...` ("data currently needs level 2: ... v3/v4 pack index").
3. **Manifest.** If it is v5 or sharded, restore the manifest version written before the upgrade (S3: copy the noted
   version back over `manifest`; directory: restore the file). Restoring an old manifest is only safe if remote GC has not
   swept what that version references, so take a version from before the last `vcs gc --remote`. A sharded repository cannot be
   un-sharded in place: restore the whole prefix from backup.
4. **Marker.** `vcs-server format --backend <url> --set-min-reader 1`. It refuses (`cannot lower ... the manifest uses the
   compact v5 layout`) while step 2 or 3 is incomplete, so it cannot hide a mistake. At level 1 it deletes the marker.
5. Swap the binary back (to the floor release or later), start one replica, publish and fetch once, then the rest.

Rollback loses nothing if step 3 restored a manifest from before the upgrade and no publishes landed after it. Publishes made
after the restore point are gone from the manifest (their objects remain until GC); republish them.

## Tested

`crates/vcs-remote/tests/format_marker.rs` covers: no marker for small repositories; the marker raised by a publish that
produces a v5 manifest, by compaction to v3 (not v2) and by `migrate-sharded`; an older reader refusing with the documented
message; a binary below the marker leaving the repository byte-identical after publish, fetch, GC and compaction attempts;
monotone raise; lowering refused while v5 or v3 data exists and allowed after the restore steps above.
`crates/vcs-server/tests/format_marker.rs` covers server startup refusal, writers raising over HTTP, and admin-only lowering.
The restore of an S3 manifest version itself is not automated or tested here (docs/known-limits.md).
