# Interop: Git export and Perforce

Status: Git export of one zone branch (with tags, symlinks and incremental re-export) is a **prototype** (`vcs export-git`,
`Repo::export_git`, tested). Perforce import is a fixture-tested prototype ([import-p4.md](import-p4.md)). Everything else
here is **design only**.

The core never runs `git` or links libgit2: export writes a `git fast-import` stream, import (see
[import-git.md](import-git.md)) reads a `git fast-export` stream. Only tests invoke `git`, to check the stream.

## Id mapping

The shared table is the one the importer owns: `.vcs/git-import`, one line `<our object id> <git key>`, where the key is the
Git commit id (from `--show-original-ids`) or `mark:<n>`.

| Direction | Git id of | Our id | Notes |
|---|---|---|---|
| import | commit | commit id (BLAKE3-256) | recorded in `.vcs/git-import`; `change_id` is derived from the Git key |
| import | blob, tree | not recorded | fast-export streams carry no tree ids; ours are recomputed from content |
| export | commit | recorded as `mark:<n>` in `.vcs/git-export` | the stream refers to old commits by mark; the SHA-1 exists only inside the receiving Git |
| export | `change_id` | carried in a `Vcs-Change-Id: <32 hex>` message trailer | added unless already present |

Ids are never equal across systems: ours hash `kind || version || payload` under a domain-separated BLAKE3 key, Git's hash
`type size\0payload` with SHA-1/SHA-256. They cannot be converted, only mapped.

Incremental export (implemented):

1. `vcs export-git` records `<our commit id> mark:<n>` in `.vcs/git-export` for every commit it writes, and on the next run
   skips those commits: new commits reference their parent as `from :<n>` and marks continue after the highest recorded
   one. The receiving Git must keep its marks: `git fast-import --export-marks=M --import-marks-if-exists=M`. Then old
   commits keep their SHAs (tested with a real git: `main~1` is unchanged after the second import). One table belongs to
   one target repository; use `--full` to export everything again and reset the table.
2. `--onto-origin` additionally treats commits that came from `import-git` as already present in the target and refers to
   them by their original SHA (`from <sha>`), for exporting back into the repository they were imported from.
   Not tested against a real round trip of an imported repository.
3. On import, a `Vcs-Change-Id` trailer overrides the derived `change_id` (the remote helper relies on it).

## Git remote helper: `git-remote-allgit`

Status: prototype, tested against a real `git` (`crates/vcs-cli/tests/git_remote.rs`, skipped when git is absent).

`cargo build` produces a second binary, `git-remote-allgit`. Put it on `PATH` and Git can talk to an allgit repository:

```
git clone allgit::/path/to/allgit-repo work
cd work && git commit ... && git push origin HEAD:refs/heads/main
git pull
```

The helper is driven by Git and never runs `git` or links libgit2. Fetching uses the `import` capability: the helper writes a
fast-import stream with `Repo::export_git_ext`, Git's own `fast-import` stores it. Pushing uses `export`: Git's `fast-export`
stream is read by `Repo::import_git_with`. Remote branches are mapped to `refs/allgit/<remote>/heads/*` and tags to
`refs/allgit/<remote>/tags/*` (the `refspec` capability); `list` reports `?` for every ref.

State is kept per remote in `$GIT_DIR/allgit/<remote>/`: `marks` (shared fast-import/fast-export marks file, handled by Git
through `*import-marks` / `*export-marks`), `pending` (objects whose Git sha is not yet known) and `map` (allgit id <-> Git sha
for commits and blobs). The marks file is written by Git after the helper may have exited, so `pending` is resolved on the
next run. `map` is what lets a pushed commit keep the sha Git gave it: later fetches refer to it by sha (`from <sha>`) instead
of re-creating it with a `Vcs-Change-Id` trailer. The importer now also honours a `Vcs-Change-Id:` trailer (last line of the
message) as the commit's change id.

