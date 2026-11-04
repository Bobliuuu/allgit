# 4. Zones

A zone is a part of the repository that owns a path prefix and has its own history and branches.
Think of a game studio. Programmers, artists and tool authors all work in one tree,
but they should not have to download each other's data or wait for each other's merges.

## The game studio example

The studio has one project with three kinds of work:

| Zone | Path | Who works there | What it holds |
| --- | --- | --- | --- |
| `engine` | `engine/` | programmers | source code |
| `assets` | `assets/` | artists | big binary files |
| `tools` | `tools/` | build engineers | scripts |
| `root` | everything else | everyone | README, config |

`root` always exists. It owns whatever no other zone owns.

What zones give you:

- **Separate history.** `vcs log --zone assets` shows only art commits.
- **Separate branches.** The artists can have a branch `art-wip` while the engine stays on `main`.
- **Subscriptions.** A programmer can leave `assets` out of their checkout.
  Their disk and their network never carry the textures.
- **One atomic commit.** A change that spans zones is still one transaction.

## Create zones

A zone owns a path prefix. Zones may not overlap.

```text
$ vcs init studio
Initialized empty vcs repository in /home/ubuntu/guide-run/ch04/studio/.vcs
$ vcs zone create engine --path engine
created zone engine owning engine/
$ vcs zone create assets --path assets
created zone assets owning assets/
$ vcs zone create tools --path tools
created zone tools owning tools/
$ vcs zone list
assets         assets               subscribed, on main; 0 branch(es)
engine         engine               subscribed, on main; 0 branch(es)
root           (rest of repo)       subscribed, on main; 0 branch(es)
tools          tools                subscribed, on main; 0 branch(es)
$ vcs zone show assets
zone:   assets
path:   assets
state:  subscribed, branch main
* assets/main                     (none)
$ vcs zone create overlap --path engine/render
error: path "engine/render" overlaps zone "engine" (path "engine"); zones may not overlap
[exit 1]
```

`zone list` shows the state of each zone for you (`subscribed`, the branch, how many branches).

## One commit, many zones

Write files in several zones, add them, and commit once. `vcs commit` makes one commit per touched zone
and ties them together in one transaction. All of them land or none do.

```text
$ mkdir -p engine/render assets/textures tools && echo "fn render() {}" > engine/render/mod.rs && echo "PNGDATA-v1" > assets/textures/hero.png && echo "#!/bin/sh" > tools/build.sh && echo "# Studio" > README.md
$ vcs status
zone assets (assets/): branch main @ (none)
zone engine (engine/): branch main @ (none)
zone root: branch main @ (none)
zone tools (tools/): branch main @ (none)
untracked:
    README.md
    assets/
    engine/
    tools/
$ vcs add .
staged 4 file(s), 0 removal(s)
$ vcs commit -m "first playable"
[assets/main 84825b00926a] first playable
[engine/main 1efc9815c984] first playable
[root/main 44e32cf0e1c5] first playable
[tools/main a34df4eb1326] first playable
atomic transaction 73cdb57f05d9 covering 4 zones
$ vcs log --zone engine --oneline
1efc9815c984 first playable
$ vcs log --zone assets --oneline
84825b00926a first playable
$ vcs log --oneline
44e32cf0e1c5 first playable
```

The same files, a second time. This change touches `engine` and `assets` only, so only two commits are made
and the transaction covers two zones.

```text
$ echo "fn render() { draw(); }" > engine/render/mod.rs && echo "PNGDATA-v2" > assets/textures/hero.png
$ vcs add .
staged 4 file(s), 0 removal(s)
$ vcs commit -m "hero gets a new look and a new draw call"
[assets/main ef1103e58356] hero gets a new look and a new draw call
[engine/main 9f16ce2cda63] hero gets a new look and a new draw call
atomic transaction 8c078fff1864 covering 2 zones
$ vcs log --zone engine --oneline
9f16ce2cda63 hero gets a new look and a new draw call
1efc9815c984 first playable
$ vcs log --zone assets --oneline
ef1103e58356 hero gets a new look and a new draw call
84825b00926a first playable
$ vcs zone branches engine
* engine/main                     9f16ce2cda63
```

Each zone has its own log. `vcs log` without `--zone` shows the zone that contains your current directory
(here `root`). A command run inside `engine/` works on `engine`.

## Branches per zone

Branches belong to a zone. The artists make theirs without touching anyone else.

