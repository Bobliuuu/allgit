# Research: fast paths in Git, Sapling, Jujutsu and Mercurial

Issue #97. Date accessed for every source: **2026-10-31**. Status of claims: **[src]** stated by a fetched primary page,
**[ours]** measured in this repo (docs/benchmarks.md), **[unverified]** not confirmed from a fetched source or only from
a secondary summary. This doc contains no performance numbers about other tools, because none of the pages fetched gave
any; the only numbers are our own, copied from docs/benchmarks.md.

## Where we are

[ours] Per docs/benchmarks.md (Apple M2 Pro, APFS, warm cache, criterion `--quick`, release): after #21/#22/#35 work,
`status` on a clean 10k-file `deep` tree took 674 ms versus 33 ms on `flat`; with one staged change `status` on `deep`
still took about 3 s because HEAD is flattened. The remaining cost is one `read_dir` per directory (`walk_untracked`) and
one object read per HEAD tree. `log` is linear in commits walked (about 14 ms per 1,000). `status` never rewrites the
index, so racy entries stay re-hashed. 100k-file numbers do not exist yet (#104).

## Techniques surveyed

### 1. Filesystem monitors (Watchman, FSEvents, inotify)
- [src] Git's built-in `fsmonitor--daemon` "watch[es] the working directory ... using platform-specific filesystem notification
  facilities" so `git status` can "avoid scanning the disk"; clients talk to it over a simple IPC interface; enabled by
  `core.fsmonitor=true`. On Linux it uses inotify, which has per-user watch limits (the page gives a default of 8192 and
  says to raise `fs.inotify.max_user_watches`). It refuses network-mounted repos unless `fsmonitor.allowRemote` is set
  (experimental), and does not know about submodules. https://git-scm.com/docs/git-fsmonitor--daemon
- [src] Git stores the monitor result in the index as the `FSMN` extension; version 2 holds an opaque token defined by the
  monitor plus a bitmap of entries not marked fsmonitor-valid. https://git-scm.com/docs/index-format
- [src] Git docs: the feature "enables git to avoid having to lstat() every file", and combined with the untracked cache
  avoids scanning the whole tree for new files. https://git-scm.com/docs/git-update-index
- [src] Sapling's scale page lists "File system monitor for faster working copy status (watchman, 2014)" and EdenFS
  ("Virtualized working copy for on-demand ... fetching", 2018). https://sapling-scm.com/docs/scale/overview
- [src] Jujutsu exposes `fsmonitor.backend` with values `"none"` or `"watchman"`, plus an optional
  `fsmonitor.watchman.register-snapshot-trigger`. http://docs.jj-vcs.dev/latest/config/
- [unverified] Watchman's own docs (facebook.github.io/watchman) returned 404 when fetched; its inotify/FSEvents use is
  not confirmed here. A search summary also repeated a claim that EdenFS cuts checkout from hours to seconds; no primary
  source found, not used.
- Fit: needs a token + changed-path contract in our index; correctness must never depend on the monitor (fall back to full
  scan on any doubt). Mac is our dev platform, FSEvents available through the `notify` crate [unverified: not fetched].

### 2. Index formats, mmap, parallel load
- [src] Git index v2 stores full paths; v3 adds an extended-flags word (skip-worktree, intent-to-add); v4 prefix-compresses
  paths against the previous entry. Extensions `EOIE` (offset of the end of entries) and `IEOT` (offset/count table) "so
  entry conversion from disk to memory can be split across threads"; `link` (split index) keeps most entries in a shared
  file; `sdir` for sparse directory entries. https://git-scm.com/docs/index-format
- [src] Mercurial's tree-shaped Rust dirstate mirrors the working directory so status can walk dirstate and filesystem in
  tandem; the dirstate-v2 file uses the same tree shape and "can access a dirstate-v2 file without parsing it", making map
  creation O(1). Source: Mercurial phabricator revision D11516 (2021), found via search, page not fetched in full:
  https://phab.mercurial-scm.org/D11516 [unverified beyond the search summary]. The mmap detail is [unverified]; the
  cached-directory-mtime part of dirstate-v2 could not be found.
- Fit: our index is a single varint-framed file parsed whole. We have no measurement of load cost, so payoff is unknown
  (issue #112 starts with measuring).

### 3. Directory-level hash caches (cache-tree)
- [src] Git's `TREE` extension "stores a recursive tree structure of existing tree objects that match unchanged sections of
  the index", speeding up tree writing for a commit and letting comparisons with `HEAD^{tree}` skip equal sections;
  updating a path invalidates its parent directory nodes. https://git-scm.com/docs/index-format
- Fit: excellent. Our trees are Merkle and the benchmarks already show tree rebuild and HEAD flattening as the staged-case
  cost. (#110)

### 4. Untracked cache
- [src] Records directory mtimes and omits reading directories (and stat of files in them) whose mtime has not changed; it
  requires the OS and filesystem to update directory `st_mtime` when entries are added, modified or deleted; testable with
  `--test-untracked-cache`. Before Git 2.17 it had a bug showing tracked files as untracked after a directory was replaced
  with a symlink. https://git-scm.com/docs/git-update-index
- Fit: directly targets our remaining `walk_untracked` cost. Risk: mtime granularity and filesystems that do not update
  directory mtimes; needs the same racy-window discipline as file stats. Already scoped in #102.

### 5. Commit-graph with generation numbers
- [src] The commit-graph is "a supplemental data structure that accelerates commit graph walks"; it avoids decompressing
  and parsing commits and walking the whole graph for topological order. Stores OID, parents (as integer positions), commit
  date, root tree and generation number. Property: if generation(A) <= generation(B) then A cannot reach B, so walks stop
  early. Two definitions: topological level (v1) and corrected committer date (v2). Large histories use a chain of files.
  Not read when grafts, replace objects or shallow commits exist. https://git-scm.com/docs/commit-graph
- [src] `git maintenance` updates it incrementally and verifies it (`commit-graph` task); `maintenance.commit-graph.auto`
  default 100. https://git-scm.com/docs/git-maintenance
- Fit: good; ours would be per zone, derived data, rebuildable. Scoped in #103.

### 6. Bloom filters for path history
- [src] Optional `BIDX`/`BDAT` chunks hold per-commit Bloom filters of paths changed against the first parent; Git currently
  supports hash version 2 (32-bit murmur3 with double hashing); commits with no changes or more than 512 changes get a
  length-one filter (all zero or all one bits). The page does not state default hash count or bits per entry.
  https://git-scm.com/docs/gitformat-commit-graph
- Fit: depends on the commit-graph. Pure accelerator (false positives only), so low correctness risk. (#114)

### 7. Parallel hashing and stat
- [ours] `add` already stages files in parallel (up to 8 threads) and `walk_untracked` walks sibling directories on extra
  threads. Git has `core.preloadIndex` and `index.threads`; the fetch of git-config returned only the first 100k
  characters and did not reach those entries, so their exact semantics are [unverified] here
  (https://git-scm.com/docs/git-config).
- Fit: largely done; remaining gains are in avoiding work (items 3, 4, 1), not more threads.

### 8. Others noticed
- Jujutsu snapshots the working copy automatically before most commands ("Jujutsu will automatically create commits from the
  working-copy contents when they have changed"), which makes snapshot cost the hot path; it also has `snapshot.max-new-file-size`
  (default 1MiB) as a guard. https://docs.jj-vcs.dev/latest/working-copy/ and http://docs.jj-vcs.dev/latest/config/
- Git `feature.manyFiles` bundles settings for large trees (index v4, untracked cache); exact contents [unverified]
  (not reached in the git-config fetch).

## Ranked table

Ranking is the author's judgment from the evidence above and our measurements, not a measured result. Payoff is relative
(H/M/L) and tied to our known costs; cost is engineering effort.

| Rank | Idea | Payoff | Cost | Risk | Fits our object model? | Issue |
|---|---|---|---|---|---|---|
| 1 | Subtree-skipping diff and cache-tree | H: removes the ~3 s staged `deep` status and tree rebuild [ours] | M | Low (id equality is exact) | Yes, native Merkle | #110 |
| 2 | Directory mtime cache / untracked cache | H: removes per-directory `read_dir` | M | Medium (mtime semantics, racy window) | Yes, stored in index | #102 (existing) |
| 3 | Racy-entry refresh in status | M for the first status after `add` | S | Low-medium (index write under lock) | Yes | #111 |
| 4 | Commit-graph + generation numbers | M-H for log/merge-base on long history | M | Low (derived, rebuildable) | Yes, per zone | #103 (existing) |
| 5 | Filesystem monitor token interface | H at 100k+ files, unproven here | L | Medium-high (missed events = wrong status) | Yes if fallback-to-scan is absolute | #113 |
| 6 | Index format (lazy/parallel/prefix) | Unknown until measured | M | Medium (format migration) | Yes | #112 |
| 7 | Changed-path Bloom filters | M for `log -- path` | M | Low | Yes, on top of 4 | #114 |
| 8 | More parallel hashing | L (already parallel) | S | Low | Yes | none |

Top 5 follow-ups opened as new issues: #110, #111, #112, #113, #114. #102 and #103 already existed and cover ranks 2 and 4;
this research endorses them. Rank 5 vs 6 order is weak: neither has our own 100k data (#104 should run first).

## What we could not verify
- Any speedup figure for Git, Sapling, Jujutsu, Mercurial or Watchman: none fetched, none quoted.
- Watchman internals, EdenFS claims, Mercurial cached directory mtimes, git-config entries for `core.preloadIndex`,
  `index.threads`, `feature.manyFiles`.
- Behaviour on Linux and on non-APFS filesystems (our benchmarks are macOS/APFS only).

## Sources (all accessed 2026-10-31)
- https://git-scm.com/docs/git-fsmonitor--daemon
- https://git-scm.com/docs/index-format
- https://git-scm.com/docs/git-update-index
- https://git-scm.com/docs/commit-graph
- https://git-scm.com/docs/gitformat-commit-graph
- https://git-scm.com/docs/git-maintenance
- https://git-scm.com/docs/git-config (partially read)
- https://sapling-scm.com/docs/scale/overview
- http://docs.jj-vcs.dev/latest/config/ (first 100k characters)
- https://docs.jj-vcs.dev/latest/working-copy/
- https://phab.mercurial-scm.org/D11516 (via search result summary only)
- https://github.blog/open-source/git/highlights-from-git-2-37/ : says the built-in monitor ships on Windows and macOS in 2.37 and gives no numbers.