What works (tested): clone, fetch/pull, push of new commits (fast-forward only) to existing or new branches, including the
checked-out branch of a non-bare allgit repository when its working tree is clean, executable bits, symlinks, pushed
annotated and lightweight tags, a second incremental push, a fresh clone seeing pushed history, per-ref errors
(`! [remote rejected]`) for non-fast-forward pushes, ref deletion and a non-repository URL.

Limits:

* **Zones**: only the root zone is mapped. Other zones are invisible to Git, and a push can only change the root zone.
* **Remote kind**: local paths (or `file://`) only; no HTTP/S3/vcs-server remotes.
* **History rewriting**: force pushes that rewrite allgit history are rejected (non-fast-forward is reported per ref); ref
  deletion is refused. A branch whose Git history was rewritten locally must be pushed under a new name.
* **Checked-out branch**: pushing to it works only when the allgit working tree is clean, otherwise the ref is rejected.
* **Tags**: annotated allgit tags reach Git as lightweight tags (Git cannot write annotated tags into a private namespace
  from a stream); annotated and lightweight Git tags can be pushed. A tag on a commit that is on no branch is not exported.
* **Large files**: blobs are held in memory on both sides (data blocks up to 1 GiB in the importer); there is no chunking,
  LFS or partial clone. Every fetch re-exports every branch (Git de-duplicates by content), so cost grows with history.
* **Lossy mapping**: as in the round-trip table above (committer, timezones, signatures, submodules, file modes, empty
  directories). Authors keep name, email and timestamp; the committer is dropped.
* **Marks**: a push whose negative revisions are not in the marks file (for example after deleting `$GIT_DIR/allgit`) arrives
  as a root commit instead of continuing the branch; keep the directory.
* **Errors**: a malformed stream aborts the push with a message on stderr and a non-zero exit, not per-ref status lines.
* Not tested: Windows, SHA-256 repositories, shallow clones, concurrent pushes from several clones.

## Prototype: what export does

`vcs export-git [--zone A] [branch] [-o file]` writes `refs/heads/<branch>` of one zone:

* Symlinks (`120000`) and the zone's tags are exported: lightweight tags as `reset refs/tags/<n>` + `from`, annotated tags
  as `tag` (tagger, message; the signature field is dropped). A tag whose commit is not in the exported branch, or whose name is
  not a valid Git ref, is skipped and reported. Tags are re-emitted on every run (same content, so same Git ids).
* Commits parents-first, deterministic (same repo state gives byte-identical output; tested). Merges use `from` +
  `merge`; file changes are diffed against the first parent (`M`/`D`).
* Each distinct blob is emitted once, at first use (tested: two identical files produce one blob).
* Paths are **zone-relative**: a zone becomes a standalone Git repository. Exporting the whole repo as one Git history
  (zones as top-level directories) needs a synthetic linearisation and is not designed here.
* Executable bit maps to `100755`. Author and committer are the commit author, timezone `+0000`. Names containing `<`,
  `>` or newline are sanitised to spaces.
* Branch names that are not valid Git refs are refused, not rewritten.
* Verified with a real `git fast-import` (when git is installed) followed by `git fsck --strict`, `git log`, `git show`
  and `git ls-tree`. Not verified: large repositories, non-UTF-8 content in messages, other importers.

## Round trip: what survives

Git -> vcs -> Git (via `import-git` then `export-git` of the root zone):

| Survives | Does not survive |
|---|---|
| file contents, paths, executable bit | Git commit SHAs (rewritten; see rule 1) |
| first-parent order and merge topology | committer identity and author/committer timezones |
| author name, email, timestamp, message | message gains a `Vcs-Change-Id` trailer |
| branch names (`refs/heads/*`), symlinks, lightweight and annotated tags | notes, remotes, other refs (ignored on import) |
| | submodules (reported and skipped on import), empty directories, non-representable paths |
| | file modes other than 644/755, `.gitattributes`-driven filters (content is bytes as stored) |
| | octopus-merge parent order beyond what `merge` lines preserve; signed commits/tags (signatures dropped) |

