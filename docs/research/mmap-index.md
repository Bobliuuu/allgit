# AGI5: memory-mapped index (issue #493)

Format (all versioned; see `crates/vcs-repo/src/index5.rs`, `indeximage.rs`):

* `index` is an immutable base generation: 64-byte header, 257-entry first-byte fanout, fixed-width 64-byte records
  sorted by path, a path heap, the cache-tree section, and a 64 KiB-chunk checksum table (`sum64`). It is mapped
  read-only; a lookup is a fanout + binary search in place and verifies only the chunks it touches. A full decode
  verifies all chunks. Layout version byte = 1; a newer version, a wrong length or a bad checksum is a clear
  `Corrupt` error, and a binary that only knows AGI2/3/4 refuses the file ("bad magic") instead of misreading it.
* `index.log` is an append-only edit log tied to the base generation (header carries generation + checksum-table
  checksum). One frame (`len, seq, checksum, ops`) per command is written with a single `write`. A torn or
  corrupt tail fails its checksum: readers use the state before it, the next writer truncates it. When the log
  passes 4096 edits or 1 MiB (or a command edits more than 1024 paths) the next write folds it into a new base
  (temp file + atomic rename); a log naming an older generation is ignored.
* Readers map the base, read the log, and re-stat the base; if a writer replaced it meanwhile they start over.
* Older layouts (AGI2/AGIX/AGI3/AGI4) are read as they are and upgraded by the first write.
* The fsmonitor fingerprint is now a hash of the header + checksum table + frame checksums (a few KiB), replacing
  the blake3 of the whole file and the `ixdigest` file cache.

Measured on 100k files, macOS, release builds, median of 9-15 runs, loaded host (load 9-17), wall / CPU ms,
base = origin/main, new = this branch:

| op | base | new |
|---|---|---|
| add one new file | 26 / 24 | 13 / 11 |
| rm | 26 / 16 | 22 / 11 |
| mv | 29 / 17 | 24 / 11 |
| diff (1 edit) | 38 / 33 | 25 / 12 |
| restore (revert) | 30 / 19 | 23 / 12 |
| add dir, commit -a, add ., status, switch --detach | within noise (not slower) | |

`add .`, `switch --detach` and `commit -a` are bounded by the stat of 100k files and the full save, not by the
index format. `vcs` has no `-a`/`-u` flags: `commit -a` is `add F && commit`, `add -u` is `add <dir>`.
