# git to vcs: one command for each git command

vcs is deliberately not git-format-compatible (different object ids, pack format and index), so it
can be as fast as possible. What it keeps is the muscle memory: every everyday git command has a
vcs equivalent, and `vcs git <git-args>` runs the git spelling directly.

```sh
vcs git log --oneline -5      # same as: vcs log -n 5 --oneline
vcs git commit -am "fix"      # same as: vcs add <tracked changes> ; vcs commit -m fix
vcs git checkout -b topic     # same as: vcs switch -c topic
```

`vcs git` parses the git arguments, rewrites them to native vcs commands and runs them in the same
process. It uses no git binary and no libgit2, and adds only an argument rewrite to the native
command (no extra process, no extra repository scan, except where the table says it composes two
native steps). When git has a command or flag vcs does not, it stops with
`git <cmd>: no vcs equivalent (...)` and names the native way to get there. Where the result is
close to but not the same as git, it prints a `vcs git: note:` line on stderr (silence with
`VCS_GIT_QUIET=1`) and the table row says how it differs.

## Installing the git spelling

* Shell alias (nothing to build): `alias git='vcs git'` in `~/.zshrc` or `~/.bashrc`. A git repository
  and a vcs repository then need different commands; use `command git` for real git.
* Symlink: `ln -s "$(command -v vcs)" ~/bin/git`. A `vcs` started under the name `git` behaves as
  `vcs git`. Make sure `~/bin` comes first in `PATH`.
* Shim binary: `cargo install --path crates/vcs-cli --features git-shim` also installs a `git`
  executable that runs the `vcs` next to it with `git` as the first argument. Off by default.

None of these detects whether the directory is a vcs or a git repository; they always answer with vcs.

## Native additions that came with this map

`vcs reset [--soft|--mixed|--hard] [rev] [-- paths]`, `vcs clean -n|-f [-d]`,
`vcs stash apply`, `vcs switch -c`, and `~N`/`^` revisions for `cherry-pick` and `revert`.

Merge shapes and forced publishes are native first, and the git spellings only rewrite to them:

* `vcs merge --no-ff | --squash | --ff-only [-m MSG] <branch>`. `--squash` stages the merged
  result (index and files) and leaves the branch where it is; record it with `vcs commit`. A squash
  that would conflict is refused with nothing changed. `--ff-only` refuses (nothing changed) when
  the branches have diverged.
* `vcs publish [BRANCH...] --force | --force-with-lease[=BRANCH[:ID]] | --force-without-lease`
  and `vcs publish --delete BRANCH`. There is no blind overwrite. A forced publish is one
  compare-and-swap of the remote branch head inside the same atomic manifest transaction as any
  other publish. `--force` (and bare `--force-with-lease`) leases against the head you last
  fetched: if someone moved the remote branch since, the publish fails and nothing changes.
  `--force-with-lease=BRANCH:ID` leases against a head you name (a prefix of its id is enough).
  `--force-without-lease` replaces whatever head the remote has when the publish reads it; it is
  still one atomic swap, only not tied to what you last fetched, and is not reachable from the git
  spelling. `--delete` is leased the same way.
* Every forced update and deletion is recorded: in the remote transaction (the replaced head is
  its `old` value, which remote garbage collection keeps as a root for the retention window), in
  your operation log (`vcs op log` names the full replaced id), as a local transaction object (a
  local GC root), and, on `vcs-server`, as `FORCE` / `DELETE-BRANCH` lines in the audit log.
  To get a replaced head back: `vcs branch rescue --at <id>` (the id is printed by the publish and
  is in the op log). A head you never fetched (`--force-without-lease`) can only be recovered
  from the remote's transaction log while it is retained.
* Server rules still apply to a forced update: `require-signed` checks the commits new to the
  remote, locks check the paths that change, and the new `protect-branch <zone|*> <glob>` policy
  rule refuses any non-fast-forward move and any deletion of matching branches.

History and refs (native commands first, `vcs git` only spells them the git way):

* Ranges: `vcs log` and `vcs shortlog` take `A..B`, `A...B`, `^A B`, `--not A` (also `--not REV`),
  and `vcs rev-list [--count] [-n N] [--reverse] [--all]` lists commit ids. A range is a set found
  on the commit-graph by generation number (the walk stops when no commit that could still belong
  to the answer is waiting), listed in `log` order (author time, newest first). Without the cache
  the same set is computed from the commit objects (`.vcs/commit-graphs` is only a cache). A range
  lives inside one zone: every revision in it is resolved in that zone. An empty range prints
  nothing. `^A` alone is empty, as in git. `vcs git diff A...B` diffs the merge base against B.
* `vcs log --all`: every branch head, every tag (peeled) and a detached HEAD of the zone you are in.
  Remote-tracking branches are not included. `vcs log --all-zones` does the same for every zone and
  interleaves the commits newest first (each commit line carries its zone).
