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
`merge`, `break`, `fixup -C`, and `-i` together with `--onto`, `--root` or `--rebase-merges`.

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
334 rows: 185 exact, 43 renamed, 82 different, 24 missing.

| git | vcs | status | notes | test |
|---|---|---|---|---|
| `git init` | `vcs init` | exact |  | translation |
| `git init proj` | `vcs init proj` | exact |  | translation |
| `git init --bare` | none | missing | a bare repository has no working tree; every vcs repository has one | translation |
| `git init -b trunk` | `vcs init --initial-branch trunk` | exact | the root zone starts on that branch | translation |
| `git clone ssh://host/app.git proj` | `vcs clone ssh://host/app.git proj` | exact | only the remotes vcs supports work (path, file://, http(s)://, s3://) | translation |
| `git clone /srv/app.git` | `vcs clone /srv/app.git app` | exact | the directory name is inferred as git does | translation |
| `git clone --depth 1 URL` | none | missing | vcs has no shallow clones; use `vcs clone --zone Z` / `--for PATH` to download only part | translation |
| `git clone --bare URL` | none | missing | no bare repositories | translation |
| `git clone --recurse-submodules URL` | none | missing | no submodules; zones are cloned together or selected with `--zone` | translation |
| `git add f.txt` | `vcs add f.txt` | exact |  | `basics` |
| `git add .` | `vcs add .` | exact |  | `basics` |
| `git add -A` | `vcs add .` | different | git stages the whole tree from any directory; vcs stages the current directory (identical at the repository root) | `basics` |
| `git add -u` | `vcs add <every modified or deleted tracked file>` | exact | tracked files only, as git | `basics` |
| `git add -f ignored.log` | `vcs add -f ignored.log` | exact | also for a file under an ignored directory and for a whole ignored directory; once tracked, a file is no longer reported as ignored, as in git | `ignored` |
| `git add -p` | `vcs add -p` | different | hunks from standard input or a terminal: y n q a d s e j J k K g ?; tracked files only, text only; no `/` search, no mode-change hunks; the answers are read one per line | translation |
| `git add -p f.txt` | `vcs add -p f.txt` | exact | same hunks, splitting and `e` editing as git; the result is staged without touching the working file | translation |
| `git rm f.txt` | `vcs rm f.txt` | exact |  | `basics` |
| `git rm --cached f.txt` | `vcs rm --cached f.txt` | exact |  | `basics` |
| `git rm -rf dir` | `vcs rm --force dir` | exact | `-r` is implied | `basics` |
| `git mv a.txt b.txt` | `vcs mv a.txt b.txt` | exact | within one zone only; a move across a zone boundary is refused | `basics` |
| `git mv a.txt b.txt dir/` | `vcs mv a.txt dir/ ; vcs mv b.txt dir/` | exact |  | translation |
| `git status` | `vcs status` | exact | the layout is per zone and wording differs; the content is the same: staged, unstaged, untracked | `basics` |
| `git status -s` | `vcs status (rendered as XY lines)` | different | rendered by `vcs git` from `vcs status` data; native `vcs status` has no short form | `basics` |
| `git status --porcelain` | `vcs status (rendered as XY lines)` | different | same two-letter codes for A/M/D/??; no rename detection (git shows a rename as R) | `basics` |
| `git status -sb` | `vcs status (rendered as XY lines with ## branch)` | different | branch line shows only the first zone's branch | `basics` |
| `git status --ignored` | `vcs status --ignored` | different | an `ignored:` section after `untracked:`; a directory ignored as a whole is one entry with a trailing `/` | translation |
| `git status -s --ignored` | `vcs status (rendered as XY lines with !! ignored)` | different | `!!` lines as git, listed after the `??` lines | `ignored` |
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
| `git diff --exit-code` | `vcs diff --exit-code` | exact | exit status 1 when there are differences, 0 when there are none | `basics` |
| `git diff --numstat` | `vcs diff --numstat` | exact |  | `basics` |
| `git diff main...feature` | `vcs git diff main...feature` | renamed | merge base of the two against the second; resolved by `vcs git diff` (native `vcs diff` has no revisions) | `ranges` |
| `git commit -m "msg"` | `vcs commit -m msg` | exact | commits every zone that has staged changes, atomically | `basics` |
| `git commit -a -m "msg"` | `vcs add <every modified or deleted tracked file> ; vcs commit -m msg` | exact |  | `basics` |
| `git commit --amend -m "msg"` | `vcs reset --soft HEAD^ && vcs commit -m msg` | different | replaces the zone's tip commit with a new one (new commit id, same parents); the old one is reachable only via `vcs op log`; without -m the old message is reused (no editor) | `amend` |
| `git commit --amend --no-edit` | `vcs reset --soft HEAD^ && vcs commit -m <old message>` | different | as above | `amend` |
| `git commit -S -m "msg"` | `vcs commit -S -m msg` | different | Ed25519 OpenSSH key signatures, not GPG | translation |
| `git commit --no-verify -m "msg"` | `vcs --no-hooks commit -m msg` | exact |  | translation |
| `git commit --allow-empty -m "msg"` | `vcs commit --allow-empty -m msg` | different | acts on ONE zone (the cwd zone) because each zone has its own head; other zones commit only when they have staged changes | `basics` |
| `git commit --author="A <a@x>" -m "msg"` | `vcs commit --author A <a@x> -m msg` | different | vcs records one identity per commit, so it is the author and the committer both (git keeps the committer from `user.*`) | `basics` |
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
| `git log -G v3` | `vcs log -n 1000000000 -G v3` | exact | Rust regex syntax; a line counts when it was added or removed in net (a moved line does not) | `history` |
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
| `git branch -r` | `vcs branch -r` | different | lists the cwd zone's remote-tracking branches as `origin/<branch>`; the remote is always `origin` | `push-force` |
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
| `git restore -p f.txt` | `vcs restore -p f.txt` | different | discards chosen hunks of the working file (relative to the index); `e` is not offered | translation |
| `git restore --staged -p f.txt` | `vcs restore --staged -p f.txt` | different | unstages chosen hunks (index against HEAD); `e` is not offered | translation |
| `git checkout -p` | `vcs checkout -p` | different | `vcs checkout` exists only for -p; discards chosen hunks of every changed tracked file from the working tree (relative to the index) | translation |
| `git checkout -p HEAD~1 -- f.txt` | `vcs checkout -p HEAD~1 -- f.txt` | different | takes chosen hunks of the revision into the working file AND the index, which must agree for that file first | translation |
| `git stash -p` | `vcs stash push -p` | different | stashes the chosen hunks of tracked files, the rest stays as unstaged changes (the index is reset to HEAD); `e` is not offered; staged new files are refused | translation |
| `git stash push -p -- f.txt` | `vcs stash push -p -- f.txt` | different | same, for those paths only | translation |
| `git reset` | `vcs reset` | different | acts on ONE zone (the cwd zone; --zone Z for another) because each zone has its own head | `reset` |
| `git reset --soft HEAD~1` | `vcs reset --soft HEAD~1` | different | as above | `reset` |
| `git reset --mixed HEAD~1` | `vcs reset --mixed HEAD~1` | different | as above | `reset` |
| `git reset --hard HEAD~1` | `vcs reset --hard HEAD~1` | different | as above; discards the zone's uncommitted work | `reset` |
| `git reset HEAD -- f.txt` | `vcs reset HEAD -- f.txt` | exact | unstages the file (same as `vcs restore --staged`) | `basics` |
| `git reset --hard HEAD@{1}` | `vcs reset --hard HEAD@{1}` | different | `<name>@{N}` (HEAD or a branch) is read from the operation log, one entry per operation that CHANGES the value (git also logs operations that leave it unchanged); dates (`@{yesterday}`), `@{upstream}` and `ORIG_HEAD` are refused; acts on one zone | `reflog` |
| `git revert HEAD` | `vcs revert HEAD` | exact |  | `pick` |
| `git revert --no-edit HEAD~2` | `vcs revert HEAD~2` | exact |  | `pick` |
| `git revert -n HEAD` | `vcs revert --no-commit HEAD` | exact | the undoing change is left staged; record it with `vcs commit` | `pick` |
| `git cherry-pick feature` | `vcs cherry-pick feature` | exact |  | `pick` |
| `git cherry-pick --continue` | `vcs commit` | exact |  | translation |
| `git cherry-pick A..B` | `vcs cherry-pick A..B` | exact | oldest first in one run; a merge commit in the range is refused up front unless `-m` names its mainline; a pick that changes nothing is skipped; a conflict stops the run, prints the commits left to pick, and is finished with `vcs commit` | translation |
| `git cherry-pick -m 1 M` | `vcs cherry-pick -m 1 M` | exact | picks the merge commit M as the change against its first parent (1-based, as git); without -m a merge commit is refused | translation |
| `git cherry-pick A...B` | `vcs cherry-pick A...B` | exact | the commits of either side that the other lacks, oldest first, in one run (merge commits among them are refused unless -m) | translation |
| `git cherry main feature` | `vcs cherry main feature` | different | `+`/`-` per commit by patch id (the diff against the first parent, line numbers ignored, context included); prints full ids, or `-v` short id and subject | translation |
| `git range-diff main..old main..new` | `vcs range-diff main..old main..new` | different | commits are paired by identical diff, then by minimum total difference of the patch texts (`--creation-factor`, default 60); `=`/`!`/`<`/`>` lines as git, merges skipped; the `!` diff is plain (no colour); also `A...B` and `base old new` | translation |
| `git merge feature` | `vcs merge feature` | exact | fast-forward when possible, else a merge commit | `merge` |
| `git merge other` | `vcs merge other` | exact | also when the two branches have several merge bases (criss-cross history): the bases are merged into a virtual base first, as git's recursive/ort strategies do | `criss_cross` |
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
| `git rebase --onto main A` | `vcs rebase --onto main A` | exact | replays the commits A lacks onto main (any revision); not combinable with -i/-x; the branch to rebase must be checked out | translation |
| `git rebase --onto main --root` | `vcs rebase --onto main --root` | exact | replays the whole branch, root commit included; linear history only | translation |
| `git rebase --rebase-merges main` | `vcs rebase --rebase-merges main` | exact | recreates the merge commits (message, author and change id kept, tree freshly merged) and stops/continues/skips/aborts on conflicts like any rebase; the default `no-rebase-cousins` leaves commits that do not descend from the merge base alone, `--rebase-merges=rebase-cousins` replays them; criss-cross histories use a virtual merge base; not with -i/-x (no label/reset/merge todo lines) | `rebase_merges` |
| `git rebase --rebase-merges=rebase-cousins main` | `vcs rebase --rebase-merges=rebase-cousins main` | exact | also replays commits that do not descend from the merge base onto the new base | `rebase_cousins` |
| `git rebase --rebase-merges=no-rebase-cousins main` | `vcs rebase --rebase-merges main` | exact | the default mode: commits that do not descend from the merge base keep their original parents | `rebase_nocousins` |
| `git rebase origin/main` | `vcs rebase origin/main` | exact | origin/<branch> is the remote-tracking head as of the last `vcs fetch` | translation |
| `git stash` | `vcs stash push` | exact | tracked changes only; untracked files stay | `stash` |
| `git stash push -m wip` | `vcs stash push -m wip` | exact |  | `stash` |
| `git stash pop` | `vcs stash pop` | different | the restored changes come back STAGED (git leaves them unstaged) | `stash` |
| `git stash apply stash@{0}` | `vcs stash apply 0` | different | keeps the entry; the restored changes come back STAGED (git leaves them unstaged) | `stash` |
| `git stash list` | `vcs stash list` | exact |  | `stash` |
| `git stash drop` | `vcs stash drop` | exact |  | `stash` |
| `git stash -u` | `vcs stash push --include-untracked` | different | untracked files are added first, so `pop` brings them back staged, not untracked; ignored files are never stashed | `stash` |
| `git tag` | `vcs tag` | exact |  | `tags` |
| `git tag v1` | `vcs tag v1` | exact |  | `tags` |
| `git tag -a v1 -m rel` | `vcs tag v1 -m rel` | exact | annotated | translation |
| `git tag v1 HEAD~1` | `vcs tag v1 HEAD~1` | exact |  | translation |
| `git tag -d v1` | `vcs tag -d v1` | exact |  | translation |
| `git tag -s v1 -m rel` | `vcs tag v1 -m rel -s` | different | Ed25519 OpenSSH key, not GPG | translation |
| `git tag -v v1` | `vcs verify-tag v1` | renamed |  | translation |
| `git tag -f v1` | `vcs tag --force v1` | exact | replaces the tag; the old one is dropped only after the new target resolves | `tags` |
| `git remote add origin /srv/app` | `vcs remote set /srv/app` | renamed | vcs has exactly one remote | translation |
| `git remote -v` | `vcs remote show` | renamed |  | translation |
| `git remote set-url origin /srv/b` | `vcs remote set /srv/b` | renamed |  | translation |
| `git remote remove origin` | `vcs remote remove` | exact | forgets the url and the remote-tracking refs; local branches stay | `push-force` |
| `git fetch` | `vcs fetch` | exact | local branches stay untouched | translation |
| `git fetch origin` | `vcs fetch` | exact |  | translation |
| `git fetch --depth 1` | none | missing | no shallow fetch | translation |
| `git pull` | `vcs sync` | different | fetch + fast-forward only; diverged branches are left alone, nothing is merged (`vcs merge --remote <branch>`) | translation |
| `git pull --rebase` | `vcs fetch ; vcs rebase origin/<current branch>` | different | a conflict stops with the usual rebase workflow (`--continue`/`--abort`); no autostash; the remote is always `origin` | translation |
| `git push` | `vcs publish` | different | publishes ALL checked-out branches in one atomic transaction; fast-forward only | translation |
| `git push -u origin main` | `vcs publish main` | different | publishes only the named branch; no upstream tracking setting | translation |
| `git push --force` | `vcs publish --force` | different | a lease, never a blind overwrite: replaces a diverged remote branch only if it is still the head you last fetched, else refuses with nothing changed. Records the replaced head in the op log and the server audit log | `push-force` |
| `git push -f origin main` | `vcs publish --force main` | different | same lease as `--force`; `--force-without-lease` is native-only | `push-force` |
| `git push origin +main` | `vcs publish --force main` | different | the `+` refspec prefix is the same lease | translation |
| `git push --force-with-lease origin main` | `vcs publish --force-with-lease main` | exact | leased against the remote-tracking head (last fetched) | translation |
| `git push --force-with-lease=main:abc123 origin` | `vcs publish --force-with-lease=main:abc123` | exact | the id may be a prefix of the remote head you expect | translation |
| `git push origin --delete feature` | `vcs publish --delete feature` | different | deletes the remote branch only if it is still the head you last fetched; the old head is recoverable with `vcs branch <name> --at <id>` | `push-force` |
| `git push origin :feature` | `vcs publish --delete feature` | different | same as `--delete` | translation |
| `git push --tags` | `vcs publish --tags` | exact | publishes only the tags, no branch moves (a plain `vcs publish` sends both); a tag that exists on the remote at another target is reported, not moved | translation |
| `git push --prune` | `vcs publish --prune` | different | also deletes remote branches of subscribed zones that have no local branch; each deletion needs the head you last fetched (or your lease), is one atomic transaction, and is in the op log and server audit log | translation |
| `git push --mirror` | `vcs publish --mirror` | different | forces every checked-out branch with a lease (stale info refuses, nothing changed), prunes remote branches with no local one, sends tags; remote tags are never deleted | translation |
| `git blame f.txt` | `vcs blame f.txt` | exact |  | `history` |
| `git blame -M f.txt` | `vcs blame -M f.txt` | exact |  | `history` |
| `git blame HEAD~1 -- f.txt` | `vcs blame --rev HEAD~1 f.txt` | exact |  | `history` |
| `git blame -L 1,5 m.txt` | `vcs blame -L 1,5 m.txt` | exact | numeric ranges only (`N,M`, `N,+K`, `N,-K`, `N`, `,M`, `N,`); no regex or function ranges | `history` |
| `git bisect start HEAD HEAD~5` | `vcs bisect start HEAD HEAD~5` | exact |  | translation |
| `git bisect good` | `vcs bisect good` | exact |  | translation |
| `git bisect bad` | `vcs bisect bad` | exact |  | translation |
| `git bisect skip` | `vcs bisect skip` | exact |  | translation |
| `git bisect run ./t.sh` | `vcs bisect run ./t.sh` | exact |  | translation |
| `git bisect reset` | `vcs bisect reset` | exact |  | translation |
| `git rerere` | `vcs rerere` | exact | needs `rerere.enabled` (`vcs config set rerere.enabled on`); records and replays text-conflict resolutions for merge, rebase, cherry-pick and revert (docs/rerere.md) | translation |
| `git rerere status` | `vcs rerere status` | exact |  | translation |
| `git rerere remaining` | `vcs rerere remaining` | exact |  | translation |
| `git rerere diff` | `vcs rerere diff` | different | same content (preimage against the file now, marker labels ignored); the diff header is vcs's | translation |
| `git rerere forget f.txt` | `vcs rerere forget f.txt` | exact |  | translation |
| `git rerere clear` | `vcs rerere clear` | exact |  | translation |
| `git rerere gc` | `vcs rerere gc` | exact | limits from `gc.rerereResolved` (60 days) and `gc.rerereUnresolved` (15 days) | translation |
| `git rerere train` | none | missing | no training from history; resolve once and it is recorded | translation |
| `git bisect log` | `vcs bisect log` | different | the marks in the order given, as `vcs bisect <verb> <id>` lines with `# verb: [id] subject` comments, ending with `# first bad commit` when found; ids are vcs ids | translation |
| `git bisect replay f.log` | `vcs bisect replay f.log` | different | replays a `vcs bisect log` (a git log carries sha1 ids that do not exist here); refuses when a bisection is in progress | translation |
| `git reflog` | `vcs op log` | renamed | repository-wide operation log, not one log per ref | translation |
| `git reflog -n 5` | `vcs op log -n 5` | renamed |  | translation |
| `git clean -n` | `vcs clean -n` | exact |  | `clean` |
| `git clean -fd` | `vcs clean -f -d` | exact |  | `clean` |
| `git clean -fx` | `vcs clean -f -x` | exact | ignored directories are removed only with -d as well; no -X (ignored files only) | `clean` |
| `git grep -n needle` | `vcs grep -n needle` | exact | Rust regex syntax | `grep` |
| `git grep -i -e needle HEAD` | `vcs grep -i needle HEAD` | exact |  | `grep` |
| `git grep -F a.b -- src` | `vcs grep -F a.b -- src` | exact |  | `grep` |
| `git grep --cached needle` | `vcs grep --cached needle` | exact | searches the staged content, not the files on disk | `grep` |
| `git describe --tags` | `vcs describe --tags` | exact |  | `tags` |
| `git describe --always` | `vcs describe --always` | exact |  | `tags` |
| `git describe --dirty` | `vcs describe --dirty` | exact | tracked changes only (staged or unstaged), as git; HEAD only | `tags` |
| `git archive -o out.tar HEAD` | `vcs archive -o out.tar HEAD` | exact |  | translation |
| `git archive --format=zip --prefix=p/ HEAD` | `vcs archive --format zip --prefix p/ HEAD` | exact |  | translation |
| `git worktree add ../wt feature` | `vcs worktree add ../wt feature` | exact |  | translation |
| `git worktree add -b new ../wt` | `vcs worktree add ../wt new` | exact |  | translation |
| `git worktree list` | `vcs worktree list` | exact |  | translation |
| `git worktree remove ../wt` | `vcs worktree remove ../wt` | exact |  | translation |
| `git worktree prune` | `vcs worktree prune` | exact | forgets the registration of a worktree whose directory is gone at once (no --expire); never deletes a directory | translation |
| `git worktree prune -n` | `vcs worktree prune --dry-run` | exact |  | translation |
| `git submodule add URL path` | none | missing | no nested repositories. A zone is a path-prefix with its own branches and history inside the SAME repository: `vcs zone create <name> --path <dir>`; subscribe with `vcs zone add`, clone parts with `vcs clone --zone` | translation |
| `git submodule update --init` | none | missing | `vcs zone add <name>` materializes a zone | translation |
| `git sparse-checkout set dir` | none | missing | `vcs sparse set <zone> <dir>` (per zone) | translation |
| `git config user.name Dev` | `vcs config set user.name Dev` | renamed |  | translation |
| `git config --global user.name Dev` | `vcs config set user.name Dev --user` | renamed |  | translation |
| `git config user.name` | `vcs config get user.name` | renamed |  | translation |
| `git config --list` | `vcs config list` | renamed |  | translation |
| `git config --unset user.name` | `vcs config unset user.name` | renamed |  | translation |
| `git config --system user.name Dev` | `vcs config set user.name Dev --system` | renamed | the file is `$VCS_SYSTEM_CONFIG`, else /etc/vcs/config; the lowest layer, below the user and repository configs | translation |
| `git ls-files` | `vcs ls-files` | exact |  | translation |
| `git ls-files -s` | `vcs ls-files -s` | exact | sha256 object ids, not sha1 | translation |
| `git ls-files -o` | `vcs ls-files -o` | different | never lists files matched by `.vcsignore` (git needs --exclude-standard for that, which is accepted and implied); one line per file | `clean` |
| `git ls-tree -r --name-only HEAD` | `vcs ls-files --rev HEAD` | renamed | always recursive | translation |
| `git ls-tree HEAD` | `vcs ls-files -s --rev HEAD --depth 1` | renamed | one level from the top tree; directories are mode 040000 with their tree id, no type column | translation |
| `git cat-file -p HEAD:f.txt` | `vcs cat-file -p HEAD:f.txt` | exact |  | translation |
| `git cat-file -t HEAD` | `vcs cat-file -t HEAD` | exact |  | translation |
| `git cat-file --batch` | `vcs cat-file --batch` | different | object names come from standard input; ids are 64 hex digits; the content of a commit or tag is vcs's own text form | translation |
| `git rev-parse HEAD` | `vcs rev-parse HEAD` | exact | 64-hex ids | translation |
| `git rev-parse --short HEAD` | `vcs rev-parse --short HEAD` | exact |  | translation |
| `git rev-parse --show-toplevel` | `vcs rev-parse --show-toplevel` | exact |  | translation |
| `git rev-parse --abbrev-ref HEAD` | `vcs branch (the * line)` | renamed |  | translation |
| `git rev-parse --git-dir` | `vcs rev-parse --git-dir` | different | prints `.vcs` (the state directory of this worktree), absolute below the root; not `.git` | `plumbing` |
| `git shortlog -sn` | `vcs shortlog -s -n` | exact |  | translation |
| `git merge-base main feature` | `vcs merge-base main feature` | exact |  | translation |
| `git show-branch main feature` | `vcs show-branch main feature` | different | the same matrix (`!`/`*` header, `+`/`-`/`*` columns, stops at the common ancestor); rows whose commit times are equal can come out in another order; no globs, -a/-r (remote branches), --topics, -g | translation |
| `git show-branch --list` | `vcs show-branch --list` | exact |  | translation |
| `git show-branch --merge-base main feature` | `vcs show-branch --merge-base main feature` | exact |  | translation |
| `git show-branch --independent main feature` | `vcs show-branch --independent main feature` | exact |  | translation |
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
| `git format-patch A...B` | `vcs format-patch A...B` | exact | the non-merge commits of either side that the other lacks | translation |
| `git format-patch --root` | `vcs format-patch --root` | exact |  | translation |
| `git format-patch --cover-letter -1` | `vcs format-patch -n 1 --cover-letter` | renamed | `0000-cover-letter.patch` first (placeholders for subject and blurb, shortlog, diffstat); `vcs am` needs the cover letter left out | `patch` |
| `git am x.patch` | `vcs am x.patch` | exact | commits with the mail's author, date and message; a patch that does not apply stops the run, nothing is half-applied | `patch` |
| `git am --continue` | `vcs am --continue` | exact | commits what you staged for the stopped patch | translation |
| `git am --skip` | `vcs am --skip` | exact |  | translation |
| `git am --abort` | `vcs am --abort` | exact |  | translation |
| `git am -3 x.patch` | `vcs am -3 x.patch` | different | a patch has no blob ids, so each hunk that does not fit is merged against its own pre-image (text merge); overlapping edits stop the run with markers in the file (resolve, `vcs add`, `vcs am --continue`) | translation |
| `git apply x.patch` | `vcs apply x.patch` | exact | unified diffs from git, `vcs diff` and `vcs show`; all hunks must fit (offsets are searched, no fuzz) or nothing changes; binary patches are refused | `patch` |
| `git apply --check x.patch` | `vcs apply --check x.patch` | exact |  | `patch` |
| `git apply --index x.patch` | `vcs apply --index x.patch` | exact | the touched files must match the index first | `patch` |
| `git apply -R x.patch` | `vcs apply -R x.patch` | exact |  | `patch` |
| `git apply -p0 x.patch` | `vcs apply -p 0 x.patch` | exact |  | translation |
| `git apply -3 x.patch` | `vcs apply -3 x.patch` | different | same hunk-local three-way merge as `am -3`; conflicts leave markers, are not staged, and exit non-zero | translation |
| `git apply --cached x.patch` | `vcs apply --cached x.patch` | exact | checked against the staged content and staged; the working tree is not touched (no --3way with it) | `patch` |
| `git apply --stat x.patch` | `vcs apply --stat x.patch` | different | applies nothing, as git; the counts are `+added -deleted` per file (git draws a bar graph); no --numstat/--summary | `patch` |
| `git mergetool` | `vcs mergetool` | different | tool named by `merge.tool`, command `mergetool.<name>.cmd` in the USER config (run with $BASE $LOCAL $REMOTE $MERGED); no built-in tool list; both-sides-modified conflicts only (delete/rename conflicts: `vcs resolve --ours\|--theirs`); keeps `<file>.orig` unless `mergetool.keepBackup off` | translation |
| `git mergetool -y` | `vcs mergetool -y` | exact | do not ask before starting the tool | translation |
| `git mergetool --tool=mine f.txt` | `vcs mergetool --tool mine f.txt` | renamed |  | translation |
| `git lfs track` | none | missing | large files are chunked natively | translation |
| `git filter-branch --subdirectory-filter lib` | `vcs rewrite --subdirectory-filter lib` | different | rewrites the checked-out branch of the cwd zone (paths are zone-relative); commits the filter empties are dropped (git keeps them unless --prune-empty); the whole run is ONE operation, `vcs op undo` puts every branch and tag back; refused when a changed commit is published, unless --force; needs a clean zone | translation |
| `git filter-branch --subdirectory-filter lib -- --all` | `vcs rewrite --subdirectory-filter lib --all-branches` | different | every branch of the zone, and the tags on rewritten commits; other zones are never touched | translation |
| `git filter-branch --prune-empty --subdirectory-filter lib` | `vcs rewrite --subdirectory-filter lib` | different | pruning is the default; `--keep-empty` keeps emptied commits | translation |
| `git filter-branch --msg-filter ./fix-msg.sh` | `vcs rewrite --msg-filter ./fix-msg.sh` | different | run with `sh -c`, old message on stdin, new message on stdout, as git | translation |
| `git filter-branch --env-filter ./fix-author.sh` | `vcs rewrite --env-filter ./fix-author.sh` | different | GIT_AUTHOR_NAME, GIT_AUTHOR_EMAIL and GIT_AUTHOR_DATE (`@SECONDS`) can be changed; a vcs commit has ONE signature, so GIT_COMMITTER_* are readable but changes to them are ignored (a warning counts them) | translation |
| `git filter-branch --tree-filter ./strip.sh` | `vcs rewrite --tree-filter ./strip.sh` | different | each commit's tree is checked out into a scratch directory under .vcs, the command runs there, what is left becomes the tree; one checkout and re-hash per commit, so slow on long histories | translation |
| `git filter-branch --tag-name-filter cat -- --all` | `vcs rewrite --tag-name-filter cat --all-branches` | different | tags on rewritten commits are always moved to the new commits (git needs this flag); the command may rename them; annotated tags lose their signature | translation |
| `git filter-branch --index-filter ./f.sh` | none | missing | no index filter, parent filter or commit filter; use `vcs rewrite --keep\|--remove` for paths or --tree-filter for anything else | translation |
| `git filter-repo --path src` | `vcs rewrite --keep src` | different | as filter-branch: checked-out branch, one op, refused when published without --force. No fresh-clone requirement | translation |
| `git filter-repo --path secrets --invert-paths` | `vcs rewrite --remove secrets` | different | repeat --path to remove several | translation |
| `git filter-repo --subdirectory-filter lib` | `vcs rewrite --subdirectory-filter lib` | different | no --to-subdirectory-filter and no --path-rename | translation |
| `git filter-repo --force --path src` | `vcs rewrite --keep src --force` | different | git's --force overrides the fresh-clone check; vcs --force overrides the refusal to change commits that origin holds (as last fetched) | translation |
| `git filter-repo --replace-text expr.txt` | none | missing | no callbacks, --replace-text, --mailmap, --path-rename or --path-regex; use --msg-filter / --tree-filter with sed | translation |
| `git notes add -m first HEAD~1` | `vcs notes add -m first HEAD~1` | exact | notes are per zone: they annotate commits of the zone of the current directory; stored as objects on an internal ref, published and fetched with the branches | `notes` |
| `git notes add -f -m replaced HEAD` | `vcs notes add --force -m replaced HEAD` | exact | without -f an existing note is refused, as git | `notes` |
| `git notes append -m more HEAD` | `vcs notes append -m more HEAD` | exact | a blank line separates the old text from the appended text, as git | `notes` |
| `git notes show HEAD` | `vcs notes show HEAD` | exact | exit 1 when the commit has no note | `notes` |
| `git notes list` | `vcs notes list` | different | prints `<note blob id> <commit id>` like git, but the ids are 64-digit BLAKE3, not SHA-1 | `notes` |
| `git notes remove HEAD` | `vcs notes remove HEAD` | exact | an error when the commit has no note | `notes` |
| `git notes edit` | none | missing | no editor: `vcs notes add --force -m <text>` or `vcs notes append` | translation |
| `git notes merge other` | none | missing | one notes ref per zone; `vcs fetch` merges remote notes itself (union, texts concatenated when both sides annotated a commit) | translation |
| `git notes copy HEAD~1 HEAD` | none | missing | `vcs notes show HEAD~1`, then `vcs notes add -m <text> HEAD` | translation |
| `git notes prune` | none | missing | notes of unreachable commits are kept; remove them with `vcs notes remove <commit>` | translation |
| `git notes --ref=review add -m x` | none | missing | only the default notes ref exists, per zone | translation |
| `git log --notes` | none | missing | `vcs log` does not print notes; use `vcs notes show <rev>` | translation |
| `git for-each-ref --format=%(refname) refs/heads` | `vcs branch + vcs tag (rendered as refs)` | different | acts on one zone; refs are refs/heads, refs/tags, refs/remotes/origin; atoms refname[:short], objectname[:short], objecttype (always commit, a tag shows its commit), subject, body, authorname, authoremail, authordate:unix, HEAD; --sort on refname, objectname, authordate, committerdate, subject; --count; ids are blake3 | `refs` |
| `git show-ref` | `vcs branch + vcs tag (rendered as `<id> <refname>`)` | different | acts on one zone; exit 1 when nothing matches; no --dereference or --exclude-existing; ids are blake3 | `refs` |
| `git show-ref --verify -q refs/heads/main` | `vcs branch + vcs tag (rendered as `<id> <refname>`)` | different | as above | `refs` |
| `git ls-remote --heads origin` | `vcs remote show (read live from the remote)` | different | reads the remote's manifest now (not the refs fetched last); every zone: the root zone as refs/heads/<b>, another as refs/zones/<zone>/heads/<b>; no HEAD line, no --symref | translation |
| `git name-rev HEAD~2` | `vcs describe-like search over branches and tags` | different | names a commit from the nearest branch or tag of the zone (`main~2`, `tags/v1~1`, `tags/v1^0` for an annotated tag); a tie goes to the tag; merges weigh as in git; no --tags/--all/--stdin | `refs` |
| `git update-ref refs/heads/topic HEAD` | `vcs branch <name> <rev> (moves the ref only)` | different | creates or moves the branch without touching the index or files, even when it is checked out (the working tree then differs from it, as in git); a branch checked out in another worktree is refused; HEAD, refs/heads/* and refs/tags/* only | `refs` |
| `git update-ref -d refs/heads/topic` | `vcs branch -d / vcs tag -d` | different | never the checked-out branch | `refs` |
| `git check-ignore ignored.log` | `vcs .vcsignore match` | different | prints the paths `.vcsignore` hides, exit 1 when none; tracked files are never ignored; no -v/-n/--stdin | `ignored` |
| `git count-objects -v` | `vcs object store count` | different | `count`, `size` (loose objects), `packs`, `size-pack` only | translation |
| `git hash-object -w f.txt` | `vcs object id of file content, stored` | different | the id of the file content as `vcs add` would store it (blake3, 64 hex digits); blobs only; without -w nothing is written | translation |
| `git write-tree` | `vcs tree id of the index` | different | the tree of the zone containing the current directory (every zone has its own) | translation |
| `git commit-tree abc -p HEAD -m msg` | `vcs commit object from a tree` | different | a commit of that zone with the given tree and parents, referenced by nothing until `update-ref`; the author is `user.name`/`user.email`; the tree id must be written in full | translation |
| `git prune` | `vcs gc` | different | deletes unreachable objects older than the 14-day grace period (git's default is 2 weeks) | translation |
| `git prune -n` | `vcs gc --dry-run` | exact |  | translation |
| `git update-index --add f.txt` | none | missing | the index is a binary image rewritten only by vcs commands: `vcs add`, `vcs rm --cached`, `vcs restore --staged` | translation |
| `git read-tree HEAD` | none | missing | one zone's index follows its branch and cannot be loaded from an arbitrary tree: `vcs reset --mixed <rev>` (moves the branch too) | translation |
| `git pack-objects --stdout` | none | missing | vcs packs are its own format, written by repack/gc and read by fetch: `vcs repack`, `vcs bundle create` | translation |
| `git check-attr -a f.txt` | none | missing | there is no .gitattributes; `.vcsattributes` only marks lockable files | translation |
| `git var GIT_AUTHOR_IDENT` | none | missing | `vcs config get user.name` and `user.email` | translation |
| `git mktag` | none | missing | annotated tags are made with `vcs tag -a` | translation |
<!-- END GENERATED TABLE -->
