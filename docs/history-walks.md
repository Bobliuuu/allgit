# History walks on long histories

Status: **implemented** (tests: `crates/vcs-repo/tests/history_walks.rs`, `crates/vcs-cli/tests/history_cmds.rs`,
`vcs-merge` proptests). Design summary is in `architecture.md` ("History walks"). This page has the method and the numbers.

## What changed

| Command | Before | Now |
|---|---|---|
| `log -- <path>` | flattened the whole tree of every commit (O(commits x files)), read all history before applying `-n` | compares the entries at the path (one tree per path component), stops at the `-n`-th match, skips commits through changed-path filters |
| `log --follow`, `blame` | two full flattens per examined commit | entry lookup at the followed path; rename detection diffs only the changed directories; filters skip commits that leave the file alone |
| `log -S` | two full flattens per commit, one thread | `diff_trees` (equal subtrees not read), commits tested on all cores, a blob's occurrences counted once |
| `log --name-status`, `show` | two full flattens per commit and every changed file's content | `diff_trees`; contents are read only when rename or copy detection is asked for |
| `merge-base` (new command), merges | map-based paint-down | same algorithm on table indexes of the commit-graph |
| `describe`, `bisect` start, `HEAD~N` | decoded commit objects one by one | parents from the commit-graph; `describe` counts with index bitmaps |
| `log` of a long history | sequential object walk | order from the commit-graph, commits read on all cores |
| `diff-tree A B` (new command) | n/a | `diff_trees`: cost follows the changed directories, not the tracked files |

Caches (all pure caches: a missing, stale or corrupt file is ignored and rebuilt, results are identical with and
without them, `vcs fsck` checks them):

* `.vcs/commit-graphs/<zone>`: parents, author time, generation (already existed; now used by more commands).
* `.vcs/path-filters/<zone>` (new): per commit and parent, a Bloom filter (12 bits, 7 hashes per entry) of the changed file
  paths and their directories. Built lazily by the first long path walk (256 commits examined), stored when 16 new
  filters exist (64 once a file exists). Not built for root commits or changes of more than 512 paths.

Not done: `grep` over several revisions (the command takes one revision; its per-file work already runs on all cores),
parallel tree walking inside one `diff_trees` (the cost is dominated by object reads, see below), a cache for
`-S`/`-G` results.

## Method