* Detached HEAD: `vcs switch --detach <rev>` (`<rev>`: tag, commit-id prefix, `HEAD~2`, branch) acts
  on the zone you are in; other zones stay on their branches. The commit lives in a hidden per-zone
  ref, so `commit`, `merge`, `reset`, `status`, `diff` and `gc` treat it like the checked-out branch,
  `vcs status` prints `HEAD detached at <id>`, the ref is never published, bundled or listed as a
  branch, and `vcs op log`/`op undo` record and restore it. `vcs switch <branch>` leaves it; commits
  that no branch or tag reaches are listed first with the rescue (`vcs branch <name> <id>` before
  leaving, or `vcs op undo` after). `vcs switch -c <name>` and `vcs branch <name>` keep them. Only one
  worktree can be detached in a given zone at a time.
* `vcs branch -m [<old>] <new>` / `-M` renames (refs and workspace move, no file is touched; refused
  while a merge or rebase is in progress; recorded in the op log); `vcs branch <name> <rev>` creates
  a branch anywhere.

### Interactive rebase

`vcs rebase -i <branch|rev>` (for example `HEAD~3`) builds a todo list, opens it in
`VCS_SEQUENCE_EDITOR`, else `GIT_SEQUENCE_EDITOR`, else `GIT_EDITOR`/`core.editor`/`$VISUAL`/`$EDITOR`,
and replays it with the same tree-merge engine as `vcs rebase`: `pick`, `reword`, `edit`, `squash`,
`fixup`, `drop`, `exec`, in any order (a commit left out is dropped; an empty list aborts).
`--autosquash` moves `fixup! X`/`squash! X` commits next to X, and `-x CMD` runs a command after
each commit (without an editor unless `-i` is also given). Scripts and CI set the sequence editor
to a non-interactive command, for example
`GIT_SEQUENCE_EDITOR="sed -i.bak 's/^pick/reword/'" GIT_EDITOR=true vcs git rebase -i HEAD~3`.
An unchanged pick reuses the original tree (no merge), so re-ordering or rewording in place is
cheap and an unchanged list recreates identical commits. `edit` and a failed `exec` stop with the
branch and working tree at the new tip (commit freely, then `--continue`); conflicts stop as in
`vcs rebase`, with `--continue`, `--skip` and `--abort`. The whole rebase is ONE operation:
`vcs op undo` restores the pre-rebase branch and working tree. Not supported: `label`, `reset`,
`merge`, `break`, `--onto`, `--root`, `--rebase-merges`, `fixup -C`.

### Patch files

* `vcs format-patch [-n N | <rev> | A..B] [-o DIR] [--stdout] [--root]`: one mbox-style file per
  commit (`From`, `Date`, `Subject: [PATCH i/n]`, message, diffstat, diff), oldest first. Merges are
  skipped. A commit that changes a binary file is refused with the file name (patches never carry
  binary content). Dates are written in UTC.
* `vcs am <mbox>... [--continue|--skip|--abort]`: applies each mail and commits it with the mail's
  author, date and message. Every patch is planned in memory first, so one that does not apply
  changes nothing; the run stops there and keeps the rest queued in `.vcs/am`. Fix the files by hand,
  `vcs add` them, `vcs am --continue`. `--abort` restores the branch heads from before the run.
  Round trip: `format-patch` then `am` on a clone that has the parent gives identical trees.
* `vcs apply <patch>... [--check] [--index] [-R] [-p N]`: applies a unified diff made by git,
  `vcs format-patch`, `vcs diff` or `vcs show` (text before the first `diff` line is ignored).
  Hunks must match exactly (the position may shift; no fuzz, no whitespace leniency). Paths
  that leave the working tree, go through a symbolic link or touch `.vcs` are refused; binary
  patches (`GIT binary patch`, `Binary files ... differ`) are refused with the file name. With
  `--index` the touched files must match the index and are staged. `-3` is not supported.
