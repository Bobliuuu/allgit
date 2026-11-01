# Path-level sparse sets inside a zone

Implements stage 2 of [research/virtual-fs.md](research/virtual-fs.md) (issue #118).

A workspace may materialize only some path prefixes of a subscribed zone. Everything else stays tracked and
committed; it is just not on disk.

```
vcs sparse set <zone> <path>...     # materialize only these directories/files (replaces the set)
vcs sparse add <zone> <path>...     # also materialize these
vcs sparse remove <zone> <path>...  # stop materializing listed paths (files leave the working tree)
vcs sparse list
vcs sparse disable <zone>           # materialize the whole zone again
```

Paths are repo-relative (the CLI resolves them against the current directory) and must lie inside the zone. The
zone must have no uncommitted changes and no merge/rebase in progress when the set changes. `status` prints the
active sets ("other paths not materialized, NOT deleted").

## Design

* **Storage.** `.vcs/workspace` keeps the set per zone. Magic `AGWS` v1 is the legacy form (subscriptions only);
  it is still read, and still written while no zone is sparse, so older binaries keep working. `AGW2` v1 appends
  the sets. A truncated `AGW2` file is an error, never silently read as legacy.
* **Index.** The index keeps an entry for *every* tracked path of the zone, taken from the branch head's tree.
  Entries outside the set carry an empty stat (size 0, mtime 0) and are never compared with the disk. Consequences:
  * `status` skips them for unstaged changes (they are not deletions) and the staged diff is unchanged;
  * `commit` builds the tree from the index, so an unmaterialized subtree is spliced in by id from the parent
    unchanged (it is never read);
  * checkout, sync, merge, rebase, cherry-pick/revert, stash and undo move such paths in the index only. No blob
    is needed for them.
* **Membership** is by path component: a prefix `a/d` covers `a/d` and `a/d/...`, not `a/dx`.

## Behaviour per operation

| Operation | Behaviour |
|---|---|
| `status`, `diff` | only materialized paths are compared with the disk; the rest is never reported as deleted |
| `add`, `rm`, `mv`, `restore` | refuse paths outside the set with a message naming `vcs sparse add`; `add .`/`rm`/`restore` on a directory silently skip the unmaterialized part and never stage it as a deletion |
| `commit` | unmaterialized paths carry over from the parent tree unchanged |
| branch switch, fast-forward, `sync`, snapshot checkout | the new tree's materialized paths are written, the others change only in the index; an untracked file in the way of a *materialized* path is refused as before |
| `merge`, `rebase` (tested); `cherry-pick`, `revert` (same check, no test yet) | a clean change to an unmaterialized path applies in the index and the merge/rebased commit (nothing is written). **A conflict on an unmaterialized path fails before anything changes** with a message listing the paths; `vcs sparse add <zone> <path>` and retry. (Decision: failing is safer than auto-widening the set behind the user's back, and conflict markers cannot be resolved in a file that is not there.) |
| `stash push` | stashes materialized work only; unmaterialized paths are recorded as unchanged |
| `stash pop` | refused (entry kept) if the stash changes a path outside this workspace's set |
| `undo` / `op restore` | a zone that stays subscribed keeps its set (implemented, no test yet) |
| `zone remove`/`delete` | the set is dropped; re-subscribing materializes the whole zone |
| `sparse set/add` | preflight (untracked files in the way) before any write; with a remote configured only the blobs of the joining paths are fetched, in one batch (`Repo::prefetch_ids`) |
| `sync`, `zone add` | `prefetch_zone_files` fetches only the blobs of materialized paths, and does not read trees outside the set |

Order of a set change: files leaving the set are removed first, then `.vcs/workspace` is saved, then joining files
are written. A crash in between shows the missing files as deletions (`vcs restore <path>` writes them); it never
loses data, because only clean files are removed.

## Limits (not done)

* Trees are still fetched and the index still holds all tracked paths of the zone (decision D9: whole-tree per
  zone). Sparse sets save disk and blob traffic, not index size or tree traffic.
* `clone` has no `--sparse` yet (set the paths after cloning; a clone fetches the default branch's blobs first).
* The set is flat prefixes, not gitignore-style patterns.

## Tests

`crates/vcs-repo/tests/sparse.rs`: per-operation tests, workspace format (legacy read, truncation), lazy fetch
(only the needed blobs are stored), and a randomized equivalence test: the same random sequence of edits, branch
switches and merges is run on a full checkout and on a sparse checkout; every branch must end with an identical
tree and the sparse working tree must equal the full one restricted to the set.
