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

RESULTS_PLACEHOLDER
