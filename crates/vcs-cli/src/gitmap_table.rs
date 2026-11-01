//! The git -> vcs mapping table: single source of truth for docs/git-to-vcs.md and its tests.
//!
//! `vcs git --map` prints it as Markdown. The unit test `table_matches_translation` checks that every
//! non-missing row translates to exactly its `vcs` column and every missing row is refused; the
//! integration test `crates/vcs-cli/tests/gitmap.rs` runs each row's `test` scenario.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Same command and flags (spelling may drop a git-only flag that has no effect).
    Exact,
    /// Different command name or shape, same meaning.
    Renamed,
    /// Works, but the semantics differ in a way the note states.
    Different,
    /// No equivalent; the row names the native way.
    Missing,
}

pub struct Row {
    pub git: &'static str,
    pub vcs: &'static str,
    pub status: Status,
    pub note: &'static str,
    /// Scenario in tests/gitmap.rs that runs this row ("-" = translation test only).
    pub test: &'static str,
}

pub const ROWS: &[Row] = &[
    Row { git: "git init", vcs: "vcs init", status: Status::Exact, note: "", test: "-" },
    Row { git: "git init proj", vcs: "vcs init proj", status: Status::Exact, note: "", test: "-" },
    Row { git: "git init --bare", vcs: "-", status: Status::Missing, note: "a bare repository has no working tree; every vcs repository has one", test: "-" },
    Row { git: "git init -b trunk", vcs: "-", status: Status::Missing, note: "the first branch is always `main`; create others with `vcs switch -c`", test: "-" },
    Row { git: "git clone ssh://host/app.git proj", vcs: "vcs clone ssh://host/app.git proj", status: Status::Exact, note: "only the remotes vcs supports work (path, file://, http(s)://, s3://)", test: "-" },
    Row { git: "git clone /srv/app.git", vcs: "vcs clone /srv/app.git app", status: Status::Exact, note: "the directory name is inferred as git does", test: "-" },
    Row { git: "git clone --depth 1 URL", vcs: "-", status: Status::Missing, note: "vcs has no shallow clones; use `vcs clone --zone Z` / `--for PATH` to download only part", test: "-" },
    Row { git: "git clone --bare URL", vcs: "-", status: Status::Missing, note: "no bare repositories", test: "-" },
    Row { git: "git clone --recurse-submodules URL", vcs: "-", status: Status::Missing, note: "no submodules; zones are cloned together or selected with `--zone`", test: "-" },
    Row { git: "git add f.txt", vcs: "vcs add f.txt", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git add .", vcs: "vcs add .", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git add -A", vcs: "vcs add .", status: Status::Different, note: "git stages the whole tree from any directory; vcs stages the current directory (identical at the repository root)", test: "basics" },
    Row { git: "git add -u", vcs: "vcs add <every modified or deleted tracked file>", status: Status::Exact, note: "tracked files only, as git", test: "basics" },
    Row { git: "git add -p", vcs: "-", status: Status::Missing, note: "no interactive staging; stage whole files, or `vcs stash` / `vcs restore --staged` to adjust", test: "basics" },
    Row { git: "git add -f ignored.log", vcs: "-", status: Status::Missing, note: "ignored files cannot be force-added; edit `.vcsignore`", test: "-" },
    Row { git: "git rm f.txt", vcs: "vcs rm f.txt", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git rm --cached f.txt", vcs: "vcs rm --cached f.txt", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git rm -rf dir", vcs: "vcs rm --force dir", status: Status::Exact, note: "`-r` is implied", test: "basics" },
    Row { git: "git mv a.txt b.txt", vcs: "vcs mv a.txt b.txt", status: Status::Exact, note: "within one zone only; a move across a zone boundary is refused", test: "basics" },
    Row { git: "git mv a.txt b.txt dir/", vcs: "vcs mv a.txt dir/ ; vcs mv b.txt dir/", status: Status::Exact, note: "", test: "-" },
    Row { git: "git status", vcs: "vcs status", status: Status::Exact, note: "the layout is per zone and wording differs; the content is the same: staged, unstaged, untracked", test: "basics" },
    Row { git: "git status -s", vcs: "vcs status (rendered as XY lines)", status: Status::Different, note: "rendered by `vcs git` from `vcs status` data; native `vcs status` has no short form", test: "basics" },
    Row { git: "git status --porcelain", vcs: "vcs status (rendered as XY lines)", status: Status::Different, note: "same two-letter codes for A/M/D/??; no rename detection (git shows a rename as R)", test: "basics" },
    Row { git: "git status -sb", vcs: "vcs status (rendered as XY lines with ## branch)", status: Status::Different, note: "branch line shows only the first zone's branch", test: "basics" },
    Row { git: "git status --ignored", vcs: "-", status: Status::Missing, note: "ignored files are never listed", test: "-" },
    Row { git: "git diff", vcs: "vcs diff", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git diff --staged", vcs: "vcs diff --staged", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git diff --cached", vcs: "vcs diff --staged", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git diff --stat", vcs: "vcs diff --stat", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git diff --name-only", vcs: "vcs diff --name-only", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git diff --name-status", vcs: "vcs diff --name-status", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git diff -U5 -w", vcs: "vcs diff -w -U 5", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git diff --patience", vcs: "vcs diff --diff-algorithm=patience", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git diff --no-color", vcs: "vcs diff --color=never", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git diff -M", vcs: "vcs diff -M", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git diff HEAD~1 HEAD", vcs: "vcs git diff HEAD~1 HEAD", status: Status::Renamed, note: "native `vcs diff` compares only the working tree and the index; `vcs git diff A B` renders two revisions with the same renderer", test: "history" },
    Row { git: "git diff main..feature", vcs: "vcs git diff main feature", status: Status::Renamed, note: "as above", test: "history" },
    Row { git: "git diff HEAD", vcs: "vcs git diff HEAD", status: Status::Renamed, note: "working tree against a revision, tracked files only", test: "history" },
    Row { git: "git diff -- f.txt", vcs: "vcs git diff -- f.txt", status: Status::Renamed, note: "native `vcs diff` has no path filter; `vcs git diff` does", test: "history" },
    Row { git: "git diff --exit-code", vcs: "-", status: Status::Missing, note: "use `vcs diff --name-only` and test for output", test: "basics" },
    Row { git: "git diff --numstat", vcs: "-", status: Status::Missing, note: "use `vcs diff --stat`", test: "-" },
    Row { git: "git diff main...feature", vcs: "vcs git diff main...feature", status: Status::Renamed, note: "merge base of the two against the second; resolved by `vcs git diff` (native `vcs diff` has no revisions)", test: "ranges" },
    Row { git: "git commit -m \"msg\"", vcs: "vcs commit -m msg", status: Status::Exact, note: "commits every zone that has staged changes, atomically", test: "basics" },
    Row { git: "git commit -a -m \"msg\"", vcs: "vcs add <every modified or deleted tracked file> ; vcs commit -m msg", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git commit --amend -m \"msg\"", vcs: "vcs reset --soft HEAD^ && vcs commit -m msg", status: Status::Different, note: "replaces the zone's tip commit with a new one (new commit id, same parents); the old one is reachable only via `vcs op log`; without -m the old message is reused (no editor)", test: "amend" },
    Row { git: "git commit --amend --no-edit", vcs: "vcs reset --soft HEAD^ && vcs commit -m <old message>", status: Status::Different, note: "as above", test: "amend" },
    Row { git: "git commit -S -m \"msg\"", vcs: "vcs commit -S -m msg", status: Status::Different, note: "Ed25519 OpenSSH key signatures, not GPG", test: "-" },
    Row { git: "git commit --no-verify -m \"msg\"", vcs: "vcs --no-hooks commit -m msg", status: Status::Exact, note: "", test: "-" },
    Row { git: "git commit --allow-empty -m \"msg\"", vcs: "-", status: Status::Missing, note: "a commit with no change is refused; make a change or use a tag", test: "-" },
    Row { git: "git commit --author=\"A <a@x>\"", vcs: "-", status: Status::Missing, note: "set `user.name`/`user.email` (or VCS_AUTHOR_*) instead", test: "-" },
    Row { git: "git log", vcs: "vcs log -n 1000000000", status: Status::Exact, note: "git has no default limit; neither does the translation (native `vcs log` shows 20)", test: "history" },
    Row { git: "git log -n 5 --oneline", vcs: "vcs log -n 5 --oneline", status: Status::Exact, note: "", test: "history" },
    Row { git: "git log -5", vcs: "vcs log -n 5", status: Status::Exact, note: "", test: "history" },
    Row { git: "git log --graph --oneline", vcs: "vcs log -n 1000000000 --oneline --graph", status: Status::Exact, note: "", test: "history" },
    Row { git: "git log --author=Dev --since=2024-01-01", vcs: "vcs log -n 1000000000 --author Dev --since 2024-01-01", status: Status::Exact, note: "", test: "history" },
    Row { git: "git log --grep=fix", vcs: "vcs log -n 1000000000 --grep fix", status: Status::Exact, note: "", test: "history" },
    Row { git: "git log -S needle", vcs: "vcs log -n 1000000000 -S needle", status: Status::Exact, note: "", test: "history" },
    Row { git: "git log --follow -- f.txt", vcs: "vcs log -n 1000000000 --follow -- f.txt", status: Status::Exact, note: "", test: "history" },
    Row { git: "git log --name-status", vcs: "vcs log -n 1000000000 --name-status", status: Status::Exact, note: "", test: "history" },
    Row { git: "git log --format=%h", vcs: "vcs log -n 1000000000 --format=%h", status: Status::Exact, note: "vcs placeholders: %H %h %P %p %an %ae %at %ad %s %b %B", test: "history" },
    Row { git: "git log main -- f.txt", vcs: "vcs log -n 1000000000 main -- f.txt", status: Status::Exact, note: "", test: "history" },
    Row { git: "git log -p", vcs: "vcs log + vcs show <each>", status: Status::Different, note: "each commit is printed with `vcs show` layout; zone line instead of git headers", test: "history" },
    Row { git: "git log --stat", vcs: "vcs log + vcs show --stat <each>", status: Status::Different, note: "as above", test: "history" },
    Row { git: "git log --all", vcs: "vcs log -n 1000000000 --all", status: Status::Different, note: "the branches, tags and detached HEAD of the zone you are in (history is per zone); `vcs log --all-zones` interleaves every zone; remote-tracking branches are not included", test: "ranges" },
    Row { git: "git log main..feature", vcs: "vcs log -n 1000000000 main..feature", status: Status::Exact, note: "commit-graph walk by generation number; same commits and order as git (commit time ties may order differently)", test: "ranges" },
    Row { git: "git log main...feature", vcs: "vcs log -n 1000000000 main...feature", status: Status::Exact, note: "symmetric difference", test: "ranges" },
    Row { git: "git log ^main feature", vcs: "vcs log -n 1000000000 ^main feature", status: Status::Exact, note: "", test: "ranges" },
    Row { git: "git log feature --not main", vcs: "vcs log -n 1000000000 feature ^main", status: Status::Exact, note: "`--not` is folded into `^rev`", test: "ranges" },
    Row { git: "git log --reverse main..feature", vcs: "vcs log -n 1000000000 --reverse main..feature", status: Status::Exact, note: "", test: "ranges" },
    Row { git: "git log -n 2 main...feature", vcs: "vcs log -n 2 main...feature", status: Status::Exact, note: "", test: "ranges" },
    Row { git: "git rev-list --count main..feature", vcs: "vcs rev-list --count main..feature", status: Status::Exact, note: "", test: "ranges" },
    Row { git: "git rev-list --all", vcs: "vcs rev-list --all", status: Status::Exact, note: "vcs commit ids, not git ids; same set and order", test: "ranges" },
    Row { git: "git shortlog -sn main..feature", vcs: "vcs shortlog -s -n main..feature", status: Status::Exact, note: "", test: "ranges" },
    Row { git: "git log -G regex", vcs: "-", status: Status::Missing, note: "`vcs log -S string` or `vcs grep regex <rev>`", test: "-" },
    Row { git: "git show", vcs: "vcs show", status: Status::Exact, note: "", test: "history" },
    Row { git: "git show HEAD~1", vcs: "vcs show HEAD~1", status: Status::Exact, note: "", test: "history" },
    Row { git: "git show --stat HEAD", vcs: "vcs show --stat HEAD", status: Status::Exact, note: "", test: "history" },
    Row { git: "git show HEAD:f.txt", vcs: "vcs cat-file -p HEAD:f.txt", status: Status::Renamed, note: "", test: "history" },
    Row { git: "git branch", vcs: "vcs branch", status: Status::Exact, note: "lists the branches of the current zone", test: "basics" },
    Row { git: "git branch feature", vcs: "vcs branch feature", status: Status::Exact, note: "creates at the checked-out head", test: "basics" },
    Row { git: "git branch -d feature", vcs: "vcs branch -d feature", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git branch -D feature", vcs: "vcs branch -d --force feature", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git branch --show-current", vcs: "vcs branch (the * line)", status: Status::Renamed, note: "", test: "basics" },
    Row { git: "git branch -m main trunk", vcs: "vcs branch -m main trunk", status: Status::Exact, note: "refs and workspace move, no file is touched; an op-log entry makes it undoable (`vcs op undo`); refused while a merge or rebase is in progress", test: "detach" },
    Row { git: "git branch -M rescue side", vcs: "vcs branch -M rescue side", status: Status::Exact, note: "replaces an existing branch, never the checked-out one", test: "detach" },
    Row { git: "git branch -r", vcs: "-", status: Status::Missing, note: "`vcs remote show` lists remote-tracking refs", test: "-" },
    Row { git: "git branch topic3 main~1", vcs: "vcs branch topic3 main~1", status: Status::Exact, note: "start is any revision: branch, tag, commit-id prefix, `HEAD~2`", test: "detach" },
    Row { git: "git switch main", vcs: "vcs switch main", status: Status::Exact, note: "a dirty zone is refused instead of carrying changes along", test: "basics" },
    Row { git: "git switch -c feature", vcs: "vcs switch -c feature", status: Status::Exact, note: "", test: "merge" },
    Row { git: "git switch --detach HEAD~1", vcs: "vcs switch --detach HEAD~1", status: Status::Exact, note: "detached HEAD is per zone: the zone you are in is detached, other zones keep their branch; commits made there go to a hidden per-zone ref (kept by gc, never published); `vcs status` says `HEAD detached`; leaving lists commits no branch reaches and how to keep them; `vcs op undo` brings the detached state back", test: "detach" },
    Row { git: "git switch -c rescue", vcs: "vcs switch -c rescue", status: Status::Exact, note: "from a detached HEAD this keeps the commits made there", test: "detach" },
    Row { git: "git switch -c topic2 main", vcs: "vcs branch topic2 main ; vcs switch topic2", status: Status::Renamed, note: "", test: "detach" },
    Row { git: "git checkout main", vcs: "vcs switch main  (or vcs switch --detach for a tag or commit, vcs restore for a file)", status: Status::Renamed, note: "a dirty zone is refused instead of carrying changes along", test: "basics" },
    Row { git: "git checkout v1.0", vcs: "vcs switch v1.0  (or vcs switch --detach for a tag or commit, vcs restore for a file)", status: Status::Renamed, note: "a tag or commit id checks out as a detached HEAD (see `git switch --detach`)", test: "detach" },
    Row { git: "git checkout -b feature", vcs: "vcs switch -c feature", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git checkout -- f.txt", vcs: "vcs restore f.txt", status: Status::Renamed, note: "", test: "basics" },
    Row { git: "git checkout HEAD -- f.txt", vcs: "vcs restore f.txt", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git checkout HEAD~1 -- f.txt", vcs: "vcs cat-file -p HEAD~1:<path> into the file + vcs add for f.txt", status: Status::Renamed, note: "writes the file from the revision and stages it, as git", test: "history" },
    Row { git: "git checkout --ours f.txt", vcs: "vcs resolve --ours f.txt", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git checkout --detach v1.0", vcs: "vcs switch --detach v1.0", status: Status::Renamed, note: "see `git switch --detach`", test: "detach" },
    Row { git: "git checkout -b feature2 main", vcs: "vcs branch feature2 main ; vcs switch feature2", status: Status::Renamed, note: "", test: "detach" },
    Row { git: "git restore f.txt", vcs: "vcs restore f.txt", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git restore --staged f.txt", vcs: "vcs restore --staged f.txt", status: Status::Exact, note: "", test: "basics" },
    Row { git: "git restore -S -W f.txt", vcs: "vcs restore --staged f.txt ; vcs restore f.txt", status: Status::Exact, note: "", test: "-" },
    Row { git: "git restore --source=HEAD~1 f.txt", vcs: "vcs cat-file -p HEAD~1:<path> into the file for f.txt", status: Status::Renamed, note: "writes the file only (not staged), as git", test: "history" },
    Row { git: "git restore -p f.txt", vcs: "-", status: Status::Missing, note: "no interactive restore", test: "-" },
    Row { git: "git reset", vcs: "vcs reset", status: Status::Different, note: "acts on ONE zone (the cwd zone; --zone Z for another) because each zone has its own head", test: "reset" },
    Row { git: "git reset --soft HEAD~1", vcs: "vcs reset --soft HEAD~1", status: Status::Different, note: "as above", test: "reset" },
    Row { git: "git reset --mixed HEAD~1", vcs: "vcs reset --mixed HEAD~1", status: Status::Different, note: "as above", test: "reset" },
    Row { git: "git reset --hard HEAD~1", vcs: "vcs reset --hard HEAD~1", status: Status::Different, note: "as above; discards the zone's uncommitted work", test: "reset" },
    Row { git: "git reset HEAD -- f.txt", vcs: "vcs reset HEAD -- f.txt", status: Status::Exact, note: "unstages the file (same as `vcs restore --staged`)", test: "basics" },
    Row { git: "git reset --hard HEAD@{1}", vcs: "-", status: Status::Missing, note: "`vcs op log`, then `vcs op undo` / `vcs op restore <id>`", test: "-" },
    Row { git: "git revert HEAD", vcs: "vcs revert HEAD", status: Status::Exact, note: "", test: "pick" },
    Row { git: "git revert --no-edit HEAD~2", vcs: "vcs revert HEAD~2", status: Status::Exact, note: "", test: "pick" },
    Row { git: "git revert -n HEAD", vcs: "-", status: Status::Missing, note: "a revert always commits", test: "-" },
    Row { git: "git cherry-pick feature", vcs: "vcs cherry-pick feature", status: Status::Exact, note: "", test: "pick" },
    Row { git: "git cherry-pick --continue", vcs: "vcs commit", status: Status::Exact, note: "", test: "-" },
    Row { git: "git cherry-pick A..B", vcs: "-", status: Status::Missing, note: "one `vcs cherry-pick` per commit", test: "-" },
    Row { git: "git merge feature", vcs: "vcs merge feature", status: Status::Exact, note: "fast-forward when possible, else a merge commit", test: "merge" },
    Row { git: "git merge --abort", vcs: "vcs merge --abort", status: Status::Exact, note: "", test: "merge" },
    Row { git: "git merge --continue", vcs: "vcs commit", status: Status::Exact, note: "", test: "-" },
    Row { git: "git merge origin/main", vcs: "vcs merge --remote main", status: Status::Renamed, note: "merges the remote-tracking branch as last fetched", test: "-" },
    Row { git: "git merge --no-ff feature", vcs: "-", status: Status::Missing, note: "no way to force a merge commit when a fast-forward is possible", test: "-" },
    Row { git: "git merge --squash feature", vcs: "-", status: Status::Missing, note: "no squash merge", test: "-" },
    Row { git: "git rebase main", vcs: "vcs rebase main", status: Status::Exact, note: "keeps each change id", test: "merge" },
    Row { git: "git rebase --continue", vcs: "vcs rebase --continue", status: Status::Exact, note: "", test: "-" },
    Row { git: "git rebase --abort", vcs: "vcs rebase --abort", status: Status::Exact, note: "", test: "merge" },
    Row { git: "git rebase -i HEAD~3", vcs: "-", status: Status::Missing, note: "no interactive rebase", test: "-" },
    Row { git: "git rebase origin/main", vcs: "-", status: Status::Missing, note: "`vcs sync`, then `vcs rebase <local branch>`", test: "-" },
    Row { git: "git stash", vcs: "vcs stash push", status: Status::Exact, note: "tracked changes only; untracked files stay", test: "stash" },
    Row { git: "git stash push -m wip", vcs: "vcs stash push -m wip", status: Status::Exact, note: "", test: "stash" },
    Row { git: "git stash pop", vcs: "vcs stash pop", status: Status::Different, note: "the restored changes come back STAGED (git leaves them unstaged)", test: "stash" },
    Row { git: "git stash apply stash@{0}", vcs: "vcs stash apply 0", status: Status::Different, note: "keeps the entry; the restored changes come back STAGED (git leaves them unstaged)", test: "stash" },
    Row { git: "git stash list", vcs: "vcs stash list", status: Status::Exact, note: "", test: "stash" },
    Row { git: "git stash drop", vcs: "vcs stash drop", status: Status::Exact, note: "", test: "stash" },
    Row { git: "git stash -u", vcs: "-", status: Status::Missing, note: "untracked files cannot be stashed; `vcs add` them first", test: "-" },
    Row { git: "git tag", vcs: "vcs tag", status: Status::Exact, note: "", test: "tags" },
    Row { git: "git tag v1", vcs: "vcs tag v1", status: Status::Exact, note: "", test: "tags" },
    Row { git: "git tag -a v1 -m rel", vcs: "vcs tag v1 -m rel", status: Status::Exact, note: "annotated", test: "-" },
    Row { git: "git tag v1 HEAD~1", vcs: "vcs tag v1 HEAD~1", status: Status::Exact, note: "", test: "-" },
    Row { git: "git tag -d v1", vcs: "vcs tag -d v1", status: Status::Exact, note: "", test: "-" },
    Row { git: "git tag -s v1 -m rel", vcs: "vcs tag v1 -m rel -s", status: Status::Different, note: "Ed25519 OpenSSH key, not GPG", test: "-" },
    Row { git: "git tag -v v1", vcs: "vcs verify-tag v1", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git tag -f v1", vcs: "-", status: Status::Missing, note: "`vcs tag -d v1` then `vcs tag v1`", test: "-" },
    Row { git: "git remote add origin /srv/app", vcs: "vcs remote set /srv/app", status: Status::Renamed, note: "vcs has exactly one remote", test: "-" },
    Row { git: "git remote -v", vcs: "vcs remote show", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git remote set-url origin /srv/b", vcs: "vcs remote set /srv/b", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git remote remove origin", vcs: "-", status: Status::Missing, note: "`vcs remote set <url>` replaces the one remote", test: "-" },
    Row { git: "git fetch", vcs: "vcs fetch", status: Status::Exact, note: "local branches stay untouched", test: "-" },
    Row { git: "git fetch origin", vcs: "vcs fetch", status: Status::Exact, note: "", test: "-" },
    Row { git: "git fetch --depth 1", vcs: "-", status: Status::Missing, note: "no shallow fetch", test: "-" },
    Row { git: "git pull", vcs: "vcs sync", status: Status::Different, note: "fetch + fast-forward only; diverged branches are left alone, nothing is merged (`vcs merge --remote <branch>`)", test: "-" },
    Row { git: "git pull --rebase", vcs: "-", status: Status::Missing, note: "`vcs fetch`, `vcs merge --remote <branch>`, or `vcs sync` then `vcs rebase <branch>`", test: "-" },
    Row { git: "git push", vcs: "vcs publish", status: Status::Different, note: "publishes ALL checked-out branches in one atomic transaction; fast-forward only", test: "-" },
    Row { git: "git push -u origin main", vcs: "vcs publish", status: Status::Different, note: "the branch argument is ignored; no upstream tracking setting", test: "-" },
    Row { git: "git push --force", vcs: "-", status: Status::Missing, note: "publish is fast-forward only", test: "-" },
    Row { git: "git push --tags", vcs: "-", status: Status::Missing, note: "tags travel with `vcs publish`", test: "-" },
    Row { git: "git blame f.txt", vcs: "vcs blame f.txt", status: Status::Exact, note: "", test: "history" },
    Row { git: "git blame -M f.txt", vcs: "vcs blame -M f.txt", status: Status::Exact, note: "", test: "history" },
    Row { git: "git blame HEAD~1 -- f.txt", vcs: "vcs blame --rev HEAD~1 f.txt", status: Status::Exact, note: "", test: "history" },
    Row { git: "git blame -L 1,5 f.txt", vcs: "-", status: Status::Missing, note: "no line range; pipe `vcs blame` through sed", test: "-" },
    Row { git: "git bisect start HEAD HEAD~5", vcs: "vcs bisect start HEAD HEAD~5", status: Status::Exact, note: "", test: "-" },
    Row { git: "git bisect good", vcs: "vcs bisect good", status: Status::Exact, note: "", test: "-" },
    Row { git: "git bisect bad", vcs: "vcs bisect bad", status: Status::Exact, note: "", test: "-" },
    Row { git: "git bisect skip", vcs: "vcs bisect skip", status: Status::Exact, note: "", test: "-" },
    Row { git: "git bisect run ./t.sh", vcs: "vcs bisect run ./t.sh", status: Status::Exact, note: "", test: "-" },
    Row { git: "git bisect reset", vcs: "vcs bisect reset", status: Status::Exact, note: "", test: "-" },
    Row { git: "git bisect log", vcs: "-", status: Status::Missing, note: "no bisect log", test: "-" },
    Row { git: "git reflog", vcs: "vcs op log", status: Status::Renamed, note: "repository-wide operation log, not one log per ref", test: "-" },
    Row { git: "git reflog -n 5", vcs: "vcs op log -n 5", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git clean -n", vcs: "vcs clean -n", status: Status::Exact, note: "", test: "clean" },
    Row { git: "git clean -fd", vcs: "vcs clean -f -d", status: Status::Exact, note: "", test: "clean" },
    Row { git: "git clean -fx", vcs: "-", status: Status::Missing, note: "ignored files are never removed", test: "-" },
    Row { git: "git grep -n needle", vcs: "vcs grep -n needle", status: Status::Exact, note: "Rust regex syntax", test: "grep" },
    Row { git: "git grep -i -e needle HEAD", vcs: "vcs grep -i needle HEAD", status: Status::Exact, note: "", test: "grep" },
    Row { git: "git grep -F a.b -- src", vcs: "vcs grep -F a.b -- src", status: Status::Exact, note: "", test: "grep" },
    Row { git: "git grep --cached x", vcs: "-", status: Status::Missing, note: "searches the working tree or a revision only", test: "-" },
    Row { git: "git describe --tags", vcs: "vcs describe --tags", status: Status::Exact, note: "", test: "tags" },
    Row { git: "git describe --always", vcs: "vcs describe --always", status: Status::Exact, note: "", test: "tags" },
    Row { git: "git describe --dirty", vcs: "-", status: Status::Missing, note: "no --dirty suffix", test: "-" },
    Row { git: "git archive -o out.tar HEAD", vcs: "vcs archive -o out.tar HEAD", status: Status::Exact, note: "", test: "-" },
    Row { git: "git archive --format=zip --prefix=p/ HEAD", vcs: "vcs archive --format zip --prefix p/ HEAD", status: Status::Exact, note: "", test: "-" },
    Row { git: "git worktree add ../wt feature", vcs: "vcs worktree add ../wt feature", status: Status::Exact, note: "", test: "-" },
    Row { git: "git worktree add -b new ../wt", vcs: "vcs worktree add ../wt new", status: Status::Exact, note: "", test: "-" },
    Row { git: "git worktree list", vcs: "vcs worktree list", status: Status::Exact, note: "", test: "-" },
    Row { git: "git worktree remove ../wt", vcs: "vcs worktree remove ../wt", status: Status::Exact, note: "", test: "-" },
    Row { git: "git worktree prune", vcs: "-", status: Status::Missing, note: "`vcs worktree remove`", test: "-" },
    Row { git: "git submodule add URL path", vcs: "-", status: Status::Missing, note: "no nested repositories. A zone is a path-prefix with its own branches and history inside the SAME repository: `vcs zone create <name> --path <dir>`; subscribe with `vcs zone add`, clone parts with `vcs clone --zone`", test: "-" },
    Row { git: "git submodule update --init", vcs: "-", status: Status::Missing, note: "`vcs zone add <name>` materializes a zone", test: "-" },
    Row { git: "git sparse-checkout set dir", vcs: "-", status: Status::Missing, note: "`vcs sparse set <zone> <dir>` (per zone)", test: "-" },
    Row { git: "git config user.name Dev", vcs: "vcs config set user.name Dev", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git config --global user.name Dev", vcs: "vcs config set user.name Dev --user", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git config user.name", vcs: "vcs config get user.name", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git config --list", vcs: "vcs config list", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git config --unset user.name", vcs: "vcs config unset user.name", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git config --system x y", vcs: "-", status: Status::Missing, note: "no system scope", test: "-" },
    Row { git: "git ls-files", vcs: "vcs ls-files", status: Status::Exact, note: "", test: "-" },
    Row { git: "git ls-files -s", vcs: "vcs ls-files -s", status: Status::Exact, note: "sha256 object ids, not sha1", test: "-" },
    Row { git: "git ls-files -o", vcs: "-", status: Status::Missing, note: "`vcs status` lists untracked files", test: "-" },
    Row { git: "git ls-tree -r --name-only HEAD", vcs: "vcs ls-files --rev HEAD", status: Status::Renamed, note: "always recursive", test: "-" },
    Row { git: "git ls-tree HEAD", vcs: "vcs ls-files -s --rev HEAD", status: Status::Different, note: "flat recursive listing, not one directory level", test: "-" },
    Row { git: "git cat-file -p HEAD:f.txt", vcs: "vcs cat-file -p HEAD:f.txt", status: Status::Exact, note: "", test: "-" },
    Row { git: "git cat-file -t HEAD", vcs: "vcs cat-file -t HEAD", status: Status::Exact, note: "", test: "-" },
    Row { git: "git cat-file --batch", vcs: "-", status: Status::Missing, note: "one object per call", test: "-" },
    Row { git: "git rev-parse HEAD", vcs: "vcs rev-parse HEAD", status: Status::Exact, note: "64-hex ids", test: "-" },
    Row { git: "git rev-parse --short HEAD", vcs: "vcs rev-parse --short HEAD", status: Status::Exact, note: "", test: "-" },
    Row { git: "git rev-parse --show-toplevel", vcs: "vcs rev-parse --show-toplevel", status: Status::Exact, note: "", test: "-" },
    Row { git: "git rev-parse --abbrev-ref HEAD", vcs: "vcs branch (the * line)", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git rev-parse --git-dir", vcs: "-", status: Status::Missing, note: "state lives in `<worktree>/.vcs`", test: "-" },
    Row { git: "git shortlog -sn", vcs: "vcs shortlog -s -n", status: Status::Exact, note: "", test: "-" },
    Row { git: "git merge-base main feature", vcs: "vcs merge-base main feature", status: Status::Exact, note: "", test: "-" },
    Row { git: "git diff-tree -r A B", vcs: "vcs diff-tree A B", status: Status::Different, note: "prints `A/D/M path` lines, not git's raw format", test: "-" },
    Row { git: "git fast-export", vcs: "vcs export-git", status: Status::Renamed, note: "one zone branch as a fast-import stream", test: "-" },
    Row { git: "git fast-import", vcs: "vcs import-git -", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git bundle create f.bundle main", vcs: "vcs bundle create f.bundle main", status: Status::Exact, note: "vcs bundle format, not git's", test: "-" },
    Row { git: "git bundle verify f.bundle", vcs: "vcs bundle verify f.bundle", status: Status::Exact, note: "", test: "-" },
    Row { git: "git gc", vcs: "vcs gc", status: Status::Different, note: "deletes unreachable objects older than 14 days (grace period); `--dry-run` previews", test: "-" },
    Row { git: "git gc --auto", vcs: "vcs repack --auto", status: Status::Renamed, note: "", test: "-" },
    Row { git: "git fsck", vcs: "vcs fsck", status: Status::Exact, note: "", test: "-" },
    Row { git: "git repack -ad", vcs: "vcs repack", status: Status::Exact, note: "", test: "-" },
    Row { git: "git format-patch -1", vcs: "-", status: Status::Missing, note: "no patch files. Exchange commits with `vcs bundle create` + `vcs fetch <file>` or a shared remote", test: "-" },
    Row { git: "git am x.patch", vcs: "-", status: Status::Missing, note: "as format-patch", test: "-" },
    Row { git: "git apply x.patch", vcs: "-", status: Status::Missing, note: "as format-patch", test: "-" },
    Row { git: "git mergetool", vcs: "-", status: Status::Missing, note: "`vcs conflict show <path>`, edit, `vcs resolve --mark <path>`", test: "-" },
    Row { git: "git lfs track", vcs: "-", status: Status::Missing, note: "large files are chunked natively", test: "-" },
    Row { git: "git filter-branch", vcs: "-", status: Status::Missing, note: "`vcs zone apply` splits history by path; otherwise none", test: "-" },
    Row { git: "git notes add", vcs: "-", status: Status::Missing, note: "none", test: "-" },
];
/// `(exact, renamed, different, missing)` row counts.
pub fn counts() -> (usize, usize, usize, usize) {
    let n = |s| ROWS.iter().filter(|r| r.status == s).count();
    (n(Status::Exact), n(Status::Renamed), n(Status::Different), n(Status::Missing))
}

#[cfg(test)]
/// Split a git command line the way a shell would for our purposes: whitespace, with `"..."` and `'...'` grouping.
pub fn words(line: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() || any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() || any {
        out.push(cur);
    }
    out
}

fn label(s: Status) -> &'static str {
    match s {
        Status::Exact => "exact",
        Status::Renamed => "renamed",
        Status::Different => "different",
        Status::Missing => "missing",
    }
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|")
}

/// The mapping table as Markdown (the generated part of docs/git-to-vcs.md).
pub fn markdown() -> String {
    let (e, r, d, m) = counts();
    let mut s = format!(
        "{} rows: {e} exact, {r} renamed, {d} different, {m} missing.\n\n| git | vcs | status | notes | test |\n|---|---|---|---|---|\n",
        ROWS.len()
    );
    for row in ROWS {
        let vcs = if row.status == Status::Missing {
            "none".to_owned()
        } else {
            format!("`{}`", cell(row.vcs))
        };
        let test = if row.test == "-" {
            "translation".to_owned()
        } else {
            format!("`{}`", row.test)
        };
        s.push_str(&format!(
            "| `{}` | {} | {} | {} | {} |\n",
            cell(row.git),
            vcs,
            label(row.status),
            cell(row.note),
            test
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitmap::{render, translate};

    #[test]
    fn table_matches_translation() {
        let mut bad = vec![];
        for row in ROWS {
            let args = words(row.git);
            assert_eq!(args[0], "git", "{}", row.git);
            let r = translate(&args[1..]);
            match (row.status, r) {
                (Status::Missing, Ok(steps)) => bad.push(format!("{:?} should be refused but gave {}", row.git, render(&steps))),
                (Status::Missing, Err(e)) => {
                    let m = e.to_string();
                    if !m.contains("no vcs equivalent") {
                        bad.push(format!("{:?}: wrong refusal text: {m}", row.git));
                    }
                }
                (_, Err(e)) => bad.push(format!("{:?} failed to translate: {e}", row.git)),
                (_, Ok(steps)) => {
                    let got = render(&steps);
                    if got != row.vcs {
                        bad.push(format!("{:?}\n    table: {}\n    gave : {got}", row.git, row.vcs));
                    }
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn doc_table_is_current() {
        let doc = include_str!("../../../docs/git-to-vcs.md");
        let a = doc.find("<!-- BEGIN GENERATED TABLE -->\n").unwrap() + "<!-- BEGIN GENERATED TABLE -->\n".len();
        let b = doc.find("<!-- END GENERATED TABLE -->").unwrap();
        assert_eq!(
            &doc[a..b],
            markdown(),
            "docs/git-to-vcs.md is stale: replace the generated part with `vcs git --map`"
        );
    }

    #[test]
    fn rows_are_unique_and_documented() {
        let mut seen = std::collections::BTreeSet::new();
        for r in ROWS {
            assert!(seen.insert(r.git), "duplicate row {}", r.git);
            if r.status == Status::Missing || r.status == Status::Different {
                assert!(!r.note.is_empty(), "{} needs a note", r.git);
            }
        }
        assert!(ROWS.len() >= 80);
    }
}
