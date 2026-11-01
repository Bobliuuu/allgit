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

### 100,000 commits, 1,000 files (quiet machine, partial: the run was cut when this doc was written)

| Case | git | git + cg | vcs |
|---|---|---|---|
| `log -n20 -- file` (hot 1%) | 7.9 | 6.4 | 83.6 |
| `log -n20 -- dir` | 7.7 | 6.1 | 82.2 |
| `log -n20 -- file` (cold) | 282 | 70 | 222 |
| `log` all matches (cold) | 757 | 192 | 483 |
| `log --follow` | 894 | 517 | 645 |

On a short walk at 100k commits vcs is about 10x slower than git: loading the 100k-entry commit-graph and
filter files (decode and verify, no mmap) costs about 80 ms per run. A fixed-layout file read in place would remove it;
not done. Remaining 100k rows were not measured.

### Known limits

* First long path walk on a history builds the filters (it reads the commits it passes); the first run on the 20k set took
  about 5 s CPU. 
* Every object read still opens the loose path before the pack and decompresses on its own; per-read cost (about 25 us) bounds `-S` and `blame`.