```text
$ vcs zone branch assets art-wip
created assets/art-wip at ef1103e58356
$ vcs zone checkout assets art-wip
zone assets: switched to art-wip
$ echo "PNGDATA-v3-wip" > assets/textures/hero.png && vcs add assets && vcs commit -m "wip hero"
staged 1 file(s), 0 removal(s)
[assets/art-wip 609d11e98098] wip hero
$ vcs status
zone assets (assets/): branch art-wip @ 609d11e98098
zone engine (engine/): branch main @ 9f16ce2cda63
zone root: branch main @ 44e32cf0e1c5
zone tools (tools/): branch main @ a34df4eb1326
clean
$ vcs zone checkout assets main
zone assets: switched to main
$ cat assets/textures/hero.png
PNGDATA-v2
```

## Snapshots

A snapshot pins one commit per zone under a name. Use it for milestones and builds.
`vcs snapshot checkout` puts every subscribed zone back to the pinned commit.
It does not move branches backwards. It makes a branch `snapshot/<name>` and checks that out,
so your own branch keeps its commits. It refuses if a zone has uncommitted work.
`vcs op undo` returns you to where you were.

```text
$ vcs snapshot create milestone-1 -m "alpha build"
snapshot milestone-1 = 5182afa52316
$ vcs snapshot list
milestone-1          5182afa52316
$ vcs snapshot show milestone-1
snapshot milestone-1 (5182afa52316ba08ce8cf07094cce448cf0ac3e4042f84dc2f045b355621afcd)
  assets         ef1103e583561b81123096ddc3dd8f549d0c704fff71c8f26923b43480dc3c86
  engine         9f16ce2cda634696ba81cd41438296960717935122a2cd2dc4967d72899e2817
  root           44e32cf0e1c5f77d0775e7373925b01b45630a31f525d3b1a9c8790837f16597
  tools          a34df4eb1326a22f9868760132950b3f9fc4015b7943ef4f147748f954454154
$ echo "fn render() { draw(); post(); }" > engine/render/mod.rs && vcs add . && vcs commit -m "post-processing"
staged 4 file(s), 0 removal(s)
[engine/main a9eb9bb9ae3a] post-processing
$ vcs snapshot checkout milestone-1
zone engine: now on snapshot/milestone-1 at 9f16ce2cda63
zone assets: already at the snapshot
zone root: already at the snapshot
zone tools: already at the snapshot
$ cat engine/render/mod.rs
fn render() { draw(); }
$ vcs branch --zone engine
  engine/main                     a9eb9bb9ae3a
* engine/snapshot/milestone-1     9f16ce2cda63
$ vcs op undo
undone; recorded operation 401ed0a81001
$ cat engine/render/mod.rs
fn render() { draw(); post(); }
```

## Subscribe and unsubscribe

`vcs zone remove` unsubscribes and takes the zone's files out of the working tree. Nothing is deleted from
the repository. `vcs zone add` subscribes again and writes the files back.

```text
$ vcs zone remove assets
unsubscribed from zone assets
$ ls
README.md
engine
tools
$ vcs status
zone engine (engine/): branch main @ a9eb9bb9ae3a
zone root: branch main @ 44e32cf0e1c5
zone tools (tools/): branch main @ a34df4eb1326
not subscribed (not downloaded/materialized, NOT deleted): assets
clean
$ vcs zone add assets
subscribed to zone assets
$ ls assets/textures
hero.png
```

When you clone from a remote you can choose zones up front. See [chapter 5](05-remotes.md#sparse-clones).

## Sparse checkout inside a zone

`vcs sparse set ZONE PATHS...` materializes only some paths of a zone. The other files stay tracked, not deleted.
`add`, `remove`, `list` and `disable ZONE` adjust it. This is different from unsubscribing: you keep the zone,
but only part of its files are on disk.

```text
$ vcs sparse set root src
0 file(s) written, 2 removed from the working tree
$ ls; vcs status
src
zone root: branch main @ 67faedb41176
sparse zone root (other paths not materialized, NOT deleted): src
clean
$ vcs sparse add root docs
1 file(s) written, 0 removed from the working tree
$ vcs sparse list
root: docs src
$ vcs sparse disable root && ls
1 file(s) written, 0 removed from the working tree
bug
docs
src
```

## Rename

`vcs zone rename` changes the display name. The old name stays the zone's permanent id in commits,
so no history is rewritten. `vcs zone delete NAME` removes a zone definition (add `--force` if it has branches).
Details: [../zones.md](../zones.md).

```text
$ vcs zone rename tools build-tools
renamed zone tools to build-tools
$ vcs zone list
assets         assets               subscribed, on main; 2 branch(es)
engine         engine               subscribed, on main; 1 branch(es)
root           (rest of repo)       subscribed, on main; 1 branch(es)
build-tools    tools                subscribed, on main; 1 branch(es)
```

Next: [chapter 5, remotes](05-remotes.md).
