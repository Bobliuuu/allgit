# Backup, restore and disaster recovery

Status: implemented (issue #570). `vcs remote verify | backup | restore | safe-point`, library in
`crates/vcs-remote/src/dr.rs`, tests `crates/vcs-remote/tests/dr.rs` and `crates/vcs-cli/tests/backup_restore.rs`.

## What state there is

A remote is immutable content-named data (`objects/`, `packs/`, `tx/`, `shelves/`) plus one mutable root: the `manifest`
(or, once migrated to sharded manifests, `shard/*` + `decision/*` behind the `manifest` marker). Coordination state
(`gc/lock`, `tomb/`, `gc/hold/`, `gc/safe-point`) is not data and is never copied.

The consistency rule is the same one a publish uses: **data before the pointer that names it**. A backup copies the data
first and installs the manifest last, and only after the closure of that exact manifest was verified in the destination.

## Commands

```text
vcs remote verify [URL] [--manifest FILE] [--deep]
vcs remote backup DEST [--from URL] [--hold-secs N] [--no-hold] [--no-deep]
vcs remote restore FROM TO [--hold-secs N] [--no-hold] [--no-deep]
vcs remote safe-point [URL]
```

* **verify** loads the manifest and walks the closure of every branch head, snapshot, tag, unexpired shelf base, pending
  shard record and retained transaction, reading and hashing every object. It reports *all* missing and corrupt objects
  (first 100 listed, the rest counted) and exits 1 if there is any, or if there is no manifest. `--deep` also reads every pack
  end to end (length, trailer, every entry against its id) and every loose object, reachable or not. `--manifest FILE` checks
  a saved manifest (the raw `manifest` object from an S3 version, a snapshot of the directory) against the store as it is
  now: that is the "is this old manifest still restorable after gc?" question, answered exactly.
* **backup** copies `objects/ packs/ tx/ shelves/` of the source (default: the configured remote) into DEST, which is any
  remote location (directory, `s3://`, ...). Re-running updates the backup incrementally: only keys DEST lacks are
  written. A sharded source is flattened into one ordinary manifest taken from a linearizable snapshot (it can be
  re-migrated with the usual command).
* **restore** is the same copy into a store that must be empty (a store with a manifest or any data refuses), from a backup or a
  live remote. The result is an ordinary remote: `vcs clone TO dir`, or `vcs remote set TO`.
* **safe-point** prints the last destructive gc and any backup holds.

## Why a kill -9 at any moment is safe

Order of writes in `backup`/`restore`: (1) packs, loose objects, tx records, shelf packs; (2) pack indexes and shelf indexes;
(3) shelf metadata; (4) a verification of the manifest snapshot against the destination (`--deep` by default); (5) the
manifest, create-only into an empty destination or compare-and-swap over the previous backup's manifest. Every write is
create-only or a CAS, so a killed process leaves only extra, valid content-named files.

* Fresh destination killed at any point: **no manifest**. `verify` says `NO MANIFEST` and exits 1; nothing is claimed.
* Existing backup updated and killed at any point: the **previous manifest is still in place and its closure is complete**,
  because the update only adds files.
* Re-running the same command resumes (skips what is there) and finishes.

Tested exhaustively in `a_backup_killed_at_every_write_never_leaves_a_manifest_without_its_data` (a destination that dies
after write 0, 1, 2 ... N of a full backup; also while updating an existing backup) and with the real binary and real
SIGKILL in `kill_9_mid_backup_then_verify_and_resume`. Drill on the bench box (`vcs remote backup` of a 6 x 1500-file
remote, SIGKILL after 10 ms ... 1.2 s): the kills at 10 and 30 ms left a destination with 18 keys and **no manifest**
(`verify`: NO MANIFEST), the later ones had finished and verified with `--deep`; re-running the backup after each kill
verified OK.

## The GC safe point

Remote gc deletes objects no root reaches once they are older than the grace period. A backup that reads the manifest
and then copies for an hour could lose that race for objects a force-moved or deleted branch used to reach. So:

* A backup first writes a lease `gc/hold/<id>` ("owner expiry", backend clock, default 1 h, renewed every quarter lease while it
  runs, deleted when it finishes), **then** looks at `gc/lock`: if an unexpired sweeper lock exists it removes its hold and
  stops (`gc is running`).
* `vcs gc --remote` (including `--compact`), **after** taking `gc/lock`, look for holds and refuse with
  `a backup holds a GC safe point` while one is unexpired. Dry runs are not blocked. Each side writes before it reads, so at
  least one sees the other; neither can start destroying or copying under the other.
* A backup killed with kill -9 leaves its hold, which expires on the backend clock (`--hold-secs`, default 1 h) and is
  deleted by the next gc. Cap: a hold claiming to outlive 24 h is clamped.
* Every destructive sweep records `gc/safe-point` (backend time, objects and packs deleted). `verify` and
  `safe-point` print it: **any manifest or backup captured before that time without a hold may name deleted objects**. With a
  backup taken through `backup` this cannot happen: it is verified before its manifest is written.

Holds need a backend that can be collected at all (`Backend::supports_gc`: directory, S3, in-memory, and an HTTP server that
answers `/v1/clock`; the HTTP path is not tested here). On a backend that cannot be collected there is nothing to hold off and `backup` runs without one. If the HTTP
server rejects writes under `gc/` for the caller's role, pass `--no-hold` and make sure no gc runs meanwhile.

## RPO, RTO

* **RPO** = time since the last completed `backup` (it is a point-in-time copy of one manifest; there is no continuous
  replication). Run it from cron at the interval you can lose. The copy of an unchanged repo is a listing plus existence checks.
* **RTO** = restore time = bytes / min(read bandwidth, write bandwidth) + verification (which reads the packs once more) +
  pointing clients at the new URL. Measured on the bench box, local NVMe to local NVMe (not S3, not representative of a network):
  a 307 MB remote (40 x 8 MB random files + 3000 small files) backed up in 1.0 s, restored in 1.0 s, `verify --deep` 0.7 s,
  `verify` 0.25 s. Rehearse on your own bucket; S3 request latency and the parallel copy (`VCS_REMOTE_PARALLEL`, default 8)
  dominate there. Memory: each pack is read whole into memory by a copy worker (gc already caps its rewrites at 512 MiB; a
  larger pack needs that much per worker).

## Restore drill (do it quarterly)

```text
vcs remote backup s3://dr-bucket/backup1            # or a directory
vcs remote verify s3://dr-bucket/backup1 --deep     # exit 0
vcs remote restore s3://dr-bucket/backup1 s3://scratch/restore-test
vcs clone s3://scratch/restore-test /tmp/drill && cd /tmp/drill && vcs log --oneline | head
```

Compare the heads of the live remote and the restore (`vcs remote show`), delete the scratch bucket.

## Restoring an old manifest (S3 versioning)

Save the `manifest` object version you may want back, then before putting it back run
`vcs remote verify URL --manifest saved-manifest`. Exit 0: every object it needs still exists. Exit 1: it lists what a gc
deleted; restore the missing objects from bucket versioning or a backup first (the object bytes are content-named, so any copy of
the same key is correct). For a sharded remote the manifest to save is `vcs remote backup`'s output manifest (the raw `manifest`
key of a sharded remote is only a marker).

