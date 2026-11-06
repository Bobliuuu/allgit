# Choosing hunks (`-p`) and merge tools

`vcs add -p`, `vcs restore -p [--staged]`, `vcs checkout -p [<rev>]` and `vcs stash push -p` go through the
changed hunks of each file and ask what to do with every one. `vcs mergetool` starts a merge tool on each
conflicted file. Both read their answers from standard input, so a script can drive them: one answer per
line, and the end of input counts as `q`.

## Hunks

Hunks come from the same line diff as `vcs diff` (Myers, three lines of context; changes with at most six
unchanged lines between them share a hunk, as in git). For each hunk the prompt is

```text
(2/3) Stage this hunk [y,n,q,a,d,k,K,g,s,e,?]?
```

| answer | meaning |
|---|---|
| `y` / `n` | use / skip this hunk |
| `q` | quit: this hunk and every later one, in this file and the rest, are skipped |
| `a` / `d` | use / skip this hunk and all later hunks of this file |
| `s` | split the hunk where unchanged lines separate its changes |
| `e` | edit the hunk in `core.editor` (`VCS_EDITOR`, `$VISUAL`, `$EDITOR`): delete `+` lines, turn `-` into context; the old side must stay intact |
| `j` `J` `k` `K` | move to the next / previous undecided hunk / hunk, leaving this one undecided |
| `g` | list the hunks and go to one |
| `?` | help |

| command | offered diff | a used hunk is... | `e` |
|---|---|---|---|
| `add -p [paths]` | index -> working tree | staged (the working file is not touched) | yes |
| `restore -p paths` | index -> working tree | discarded from the working file | no |
| `restore --staged -p paths` | HEAD -> index | unstaged | no |
| `checkout -p [-- paths]` | index -> working tree | discarded from the working file | no |
| `checkout -p HEAD -- paths` | HEAD -> working tree | discarded from index and working file | no |
| `checkout -p <rev> -- paths` | working tree -> `<rev>` | taken from `<rev>` into index and working file | yes |
| `stash push -p [-- paths]` | HEAD -> working tree | stashed; the rest stays as unstaged changes, the index is reset to HEAD | no |

`vcs checkout` exists only for `-p`; use `vcs switch` to change branch and `vcs restore` for files.
Without a path, `add -p`, `checkout -p` and `stash push -p` go through every changed tracked file.

Not offered (a note names the file): binary files and symbolic links. Untracked files are never offered
(`stash` has no `-p -u`). Not supported, so not faked: the `/` search command, mode-change hunks and
intent-to-add. `checkout -p <rev>` refuses a file whose index and working copy differ (it would have to
change both); `stash push -p` refuses while a new file is staged, because the stash would take it whole.
If anything fails while `stash push -p` is moving files around, every changed file is put back as it was.

## `vcs mergetool`

```sh
vcs config set --user merge.tool mytool
vcs config set --user mergetool.mytool.cmd 'mytool "$LOCAL" "$BASE" "$REMOTE" -o "$MERGED"'
vcs merge topic            # conflicts
vcs mergetool              # one run per unresolved file; -y skips the "hit return" prompt
vcs commit -m merged
```

The command runs through `sh -c` in the worktree root with `$BASE` (common ancestor, empty for add/add),
`$LOCAL` (ours), `$REMOTE` (theirs) and `$MERGED` (the working file, with conflict markers) set; it writes
the result to `$MERGED`. Afterwards:

* with `mergetool.<name>.trustExitCode on` the exit status decides; otherwise, as in git, a changed
  `$MERGED` is a success, and an unchanged one asks `Was the merge successful? [y/n]` on standard input;
* a result that still holds conflict markers is never accepted;
* the file is staged and marked resolved (`vcs resolve --check`), and `<file>.orig` keeps the conflicted
  text unless `mergetool.keepBackup off`.

`mergetool.<name>.cmd` is a command, so it is read from the user or system config only; a repository's
`.vcs/config` is ignored for it. There is no built-in tool list (no `vimdiff` or `meld` presets): name the
command yourself. Conflicts where one side deleted or renamed the file are skipped with a pointer to
`vcs resolve --ours|--theirs`; the exit status is non-zero while anything stays unresolved.