* Caveat: `apply --index` and `am` stage and commit without the lockable-file guard that
  `vcs add` applies (the server's require-lock policy can still refuse the publish).

## Concepts that differ

* Zones: each zone has its own branch heads, so `reset`, `switch`, `branch`, `log` act on the zone
  containing the current directory (`--zone Z` for another). `commit` is atomic across all zones.
* Submodules: there are none. A zone is a path prefix with its own branches inside the same
  repository: `vcs zone create <name> --path <dir>`, `vcs zone add|remove <name>` to subscribe,
  `vcs clone --zone <name>` to download only some zones (docs/zones.md).
* One remote, no per-branch upstream. Patch files exist natively (`vcs format-patch`, `vcs am`,
  `vcs apply`, see above); `vcs publish`/`vcs fetch`/`vcs bundle` remain the way to exchange
  commits with full fidelity. `git push --force` is a lease (see above), never a blind overwrite.
* `vcs log` shows 20 commits by default; `vcs git log` shows all, like git.

## The table

The table below is generated by `vcs git --map` from `crates/vcs-cli/src/gitmap_table.rs`; a unit
test fails when they differ. "test" names the scenario in `crates/vcs-cli/tests/gitmap.rs` that runs
the git command line through `vcs git` AND through real git (when installed) in twin repositories
and compares tracked files, working tree, log subjects, branch, tags and status codes. "translation"
means only the unit test `table_matches_translation` checks the rewrite; nothing ran it end to end.
Status: exact (same command and flags), renamed (other spelling, same meaning), different (works,
semantics differ as the notes say), missing (no equivalent; the notes name the native route).

<!-- BEGIN GENERATED TABLE -->
255 rows: 141 exact, 39 renamed, 28 different, 47 missing.

| git | vcs | status | notes | test |
|---|---|---|---|---|
| `git init` | `vcs init` | exact |  | translation |
| `git init proj` | `vcs init proj` | exact |  | translation |
| `git init --bare` | none | missing | a bare repository has no working tree; every vcs repository has one | translation |
| `git init -b trunk` | none | missing | the first branch is always `main`; create others with `vcs switch -c` | translation |
| `git clone ssh://host/app.git proj` | `vcs clone ssh://host/app.git proj` | exact | only the remotes vcs supports work (path, file://, http(s)://, s3://) | translation |
| `git clone /srv/app.git` | `vcs clone /srv/app.git app` | exact | the directory name is inferred as git does | translation |
| `git clone --depth 1 URL` | none | missing | vcs has no shallow clones; use `vcs clone --zone Z` / `--for PATH` to download only part | translation |
| `git clone --bare URL` | none | missing | no bare repositories | translation |
| `git clone --recurse-submodules URL` | none | missing | no submodules; zones are cloned together or selected with `--zone` | translation |
| `git add f.txt` | `vcs add f.txt` | exact |  | `basics` |
| `git add .` | `vcs add .` | exact |  | `basics` |
| `git add -A` | `vcs add .` | different | git stages the whole tree from any directory; vcs stages the current directory (identical at the repository root) | `basics` |
| `git add -u` | `vcs add <every modified or deleted tracked file>` | exact | tracked files only, as git | `basics` |
| `git add -p` | none | missing | no interactive staging; stage whole files, or `vcs stash` / `vcs restore --staged` to adjust | `basics` |
| `git add -f ignored.log` | none | missing | ignored files cannot be force-added; edit `.vcsignore` | translation |
| `git rm f.txt` | `vcs rm f.txt` | exact |  | `basics` |
| `git rm --cached f.txt` | `vcs rm --cached f.txt` | exact |  | `basics` |
| `git rm -rf dir` | `vcs rm --force dir` | exact | `-r` is implied | `basics` |
| `git mv a.txt b.txt` | `vcs mv a.txt b.txt` | exact | within one zone only; a move across a zone boundary is refused | `basics` |
| `git mv a.txt b.txt dir/` | `vcs mv a.txt dir/ ; vcs mv b.txt dir/` | exact |  | translation |
| `git status` | `vcs status` | exact | the layout is per zone and wording differs; the content is the same: staged, unstaged, untracked | `basics` |
| `git status -s` | `vcs status (rendered as XY lines)` | different | rendered by `vcs git` from `vcs status` data; native `vcs status` has no short form | `basics` |
| `git status --porcelain` | `vcs status (rendered as XY lines)` | different | same two-letter codes for A/M/D/??; no rename detection (git shows a rename as R) | `basics` |
| `git status -sb` | `vcs status (rendered as XY lines with ## branch)` | different | branch line shows only the first zone's branch | `basics` |
| `git status --ignored` | none | missing | ignored files are never listed | translation |
| `git diff` | `vcs diff` | exact |  | `basics` |
| `git diff --staged` | `vcs diff --staged` | exact |  | `basics` |
| `git diff --cached` | `vcs diff --staged` | exact |  | `basics` |
| `git diff --stat` | `vcs diff --stat` | exact |  | `basics` |
| `git diff --name-only` | `vcs diff --name-only` | exact |  | `basics` |
| `git diff --name-status` | `vcs diff --name-status` | exact |  | `basics` |
| `git diff -U5 -w` | `vcs diff -w -U 5` | exact |  | `basics` |
| `git diff --patience` | `vcs diff --diff-algorithm=patience` | exact |  | `basics` |
| `git diff --no-color` | `vcs diff --color=never` | exact |  | `basics` |
| `git diff -M` | `vcs diff -M` | exact |  | `basics` |
| `git diff HEAD~1 HEAD` | `vcs git diff HEAD~1 HEAD` | renamed | native `vcs diff` compares only the working tree and the index; `vcs git diff A B` renders two revisions with the same renderer | `history` |
| `git diff main..feature` | `vcs git diff main feature` | renamed | as above | `history` |
| `git diff HEAD` | `vcs git diff HEAD` | renamed | working tree against a revision, tracked files only | `history` |
| `git diff -- f.txt` | `vcs git diff -- f.txt` | renamed | native `vcs diff` has no path filter; `vcs git diff` does | `history` |
| `git diff --exit-code` | none | missing | use `vcs diff --name-only` and test for output | `basics` |
| `git diff --numstat` | none | missing | use `vcs diff --stat` | translation |
| `git diff main...feature` | `vcs git diff main...feature` | renamed | merge base of the two against the second; resolved by `vcs git diff` (native `vcs diff` has no revisions) | `ranges` |
| `git commit -m "msg"` | `vcs commit -m msg` | exact | commits every zone that has staged changes, atomically | `basics` |
| `git commit -a -m "msg"` | `vcs add <every modified or deleted tracked file> ; vcs commit -m msg` | exact |  | `basics` |
| `git commit --amend -m "msg"` | `vcs reset --soft HEAD^ && vcs commit -m msg` | different | replaces the zone's tip commit with a new one (new commit id, same parents); the old one is reachable only via `vcs op log`; without -m the old message is reused (no editor) | `amend` |
| `git commit --amend --no-edit` | `vcs reset --soft HEAD^ && vcs commit -m <old message>` | different | as above | `amend` |
| `git commit -S -m "msg"` | `vcs commit -S -m msg` | different | Ed25519 OpenSSH key signatures, not GPG | translation |
| `git commit --no-verify -m "msg"` | `vcs --no-hooks commit -m msg` | exact |  | translation |
| `git commit --allow-empty -m "msg"` | none | missing | a commit with no change is refused; make a change or use a tag | translation |
| `git commit --author="A <a@x>"` | none | missing | set `user.name`/`user.email` (or VCS_AUTHOR_*) instead | translation |
| `git log` | `vcs log -n 1000000000` | exact | git has no default limit; neither does the translation (native `vcs log` shows 20) | `history` |
| `git log -n 5 --oneline` | `vcs log -n 5 --oneline` | exact |  | `history` |
| `git log -5` | `vcs log -n 5` | exact |  | `history` |
| `git log --graph --oneline` | `vcs log -n 1000000000 --oneline --graph` | exact |  | `history` |
| `git log --author=Dev --since=2024-01-01` | `vcs log -n 1000000000 --author Dev --since 2024-01-01` | exact |  | `history` |
| `git log --grep=fix` | `vcs log -n 1000000000 --grep fix` | exact |  | `history` |
| `git log -S needle` | `vcs log -n 1000000000 -S needle` | exact |  | `history` |
| `git log --follow -- f.txt` | `vcs log -n 1000000000 --follow -- f.txt` | exact |  | `history` |
| `git log --name-status` | `vcs log -n 1000000000 --name-status` | exact |  | `history` |
| `git log --format=%h` | `vcs log -n 1000000000 --format=%h` | exact | vcs placeholders: %H %h %P %p %an %ae %at %ad %s %b %B | `history` |
| `git log main -- f.txt` | `vcs log -n 1000000000 main -- f.txt` | exact |  | `history` |
| `git log -p` | `vcs log + vcs show <each>` | different | each commit is printed with `vcs show` layout; zone line instead of git headers | `history` |
| `git log --stat` | `vcs log + vcs show --stat <each>` | different | as above | `history` |
| `git log --all` | `vcs log -n 1000000000 --all` | different | the branches, tags and detached HEAD of the zone you are in (history is per zone); `vcs log --all-zones` interleaves every zone; remote-tracking branches are not included | `ranges` |
| `git log main..feature` | `vcs log -n 1000000000 main..feature` | exact | commit-graph walk by generation number; same commits and order as git (commit time ties may order differently) | `ranges` |
| `git log main...feature` | `vcs log -n 1000000000 main...feature` | exact | symmetric difference | `ranges` |
| `git log ^main feature` | `vcs log -n 1000000000 ^main feature` | exact |  | `ranges` |
| `git log feature --not main` | `vcs log -n 1000000000 feature ^main` | exact | `--not` is folded into `^rev` | `ranges` |
| `git log --reverse main..feature` | `vcs log -n 1000000000 --reverse main..feature` | exact |  | `ranges` |
| `git log -n 2 main...feature` | `vcs log -n 2 main...feature` | exact |  | `ranges` |
| `git rev-list --count main..feature` | `vcs rev-list --count main..feature` | exact |  | `ranges` |
| `git rev-list --all` | `vcs rev-list --all` | exact | vcs commit ids, not git ids; same set and order | `ranges` |
| `git shortlog -sn main..feature` | `vcs shortlog -s -n main..feature` | exact |  | `ranges` |
| `git log -G regex` | none | missing | `vcs log -S string` or `vcs grep regex <rev>` | translation |
| `git show` | `vcs show` | exact |  | `history` |
| `git show HEAD~1` | `vcs show HEAD~1` | exact |  | `history` |
| `git show --stat HEAD` | `vcs show --stat HEAD` | exact |  | `history` |
| `git show HEAD:f.txt` | `vcs cat-file -p HEAD:f.txt` | renamed |  | `history` |
| `git branch` | `vcs branch` | exact | lists the branches of the current zone | `basics` |
| `git branch feature` | `vcs branch feature` | exact | creates at the checked-out head | `basics` |
| `git branch -d feature` | `vcs branch -d feature` | exact |  | `basics` |
| `git branch -D feature` | `vcs branch -d --force feature` | renamed |  | translation |
| `git branch --show-current` | `vcs branch (the * line)` | renamed |  | `basics` |
| `git branch -m main trunk` | `vcs branch -m main trunk` | exact | refs and workspace move, no file is touched; an op-log entry makes it undoable (`vcs op undo`); refused while a merge or rebase is in progress | `detach` |
| `git branch -M rescue side` | `vcs branch -M rescue side` | exact | replaces an existing branch, never the checked-out one | `detach` |
| `git branch -r` | none | missing | `vcs remote show` lists remote-tracking refs | translation |
| `git branch topic3 main~1` | `vcs branch topic3 main~1` | exact | start is any revision: branch, tag, commit-id prefix, `HEAD~2` | `detach` |
| `git switch main` | `vcs switch main` | exact | a dirty zone is refused instead of carrying changes along | `basics` |
| `git switch -c feature` | `vcs switch -c feature` | exact |  | `merge` |
| `git switch --detach HEAD~1` | `vcs switch --detach HEAD~1` | exact | detached HEAD is per zone: the zone you are in is detached, other zones keep their branch; commits made there go to a hidden per-zone ref (kept by gc, never published); `vcs status` says `HEAD detached`; leaving lists commits no branch reaches and how to keep them; `vcs op undo` brings the detached state back | `detach` |
| `git switch -c rescue` | `vcs switch -c rescue` | exact | from a detached HEAD this keeps the commits made there | `detach` |
| `git switch -c topic2 main` | `vcs branch topic2 main ; vcs switch topic2` | renamed |  | `detach` |
| `git checkout main` | `vcs switch main  (or vcs switch --detach for a tag or commit, vcs restore for a file)` | renamed | a dirty zone is refused instead of carrying changes along | `basics` |
| `git checkout v1.0` | `vcs switch v1.0  (or vcs switch --detach for a tag or commit, vcs restore for a file)` | renamed | a tag or commit id checks out as a detached HEAD (see `git switch --detach`) | `detach` |
| `git checkout -b feature` | `vcs switch -c feature` | renamed |  | translation |
| `git checkout -- f.txt` | `vcs restore f.txt` | renamed |  | `basics` |
| `git checkout HEAD -- f.txt` | `vcs restore f.txt` | renamed |  | translation |
| `git checkout HEAD~1 -- f.txt` | `vcs cat-file -p HEAD~1:<path> into the file + vcs add for f.txt` | renamed | writes the file from the revision and stages it, as git | `history` |
| `git checkout --ours f.txt` | `vcs resolve --ours f.txt` | renamed |  | translation |
| `git checkout --detach v1.0` | `vcs switch --detach v1.0` | renamed | see `git switch --detach` | `detach` |
| `git checkout -b feature2 main` | `vcs branch feature2 main ; vcs switch feature2` | renamed |  | `detach` |
| `git restore f.txt` | `vcs restore f.txt` | exact |  | `basics` |
| `git restore --staged f.txt` | `vcs restore --staged f.txt` | exact |  | `basics` |
| `git restore -S -W f.txt` | `vcs restore --staged f.txt ; vcs restore f.txt` | exact |  | translation |
| `git restore --source=HEAD~1 f.txt` | `vcs cat-file -p HEAD~1:<path> into the file for f.txt` | renamed | writes the file only (not staged), as git | `history` |
| `git restore -p f.txt` | none | missing | no interactive restore | translation |
| `git reset` | `vcs reset` | different | acts on ONE zone (the cwd zone; --zone Z for another) because each zone has its own head | `reset` |
| `git reset --soft HEAD~1` | `vcs reset --soft HEAD~1` | different | as above | `reset` |
| `git reset --mixed HEAD~1` | `vcs reset --mixed HEAD~1` | different | as above | `reset` |
| `git reset --hard HEAD~1` | `vcs reset --hard HEAD~1` | different | as above; discards the zone's uncommitted work | `reset` |
| `git reset HEAD -- f.txt` | `vcs reset HEAD -- f.txt` | exact | unstages the file (same as `vcs restore --staged`) | `basics` |
| `git reset --hard HEAD@{1}` | none | missing | `vcs op log`, then `vcs op undo` / `vcs op restore <id>` | translation |
| `git revert HEAD` | `vcs revert HEAD` | exact |  | `pick` |
| `git revert --no-edit HEAD~2` | `vcs revert HEAD~2` | exact |  | `pick` |
| `git revert -n HEAD` | none | missing | a revert always commits | translation |
| `git cherry-pick feature` | `vcs cherry-pick feature` | exact |  | `pick` |
| `git cherry-pick --continue` | `vcs commit` | exact |  | translation |
| `git cherry-pick A..B` | none | missing | one `vcs cherry-pick` per commit | translation |
| `git merge feature` | `vcs merge feature` | exact | fast-forward when possible, else a merge commit | `merge` |
| `git merge --abort` | `vcs merge --abort` | exact |  | `merge` |
| `git merge --continue` | `vcs commit` | exact |  | translation |
| `git merge origin/main` | `vcs merge --remote main` | renamed | merges the remote-tracking branch as last fetched | translation |
| `git merge --no-ff feature` | `vcs merge --no-ff feature` | exact | always a merge commit, even when a fast-forward is possible | `merge-shapes` |
| `git merge --no-ff -m release feature` | `vcs merge --no-ff -m release feature` | exact |  | `merge-shapes` |
| `git merge --squash feature` | `vcs merge --squash feature` | exact | stages the merged result; the branch does not move until you commit. A squash that would conflict is refused with nothing changed | `merge-shapes` |
| `git merge --ff-only feature` | `vcs merge --ff-only feature` | exact | refuses (nothing changed) unless it can fast-forward | `merge-shapes` |
| `git rebase main` | `vcs rebase main` | exact | keeps each change id | `merge` |
| `git rebase --continue` | `vcs rebase --continue` | exact |  | translation |
| `git rebase --abort` | `vcs rebase --abort` | exact |  | `merge` |
| `git rebase -i HEAD~3` | `vcs rebase -i HEAD~3` | exact | todo list with pick/reword/edit/squash/fixup/drop/exec and reordering; editor from GIT_SEQUENCE_EDITOR, VCS_SEQUENCE_EDITOR, core.editor or EDITOR; no label/reset/merge/break lines; the upstream may be any revision | `rebase_i` |
| `git rebase -i --autosquash HEAD~3` | `vcs rebase -i --autosquash HEAD~3` | exact | `fixup! X` and `squash! X` commits move next to X | `rebase_i` |
| `git rebase -x "test -f f.txt" main` | `vcs rebase -x test -f f.txt main` | exact | runs the command after each replayed commit; a failure stops the rebase (fix, commit, `--continue`) | `rebase_i` |
| `git rebase --skip` | `vcs rebase --skip` | exact | drops the commit that stopped with conflicts | `merge` |
| `git rebase origin/main` | none | missing | `vcs sync`, then `vcs rebase <local branch>` | translation |
| `git stash` | `vcs stash push` | exact | tracked changes only; untracked files stay | `stash` |
| `git stash push -m wip` | `vcs stash push -m wip` | exact |  | `stash` |
| `git stash pop` | `vcs stash pop` | different | the restored changes come back STAGED (git leaves them unstaged) | `stash` |
| `git stash apply stash@{0}` | `vcs stash apply 0` | different | keeps the entry; the restored changes come back STAGED (git leaves them unstaged) | `stash` |
| `git stash list` | `vcs stash list` | exact |  | `stash` |
| `git stash drop` | `vcs stash drop` | exact |  | `stash` |
| `git stash -u` | none | missing | untracked files cannot be stashed; `vcs add` them first | translation |
| `git tag` | `vcs tag` | exact |  | `tags` |
| `git tag v1` | `vcs tag v1` | exact |  | `tags` |
| `git tag -a v1 -m rel` | `vcs tag v1 -m rel` | exact | annotated | translation |
| `git tag v1 HEAD~1` | `vcs tag v1 HEAD~1` | exact |  | translation |
| `git tag -d v1` | `vcs tag -d v1` | exact |  | translation |
| `git tag -s v1 -m rel` | `vcs tag v1 -m rel -s` | different | Ed25519 OpenSSH key, not GPG | translation |
| `git tag -v v1` | `vcs verify-tag v1` | renamed |  | translation |
| `git tag -f v1` | none | missing | `vcs tag -d v1` then `vcs tag v1` | translation |
| `git remote add origin /srv/app` | `vcs remote set /srv/app` | renamed | vcs has exactly one remote | translation |
| `git remote -v` | `vcs remote show` | renamed |  | translation |
| `git remote set-url origin /srv/b` | `vcs remote set /srv/b` | renamed |  | translation |
| `git remote remove origin` | none | missing | `vcs remote set <url>` replaces the one remote | translation |
| `git fetch` | `vcs fetch` | exact | local branches stay untouched | translation |
| `git fetch origin` | `vcs fetch` | exact |  | translation |
| `git fetch --depth 1` | none | missing | no shallow fetch | translation |
| `git pull` | `vcs sync` | different | fetch + fast-forward only; diverged branches are left alone, nothing is merged (`vcs merge --remote <branch>`) | translation |
| `git pull --rebase` | none | missing | `vcs fetch`, `vcs merge --remote <branch>`, or `vcs sync` then `vcs rebase <branch>` | translation |
| `git push` | `vcs publish` | different | publishes ALL checked-out branches in one atomic transaction; fast-forward only | translation |
| `git push -u origin main` | `vcs publish main` | different | publishes only the named branch; no upstream tracking setting | translation |
| `git push --force` | `vcs publish --force` | different | a lease, never a blind overwrite: replaces a diverged remote branch only if it is still the head you last fetched, else refuses with nothing changed. Records the replaced head in the op log and the server audit log | `push-force` |
| `git push -f origin main` | `vcs publish --force main` | different | same lease as `--force`; `--force-without-lease` is native-only | `push-force` |
| `git push origin +main` | `vcs publish --force main` | different | the `+` refspec prefix is the same lease | translation |
| `git push --force-with-lease origin main` | `vcs publish --force-with-lease main` | exact | leased against the remote-tracking head (last fetched) | translation |
| `git push --force-with-lease=main:abc123 origin` | `vcs publish --force-with-lease=main:abc123` | exact | the id may be a prefix of the remote head you expect | translation |
| `git push origin --delete feature` | `vcs publish --delete feature` | different | deletes the remote branch only if it is still the head you last fetched; the old head is recoverable with `vcs branch <name> --at <id>` | `push-force` |
| `git push origin :feature` | `vcs publish --delete feature` | different | same as `--delete` | translation |
| `git push --tags` | none | missing | tags travel with `vcs publish` | translation |
| `git blame f.txt` | `vcs blame f.txt` | exact |  | `history` |
| `git blame -M f.txt` | `vcs blame -M f.txt` | exact |  | `history` |
| `git blame HEAD~1 -- f.txt` | `vcs blame --rev HEAD~1 f.txt` | exact |  | `history` |
| `git blame -L 1,5 f.txt` | none | missing | no line range; pipe `vcs blame` through sed | translation |
| `git bisect start HEAD HEAD~5` | `vcs bisect start HEAD HEAD~5` | exact |  | translation |
| `git bisect good` | `vcs bisect good` | exact |  | translation |
| `git bisect bad` | `vcs bisect bad` | exact |  | translation |
| `git bisect skip` | `vcs bisect skip` | exact |  | translation |
| `git bisect run ./t.sh` | `vcs bisect run ./t.sh` | exact |  | translation |
| `git bisect reset` | `vcs bisect reset` | exact |  | translation |
| `git bisect log` | none | missing | no bisect log | translation |
| `git reflog` | `vcs op log` | renamed | repository-wide operation log, not one log per ref | translation |
| `git reflog -n 5` | `vcs op log -n 5` | renamed |  | translation |
| `git clean -n` | `vcs clean -n` | exact |  | `clean` |
| `git clean -fd` | `vcs clean -f -d` | exact |  | `clean` |
| `git clean -fx` | none | missing | ignored files are never removed | translation |
| `git grep -n needle` | `vcs grep -n needle` | exact | Rust regex syntax | `grep` |
| `git grep -i -e needle HEAD` | `vcs grep -i needle HEAD` | exact |  | `grep` |
| `git grep -F a.b -- src` | `vcs grep -F a.b -- src` | exact |  | `grep` |
| `git grep --cached x` | none | missing | searches the working tree or a revision only | translation |
| `git describe --tags` | `vcs describe --tags` | exact |  | `tags` |
| `git describe --always` | `vcs describe --always` | exact |  | `tags` |
| `git describe --dirty` | none | missing | no --dirty suffix | translation |
| `git archive -o out.tar HEAD` | `vcs archive -o out.tar HEAD` | exact |  | translation |
| `git archive --format=zip --prefix=p/ HEAD` | `vcs archive --format zip --prefix p/ HEAD` | exact |  | translation |
| `git worktree add ../wt feature` | `vcs worktree add ../wt feature` | exact |  | translation |
| `git worktree add -b new ../wt` | `vcs worktree add ../wt new` | exact |  | translation |
| `git worktree list` | `vcs worktree list` | exact |  | translation |
| `git worktree remove ../wt` | `vcs worktree remove ../wt` | exact |  | translation |
| `git worktree prune` | none | missing | `vcs worktree remove` | translation |
| `git submodule add URL path` | none | missing | no nested repositories. A zone is a path-prefix with its own branches and history inside the SAME repository: `vcs zone create <name> --path <dir>`; subscribe with `vcs zone add`, clone parts with `vcs clone --zone` | translation |
| `git submodule update --init` | none | missing | `vcs zone add <name>` materializes a zone | translation |
| `git sparse-checkout set dir` | none | missing | `vcs sparse set <zone> <dir>` (per zone) | translation |
| `git config user.name Dev` | `vcs config set user.name Dev` | renamed |  | translation |
| `git config --global user.name Dev` | `vcs config set user.name Dev --user` | renamed |  | translation |
| `git config user.name` | `vcs config get user.name` | renamed |  | translation |
| `git config --list` | `vcs config list` | renamed |  | translation |
| `git config --unset user.name` | `vcs config unset user.name` | renamed |  | translation |
| `git config --system x y` | none | missing | no system scope | translation |
| `git ls-files` | `vcs ls-files` | exact |  | translation |
| `git ls-files -s` | `vcs ls-files -s` | exact | sha256 object ids, not sha1 | translation |
| `git ls-files -o` | none | missing | `vcs status` lists untracked files | translation |
| `git ls-tree -r --name-only HEAD` | `vcs ls-files --rev HEAD` | renamed | always recursive | translation |
| `git ls-tree HEAD` | `vcs ls-files -s --rev HEAD` | different | flat recursive listing, not one directory level | translation |
| `git cat-file -p HEAD:f.txt` | `vcs cat-file -p HEAD:f.txt` | exact |  | translation |
| `git cat-file -t HEAD` | `vcs cat-file -t HEAD` | exact |  | translation |
| `git cat-file --batch` | none | missing | one object per call | translation |
| `git rev-parse HEAD` | `vcs rev-parse HEAD` | exact | 64-hex ids | translation |
| `git rev-parse --short HEAD` | `vcs rev-parse --short HEAD` | exact |  | translation |
| `git rev-parse --show-toplevel` | `vcs rev-parse --show-toplevel` | exact |  | translation |
| `git rev-parse --abbrev-ref HEAD` | `vcs branch (the * line)` | renamed |  | translation |
| `git rev-parse --git-dir` | none | missing | state lives in `<worktree>/.vcs` | translation |
| `git shortlog -sn` | `vcs shortlog -s -n` | exact |  | translation |
| `git merge-base main feature` | `vcs merge-base main feature` | exact |  | translation |
| `git diff-tree -r A B` | `vcs diff-tree A B` | different | prints `A/D/M path` lines, not git's raw format | translation |
| `git fast-export` | `vcs export-git` | renamed | one zone branch as a fast-import stream | translation |
| `git fast-import` | `vcs import-git -` | renamed |  | translation |
| `git bundle create f.bundle main` | `vcs bundle create f.bundle main` | exact | vcs bundle format, not git's | translation |
| `git bundle verify f.bundle` | `vcs bundle verify f.bundle` | exact |  | translation |
| `git gc` | `vcs gc` | different | deletes unreachable objects older than 14 days (grace period); `--dry-run` previews | translation |
| `git gc --auto` | `vcs repack --auto` | renamed |  | translation |
| `git fsck` | `vcs fsck` | exact |  | translation |
| `git repack -ad` | `vcs repack` | exact |  | translation |
| `git format-patch -1` | `vcs format-patch -n 1` | renamed | writes 0001-subject.patch (mbox style: From, Date, Subject, message, diff) for the last commit of the zone; binary changes are refused, never mangled | translation |
| `git format-patch -1 --stdout` | `vcs format-patch -n 1 --stdout` | renamed | one mailbox on standard output | `patch` |
| `git format-patch -3 -o out` | `vcs format-patch -n 3 -o out` | renamed |  | translation |
| `git format-patch main` | `vcs format-patch main` | exact | the commits of the branch that main lacks | translation |
| `git format-patch HEAD~2..HEAD` | `vcs format-patch HEAD~2..HEAD` | exact |  | translation |
| `git format-patch --root` | `vcs format-patch --root` | exact |  | translation |
| `git format-patch --cover-letter -1` | none | missing | no cover letter; write it by hand | translation |
| `git am x.patch` | `vcs am x.patch` | exact | commits with the mail's author, date and message; a patch that does not apply stops the run, nothing is half-applied | `patch` |
| `git am --continue` | `vcs am --continue` | exact | commits what you staged for the stopped patch | translation |
| `git am --skip` | `vcs am --skip` | exact |  | translation |
| `git am --abort` | `vcs am --abort` | exact |  | translation |
| `git am -3 x.patch` | none | missing | three-way application: a patch carries no blob ids to merge against; edit the patch or `vcs cherry-pick` after `vcs fetch` | translation |
| `git apply x.patch` | `vcs apply x.patch` | exact | unified diffs from git, `vcs diff` and `vcs show`; all hunks must fit (offsets are searched, no fuzz) or nothing changes; binary patches are refused | `patch` |
| `git apply --check x.patch` | `vcs apply --check x.patch` | exact |  | `patch` |
| `git apply --index x.patch` | `vcs apply --index x.patch` | exact | the touched files must match the index first | `patch` |
| `git apply -R x.patch` | `vcs apply -R x.patch` | exact |  | `patch` |
| `git apply -p0 x.patch` | `vcs apply -p 0 x.patch` | exact |  | translation |
| `git apply -3 x.patch` | none | missing | three-way application is not supported | translation |
| `git apply --cached x.patch` | none | missing | index-only application: `vcs apply --index` then `vcs restore` the file | translation |
| `git apply --stat x.patch` | none | missing | no summary modes; `vcs apply --check -v x.patch` lists the files | translation |
| `git mergetool` | none | missing | `vcs conflict show <path>`, edit, `vcs resolve --mark <path>` | translation |
| `git lfs track` | none | missing | large files are chunked natively | translation |
| `git filter-branch` | none | missing | `vcs zone apply` splits history by path; otherwise none | translation |
| `git notes add` | none | missing | none | translation |
<!-- END GENERATED TABLE -->