* Generator: `scripts/histgen.py` writes a `git fast-import` stream; the same stream feeds `git fast-import` and
  `vcs import-git`, so git and vcs hold the same history (checked: `git log --format=%s` and `vcs log --format=%s` give the same
  3000 subjects in the same order on the small set; path-limited logs equal git's on the paths tried).
  Shape of the set measured here: 20,000 commits (about 8% start a short side branch that is merged back), a tree of
  20,000 files in 400 directories (3 levels), 1-3 files edited per commit (30% of edits go to 1% of the files), a few adds and renames,
  14 annotated tags, one file (`hot/old_name.txt`, 400 lines) edited in 5% of the commits.
* Harness: `scripts/histbench.py`. Each round runs every tool once, rotating who goes first; the first round is discarded;
  wall time is `perf_counter` around the process, CPU is user+sys of the child from `getrusage`; medians of 9 rounds.
  Page cache is warm. "git" is git 2.43.0 on a repository as `git fast-import` left it (no commit-graph file, which
  is git's default state for a fresh repository); "git+cg" is a copy after
  `git commit-graph write --reachable --changed-paths`, i.e. git with the same kind of caches vcs now has.
  "vcs" is the release build of this branch, with its caches warm (built by the first run; see the cold-start line).
* Correctness: `scripts/` has no equality oracle for output formats; instead the 3000-commit set was run through the
  previous release binary and this one for 24 commands (log with paths, `--follow`, `-S`, `--name-status`, blame, `-M`,
  describe, show, grep): identical output in each case, and `diff-tree` equals `git diff --no-renames --name-status`.

## Results

All medians of 9 rounds (7 for the 100k set), warm page cache, wall ms (CPU in the log files was within a few percent
except where noted). Machine for the 20k set: AWS 8 vCPU, **load1 15-25 during the runs (shared, not quiet)**; for the
100k set: a second 8 vCPU instance, load1 1.6-1.9.

### 20,000 commits, 20,000 files (git 2.43.0)

| Case | git | git + commit-graph --changed-paths | vcs (this branch) | vcs before (1 run) |
|---|---|---|---|---|
| `log -n20 -- file` (hot 1%) | 94 | 24 | 45 | 510,199 (8.5 min) |
| `log -n20 -- file` (cold) | 163 | 28 | 67 | not run (same algorithm) |
| `log -n20 -- dir` | 40 | 13 | 31 | not run |
| `log --follow file` | 298 | 210 | 163 | not run |
| `log -S str` | 1560 | 1409 | 1332 (CPU 6650) | not run |
| `log -n200 --name-status` | 36 | 37 | 62 | not run |
| `blame` (400 lines, 1000 edits) | 248 | 147 | 298 | not run |
| diff of two commits 10,000 apart (names) | 111 | 72 | 65 (`diff-tree`) | no such command |
| `merge-base` | 34 | 8 | 27 | not run |
| `describe` | 9 | 5 | 21 | not run |
| `grep` one revision | 207 | 206 | 71 | not run |

Honest reading: against a default git repository vcs is faster on most rows; against git with the same kind of caches
vcs is slower on `log -- path` (about 2x), `blame`, `describe`, `merge-base`, `--name-status`. `-S` wall time is below git's only
because it runs on all cores (4x the CPU). `bisect` was not timed to completion. The "before" column is the binary
at the base commit; it is O(commits x files), so only one run of the first case was taken. The other "before" cells
were not measured at this size (3000 commits / 2000 files: 10.8 s for `log -n20 -- file` before, 27 ms after, one load-14 run).

### 100,000 commits, 1,000 files (quiet machine, load1 1.6-3; medians of 7 rounds)

"vcs (flat graph)" reads the commit-graph into flat arrays (no allocation per commit); "vcs (first PR)" decoded a
vector per commit.

| Case | git | git + cg | vcs (first PR) | vcs (flat graph) |
|---|---|---|---|---|
| `log -n20 -- file` (hot 1%) | 8.1 | 7.8 | 83.9 | 65.9 |
| `log -n20 -- dir` | 7.6 | 7.5 | 86.4 | 65.6 |
| `log -n20 -- file` (cold) | 281 | 78 | 235 | 165 |
| `log` all matches (cold) | 759 | 204 | 482 | 335 |
| `describe` | 9.8 | 8.5 | 92 | 62 |
| `merge-base` | 190 | 47 | 127 | 81 |
| `log -n200 --name-status` | 13 | 15 | 88 | 66 |

Earlier run of this set (first PR build): `log --follow` git 894 / git+cg 517 / vcs 645; `log -S` git 12365 / 11945 / vcs
5680 (CPU 32,968); `blame` git 1103 / 814 / vcs 4184 (3.8x git, slower); `grep` one hit git 22 / 24 / vcs 53;
diff of commits 10,000 apart git 345 / 130 / vcs 105.

Where the short-walk floor comes from: about 41 ms of the 62-66 ms is not the walk. Any command opens the store, and
`Pack::open` reads the whole 32 MB pack index of this history (23 ms in `read`); `vcs rev-parse HEAD` alone takes 41 ms
here. The graph (about 5 ms now) and filter (about 5 ms) files are the rest. Reading the pack index on demand is a
`vcs-store` change and was not made here.

### Pack index read on demand (100k commits, quiet machine, medians of 5 starts)

A local `Pack::open` used to read the whole `.idx` (32 MB at this size) on every start. v2+ indexes are now opened by
reading the header and fanout only; a lookup reads its first-byte bucket (about 50 KB) once. Verification is
unchanged: `fsck`/`verify` still check the trailer, ordering and fanout, objects are still hash-checked on read, and a
file whose length does not match its entry count is refused at open. v1 indexes are still read and verified in full at
open (existing contract). The streaming pack writer (used by import and sync) now writes v2 packs (raw codec) so new data
gets the fanout index; existing v1 repositories get it after a repack.

| Command, 100k commits | v1 packs (before) | v2 packs, on-demand index |
|---|---|---|
| `rev-parse HEAD` | 40-50 ms | under 10 ms |
| `status` | 50 ms | under 10 ms |
| `log -n20 -- file` (hot) | 60 ms | 20 ms |
| `describe` | 60 ms | 20 ms |
| `blame` (5000-edit file) | 3.9 s | 2.9 s |

(ms are `/usr/bin/time` centiseconds, so "under 10 ms" is a bound, not a measurement; git `rev-parse` is also under 10 ms.)
`blame` at 100k is dominated by the line diff; the LCS table is now 16 bit and lines are interned (identical alignments,
tested against the old routine), which is the 3.9 s to 2.9 s. git takes about 1.1 s; still 2.6x slower.

### Known limits

* First long path walk on a history builds the filters (it reads the commits it passes); the first run on the 20k set took
  about 5 s CPU. 
* Every object read still opens the loose path before the pack and decompresses on its own; per-read cost (about 25 us) bounds `-S` and `blame`.
