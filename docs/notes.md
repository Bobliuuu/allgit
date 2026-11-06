# Commit notes

Status: **implemented** (`crates/vcs-repo/src/notes.rs`, `crates/vcs-cli/src/notes_cmd.rs`; tests `crates/vcs-repo/tests/notes.rs`,
the `notes` scenario of `crates/vcs-cli/tests/gitmap.rs`, which also runs real git as a yardstick).

A note is a piece of text attached to a commit after the fact, without changing the commit (so its id stays the same).

```sh
vcs notes add -m "reviewed by Sam" HEAD~2       # refused when the commit already has a note, unless -f
vcs notes append -m "also benchmarked" HEAD~2   # blank line, then the new text; creates the note when missing
vcs notes show HEAD~2                           # exit 1 when there is none
vcs notes list                                  # "<note blob id> <annotated commit id>", one per note
vcs notes remove HEAD~2
```

`-m` may be repeated (paragraphs are separated by a blank line) and `-F <file>` (or `-F -`) reads the text; there is no
editor. A revision is anything `vcs tag` accepts (`HEAD`, branch, tag, id prefix, `~N`, `^`). The text is normalized like
git's `stripspace`: trailing blank lines are dropped and one final newline is kept. A note is a plain blob below 1 MiB.
`vcs git notes ...` maps the five subcommands one to one ([git-to-vcs.md](git-to-vcs.md)).

## Where notes live

Commits are per zone, so notes are too: `vcs notes` works on the zone of the current directory (`--zone Z` overrides).
Each zone keeps its notes on one internal ref, `(zone, "refs/notes/commits")`, which points at an ordinary **Commit** of
that zone. Its tree maps the annotated commit's id to the note blob with a two-hex-digit fan-out
(`<aa>/<62 hex digits>`), exactly the layout git uses, and every `add`, `append` or `remove` is a new commit on that ref,
so the history of the notes is kept. See [object-model.md](object-model.md#commit-notes).

No new object kind or format version was needed: a notes ref is a branch whose commits happen to hold annotations, so
the object store, `gc`, `fsck`, packs, bundles, the manifest and the server already handle it. The ref is not a user
branch. It is hidden from `vcs branch`, `log --all`, bundles of a zone's branches (it is added explicitly), the git remote
helper and `Refs::branches_of`, and `branch`, `switch` and `checkout` refuse the name.

| Concern | Behaviour |
|---|---|
| `publish` | A full publish also moves the notes ref of every published zone when the remote has none or an ancestor of the local one. It is skipped for a scoped publish (`publish <branch>`, `--delete`) and `--tags`, as `git push` does not push notes either. If the remote notes are not an ancestor of the local ones, the notes are **not** published; the publish still succeeds and says so on stderr (`PublishReport::notes_diverged`). |
| `fetch`, `sync`, `pull` | The remote notes ref is downloaded together with the note blobs (a lazy fetch normally stores trees only) and folded into the local ref: adopted when there is none, fast-forwarded when the local one is behind, left alone when it is ahead, otherwise merged. |
| `clone` | The notes of every cloned zone arrive with the clone, blobs included. |
| `bundle create` / `fetch` / `clone` | Notes are in a default (all branches) bundle; a zone selector adds that zone's notes. `bundle fetch` folds them into the local notes as `fetch` does. |
| `gc` | The notes ref (local and remote-tracking) is a root like any branch; the history of the notes and every blob stay. `gc --remote` sees it as a manifest branch. |
| `fsck` | Checks it like a branch: the head must be a commit of its own zone. |
| `op undo` | The operation log records every notes change, so `vcs op undo` reverts one. |

## Merging diverged notes

Two clones may annotate independently. `fetch` then records a merge commit (parents: local and remote notes) holding the
union of both trees. If both sides annotated the *same* commit with different text, the note becomes the local text, a
blank line, then the remote text. There is no per-note base, so **a note removed on one side comes back from the other**,
and a note edited on both sides keeps both versions. This is deliberately simpler than `git notes merge` (whose default
strategy refuses on such conflicts); `git notes merge` has no equivalent command.

## Limits (also in [known-limits.md](known-limits.md))

* Notes do not follow a commit that is rewritten: after `commit --amend`, `rebase`, `cherry-pick` or `zone apply` the new
  commit has no note (git's `notes.rewriteRef` is not implemented). The old note stays on the old commit.
* `vcs log` and `vcs show` do not print notes; use `vcs notes show`.
* One notes ref per zone: no `--ref`, no `GIT_NOTES_REF`, no `notes.displayRef`.
* No `notes edit` (no editor), `copy`, `merge` or `prune`; notes of unreachable commits are kept until removed.
* The merge on `fetch` cannot propagate deletions, see above.
* `vcs notes list` prints BLAKE3 ids, not SHA-1 ones.
* The notes tree uses a flat two-level fan-out: adding a note rewrites one 256-way top tree and one subtree. That is fine
  for tens of thousands of notes; it was not measured at millions.
* The `post-publish` hook sees the notes ref as an ordinary updated branch (`<zone> refs/notes/commits ...`).
