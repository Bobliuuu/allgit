# 1. Install and your first repository

This chapter gets `vcs` onto your machine and makes one commit.

All sessions in this guide were run on a Linux box (Ubuntu, 30 cores) with a release build of the current `main`.
Output is pasted as it came out. Commit ids and timestamps change on every run, so yours will differ.
Paths like `/home/ubuntu/...` are that box's paths.

## Install

There are two ways. Both need nothing but the binary. `vcs` never calls `git`.

**From a release.** Tagged versions publish tarballs for Linux and macOS with a `SHA256SUMS` file.
The steps are in [../install.md](../install.md). *Not run for this guide: no release tag has been cut yet.*

**From source.** You need Rust 1.80 or newer.

```sh
git clone https://github.com/Bobliuuu/allgit && cd allgit
cargo build --release          # binaries land in target/release
cargo install --path crates/vcs-cli --locked    # or install vcs onto your PATH
```

*Run for this guide: `cargo build --release --workspace`. It finished and produced `vcs`, `vcs-server`,
`git-remote-allgit` and `vcs-assist-claude`. `cargo install` was not run.*

```text
$ vcs --version
vcs 0.1.0
```

Windows is experimental and has only been compile-checked. See [../install.md](../install.md).

Shell completions and a man page come from the binary:

```text
$ vcs gen completions bash > vcs.bash && head -3 vcs.bash
_vcs() {
    local i cur prev opts cmd
    COMPREPLY=()
```

Use `vcs gen man` for the man page.

## Tell vcs who you are

Every commit needs an author. Set it once for your user:

```text
$ vcs config set --user user.name "Ada Lovelace"
$ vcs config set --user user.email ada@example.com
$ vcs config get user.name
Ada Lovelace
```

You can also set `user.name` and `user.email` for one repository (leave out `--user`),
or use the environment variables `VCS_AUTHOR_NAME` and `VCS_AUTHOR_EMAIL`.
Environment variables win over the repository, and the repository wins over your user config.

```text
$ echo more >> hello.txt && vcs add hello.txt && VCS_AUTHOR_NAME="Grace Hopper" VCS_AUTHOR_EMAIL=grace@example.com vcs commit -m "env identity" && vcs log --oneline
staged 1 file(s), 0 removal(s)
[root/main 1d4195c7ebd6] env identity
1d4195c7ebd6 env identity
8983dacc3600 first commit
```

If nothing is set, `vcs` stops and tells you what to do:

```text
$ VCS_CONFIG_HOME=/nonexistent vcs commit -m x
error: user.name and user.email are not set

Tell vcs who you are:
  vcs config set --user user.name "Your Name"
  vcs config set --user user.email you@example.com
(drop --user to set it for this repository only)
[exit 1]
```

`vcs config list` shows every effective setting and where it came from:

```text
$ vcs config list
user.email=ada@example.com  (user)
user.name=Ada Lovelace  (user)
```

## Make a repository

```text
$ vcs init hello
Initialized empty vcs repository in /home/ubuntu/guide-run/ch01/hello/.vcs
$ echo "hello, world" > hello.txt
$ mkdir src && echo "fn main() {}" > src/main.rs
$ vcs status
zone root: branch main @ (none)
untracked:
    hello.txt
    src/
$ vcs add .
staged 2 file(s), 0 removal(s)
$ vcs status
zone root: branch main @ (none)
  staged:
    new file:  hello.txt
    new file:  src/main.rs
$ vcs commit -m "first commit"
[root/main 8983dacc3600] first commit
$ vcs log
commit 8983dacc3600a818bdd357cfdd6b65990ce788286057ca79d74329820c78a2bd
change 87d1d67487b9d612bc69bb7f37eb58be   zone root
Author: Ada Lovelace <ada@example.com>
Date:   1793500016
    first commit

$ ls -a
.
..
.vcs
hello.txt
src
$ vcs fsck
checked 8 objects
ok
```

What happened:

- `vcs init hello` made a directory with a `.vcs` folder in it. That folder is the whole repository.
- `vcs status` listed two untracked paths. The first line says `zone root`. Zones are explained in
  [chapter 4](04-zones.md). For now: there is one zone, called `root`, and it owns everything.
- `vcs add .` staged both files. `vcs commit` wrote one commit on branch `main`.
- `vcs fsck` checked every object. Run it any time you want to be sure.

A commit id is a 64 hex digit BLAKE3 hash. Output usually shows the first 12 digits.
The `change` line is a second id that stays the same when a commit is rebased.

Next: [chapter 2, the daily workflow](02-daily-workflow.md).
