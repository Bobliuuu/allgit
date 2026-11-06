# rerere: reuse recorded conflict resolutions

Status: **implemented** (`vcs-repo::rerere`, `vcs rerere`, config `rerere.enabled`).

When the same conflict comes up again, for example when you rebase a topic branch several times or
merge the same two branches after a `reset`, vcs can apply the resolution you wrote the first time.
Same idea and same observable behaviour as `git rerere`.

## Turning it on

```
vcs config set rerere.enabled on          # this repository (use --user for every repository)
vcs config set rerere.autoUpdate on       # also stage a replayed file (default off)
```

`rerere.enabled` unset means "on exactly when `.vcs/rr-cache` exists", as in git. Off by default:
nothing is recorded and nothing is replayed. `true`/`false`/`yes`/`no`/`1`/`0` are accepted too.
Environment: `VCS_RERERE_ENABLED`, `VCS_RERERE_AUTOUPDATE`.

## What happens

1. `vcs merge`, `vcs cherry-pick`, `vcs revert` or `vcs rebase` stops on a text conflict. For every
   conflicted file vcs writes `.vcs/rr-cache/<id>/preimage` (the file with its conflict markers) and
   remembers which path has which id in `.vcs/rr-merge` (git's `MERGE_RR`).
2. You resolve the file and `vcs add` it (or `vcs resolve --ours|--theirs|--file`). The resolved file
   is stored as `postimage` ("Recorded resolution for 'f.txt'."). `vcs rerere` with no argument does
   the same without `add`.
3. The next time the same conflict appears, vcs merges your change *preimage to postimage* into the
   fresh conflicted file and writes the result into the working tree ("Resolved 'f.txt' using
   previous resolution."). The path is still conflicted until you `vcs add` it (or until
   `rerere.autoUpdate` stages it), so you can look first. A replay that would itself conflict is not
   applied; the file keeps its markers.

The conflict id hashes the conflicting hunks only, with the two sides in byte order and the diff3 base
section left out. So the id does not depend on marker labels, on which side is "ours", or on the rest
of the file: a resolution recorded by `merge topic` on `main` also serves `merge main` on `topic` and
`rebase main` on `topic`, and edits elsewhere in the file carry over because the replay is a
three-way merge of the whole file.

## Commands

| Command | Meaning |
|---|---|
| `vcs rerere` | record the resolution of every tracked file whose markers are gone; replay recorded resolutions into files that still have markers |
| `vcs rerere status` | conflicted paths rerere is tracking: preimage saved, no resolution to replay yet (a path whose resolution was replayed drops off, as in git) |
| `vcs rerere remaining` | conflicted paths rerere did not resolve (hand edit needed; binary and delete/modify conflicts always show here) |
| `vcs rerere diff` | unified diff of each tracked file: recorded preimage against the file now (marker labels are ignored). The header is `diff --vcs a/f b/f`, not git's |
| `vcs rerere forget <path>...` | drop the recorded resolution for the conflict in progress at those paths and restore its preimage; works after replay and after `add`, until the merge is committed. The working file is not touched |
| `vcs rerere clear` | drop the unresolved preimages of the conflict in progress and stop tracking it; recorded resolutions stay |
| `vcs rerere gc` | delete resolutions unused for `gc.rerereResolved` days (60) and conflicts never resolved for `gc.rerereUnresolved` days (15). Age is the file mtime; replaying a resolution refreshes it |

`vcs git rerere ...` translates to the same commands (docs/git-to-vcs.md).

## Limits

* Text conflicts only. A binary file, a delete/modify or add/add conflict without markers is never
  recorded.
* A file whose conflict markers are nested or unterminated is not tracked.
* An existing resolution is not overwritten by a later one; use `vcs rerere forget`, resolve again.
* The conflict id is blake3 based, not git's SHA-1, so a git `rr-cache` cannot be shared with `.vcs`.
  Behaviour is compared with real git in `crates/vcs-cli/tests/rerere.rs` (merge, swapped sides,
  rebase, forget).
* `stash apply`, `vcs apply -3` and `am -3` conflicts are not recorded; only merge, cherry-pick,
  revert and rebase are.
* `git rerere train` has no equivalent.