## Cross-region replication recipe

1. Primary bucket and a replica bucket in another region, both versioned. Enable S3 replication (CRR) of the whole prefix. CRR is
   asynchronous and does not order keys across objects, so the replica can briefly hold a manifest whose objects have not
   arrived. **Never serve or restore from the replica without `vcs remote verify REPLICA --deep`**; retry until it exits 0.
2. Better: run `vcs remote backup s3://replica-bucket/prefix` from a scheduled job in the replica region. It carries the
   ordering guarantee (manifest last, verified) that CRR does not, and takes the GC safe point on the primary.
3. Keep gc's grace period longer than the interval between backups (default 7 days vs a daily backup).
4. Failover: `vcs remote verify` the replica, point `vcs-server --backend` (or clients' `vcs remote set`) at it. Publishes newer than the last backup are lost and must be re-published from clones.

## Audit log

The server's audit log (`--audit-log`) is still a single local file per server process and is **not** replicated by these
commands. Ship it to a second sink (the `/v1/audit` endpoint, or a log shipper on the file); see `deploy.md`.

## Not done

* No continuous replication; RPO is the backup interval.
* `backup` copies everything under the four prefixes, including unreachable-but-not-yet-collected garbage; it does not copy only the closure.
* A pack is held in memory while copied.
* The sharded layout is not preserved by a backup (flattened to one manifest). A restore of a very large manifest-heavy repo is
  therefore a single-manifest remote until `migrate-sharded` is run again.
* Not run against real S3 (only the directory and in-memory backends are covered by tests; the code uses the same `Backend` calls
  gc uses on S3, and holds are plain create-only/CAS keys).
