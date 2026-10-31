# Interop: Git export and Perforce

Status: Git export of one zone branch is a **prototype** (`vcs export-git`, `Repo::export_git`, tested). Everything
else here, including the Perforce mapping, is **design only**; nothing in the Perforce section is built.

The core never runs `git` or links libgit2: export writes a `git fast-import` stream, import (see
[import-git.md](import-git.md)) reads a `git fast-export` stream. Only tests invoke `git`, to check the stream.

## Id mapping

The shared table is the one the importer owns: `.vcs/git-import`, one line `<our object id> <git key>`, where the key is the
Git commit id (from `--show-original-ids`) or `mark:<n>`.

| Direction | Git id of | Our id | Notes |
|---|---|---|---|
| import | commit | commit id (BLAKE3-256) | recorded in `.vcs/git-import`; `change_id` is derived from the Git key |
| import | blob, tree | not recorded | fast-export streams carry no tree ids; ours are recomputed from content |
| export | commit | not recorded yet | the Git SHA-1 exists only inside the receiving Git |
| export | `change_id` | carried in a `Vcs-Change-Id: <32 hex>` message trailer | added unless already present |

Ids are never equal across systems: ours hash `kind || version || payload` under a domain-separated BLAKE3 key, Git's hash
`type size\0payload` with SHA-1/SHA-256. They cannot be converted, only mapped.

Stable-mapping rules (design, to be implemented together with trailer import):

1. A commit with an entry in `.vcs/git-import` is exported under its original Git identity when the target repo already
   has it: the exporter would use the incremental form (`from <git sha>`) instead of re-emitting it. Today export always
   emits the whole branch, so re-exporting into the repo it came from produces *new* Git commits (different SHAs: we
   write timezone `+0000` and committer = author, which the import does not retain).
2. Export records `<our commit id> <git sha>` in a new `.vcs/git-export` table only after the receiving side reports the
   SHAs (`git fast-import --export-marks`, then `git rev-parse`); marks are not ids and are not persisted.
3. On import, a `Vcs-Change-Id` trailer overrides the derived `change_id`, so a rebased-then-exported-then-imported change
   keeps its logical identity. The importer on main does not read it yet; follow-up.

## Prototype: what export does

`vcs export-git [--zone A] [branch] [-o file]` writes `refs/heads/<branch>` of one zone:

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
| branch names (`refs/heads/*`) | tags, notes, remotes, other refs (ignored on import) |
| | symlinks, submodules, empty directories, non-representable paths (skipped on import) |
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

## Perforce changelist mapping (design only)

Nothing is implemented; no Perforce server or `p4` client was available to validate any of this.

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