vcs -> Git -> vcs (export then import, re-using the trailer once implemented): commits, trees and blobs reproduce the
same object ids only if author, message (including the trailer) and timestamps map back byte-for-byte; the zone name
is not stored in Git, so everything lands in the `root` zone. Ids of the commits differ from the originals whenever the
trailer was appended.

Lossy by design, and by whom:

| vcs concept | In Git | Consequence |
|---|---|---|
| **Zone** | nothing; only one zone is exported, zone-relative | other zones, and each zone's independent history, are absent |
| **Transaction** (atomic multi-zone publish) | nothing | which commits moved together is lost; could be rendered as a shared `Vcs-Transaction:` trailer (design, not built) |
| **Snapshot** (pinned set of zone versions) | could map to a lightweight tag per zone | not exported |
| **Operation log / undo** | nothing | local history of the tool, not project history |
| `change_id` | trailer only | survives only through the trailer |
| Locks, leases | nothing | advisory in Git; see [locks.md](locks.md) |
| Conflict objects (designed, M4) | nothing | would have to be exported resolved or refused |

## Adoption path (suggested)

1. Import a Git repo into the root zone; teams keep pushing to Git as the source of truth.
2. Periodically `export-git` a branch into a Git mirror for CI and tooling that only speaks Git.
3. Flip the source of truth to vcs once zones are split. After that, export is one-way; Git-side commits come back only by
   re-import into a new branch.

Not designed: bidirectional continuous sync, conflict handling when both sides moved, and large-history performance.
No numbers are claimed; nothing here was benchmarked.

## Perforce changelist mapping

A fixture-driven **import** prototype exists (`vcs import-p4`, see [import-p4.md](import-p4.md)): changelists become commits
in the root zone, with a view file for path mapping and a `Perforce-Change: N` trailer. It is tested only against
hand-written fixtures and has never been run against a real server or `p4` output. The table below is the wider design;
rows other than add/edit/delete/move, `+x`/symlink types and the id table are not built. Export to Perforce is not built.

| Perforce | vcs |
|---|---|
| depot path `//depot/proj/...` | an **zone** (one depot subtree per zone; path prefix = zone path) |
| stream / branch spec | branch of that zone |
| submitted changelist N | one Commit per touched zone, grouped by one **Transaction** (a changelist is atomic across paths, as a Transaction is across zones) |
| pending (numbered) changelist | an unpublished set of staged changes in the workspace (no object until submit) |
| changelist description / user / date | commit message / author / timestamp (client workspace name dropped) |
| revision `#rev` of a file | the blob at that commit; per-file rev numbers are not stored, recoverable from history |
| `edit`/`add`/`delete`/`move` | tree diff; `move/add` + `move/delete` pair becomes a rename recorded as delete+add (renames are not tracked in the object model) |
| file type `+x` | Executable; other types (`+l` symlink, `text+k` keyword expansion, `apple`, `utf16`) are unsupported or must be refused/normalised, listed in the import report |
| `p4 lock` / exclusive-open (`+l`) | vcs locks; only meaningful when server enforced (see locks.md), otherwise advisory |
| `p4 integrate` / `resolve` | a merge commit when resolved; Perforce integration records do not map one to one |
| labels / `p4 label` | Snapshot (named, pinned) |

Id mapping: a table `.vcs/p4-import` of `<our commit id> p4:<changelist>:<zone>`; changelist numbers are global and
monotonic, so re-import is "all changelists above the highest recorded". The transaction id is derived from the changelist
number the way the Git importer derives `change_id`, so re-imports are idempotent.

Open questions: obliterate and changelist renumbering (history rewritten server-side invalidates the table); whether to
read via `p4 -G` marshalled output or a checkpoint/journal dump (the latter avoids spawning `p4` from core); handling of
huge binaries (needs the M5 chunking). Export to Perforce is out of scope until a use case exists.
