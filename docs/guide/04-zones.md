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
[assets/main 61615c4a9a0a] first playable
[engine/main ea9707b97dd9] first playable
[root/main 31b78928dec3] first playable
[tools/main 7a77cf067fbc] first playable
atomic transaction 6d0bed869d62 covering 4 zones
$ vcs log --zone engine --oneline
ea9707b97dd9 first playable
$ vcs log --zone assets --oneline
61615c4a9a0a first playable
$ vcs log --oneline
31b78928dec3 first playable
```

The same files, a second time. This change touches `engine` and `assets` only, so only two commits are made
and the transaction covers two zones.

```text
$ echo "fn render() { draw(); }" > engine/render/mod.rs && echo "PNGDATA-v2" > assets/textures/hero.png
$ vcs add .
staged 4 file(s), 0 removal(s)
$ vcs commit -m "hero gets a new look and a new draw call"
[assets/main 165508803861] hero gets a new look and a new draw call
[engine/main a45ec54c3a41] hero gets a new look and a new draw call
atomic transaction 42054034c758 covering 2 zones
$ vcs log --zone engine --oneline
a45ec54c3a41 hero gets a new look and a new draw call
ea9707b97dd9 first playable
$ vcs log --zone assets --oneline
165508803861 hero gets a new look and a new draw call
61615c4a9a0a first playable
$ vcs zone branches engine
* engine/main                     a45ec54c3a41
```

Each zone has its own log. `vcs log` without `--zone` shows the zone that contains your current directory
(here `root`). A command run inside `engine/` works on `engine`.

## Branches per zone

Branches belong to a zone. The artists make theirs without touching anyone else.

```text
$ vcs zone branch assets art-wip
created assets/art-wip at 165508803861
$ vcs zone checkout assets art-wip
zone assets: switched to art-wip
$ echo "PNGDATA-v3-wip" > assets/textures/hero.png && vcs add assets && vcs commit -m "wip hero"
staged 1 file(s), 0 removal(s)
[assets/art-wip b31251219674] wip hero
$ vcs status
zone assets (assets/): branch art-wip @ b31251219674
zone engine (engine/): branch main @ a45ec54c3a41
zone root: branch main @ 31b78928dec3
zone tools (tools/): branch main @ 7a77cf067fbc
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
snapshot milestone-1 = c6d803617e04
$ vcs snapshot list
milestone-1          c6d803617e04
$ vcs snapshot show milestone-1
snapshot milestone-1 (c6d803617e0408af1c90721b5155f74bbe5b21b1a152ea973db954d55e72c32f)
  assets         16550880386173ba6a00ae77ba5f8a689e5e9aaeaf23174bc0c1075d5c0d49ac
  engine         a45ec54c3a41e3995d14a29badd18ad5d937dfd41176121d6c64aad28d9a9819
  root           31b78928dec3cc2725f0577210c462566116e1f0cdc3a17eb0e5fb591348d7a4
  tools          7a77cf067fbcbf56ea2a7a02d07000032738516dd1b953cba61626dd4bc33d78
$ echo "fn render() { draw(); post(); }" > engine/render/mod.rs && vcs add . && vcs commit -m "post-processing"
staged 4 file(s), 0 removal(s)
[engine/main 7f2dc6c0097c] post-processing
$ vcs snapshot checkout milestone-1
zone engine: now on snapshot/milestone-1 at a45ec54c3a41
zone assets: already at the snapshot
zone root: already at the snapshot
zone tools: already at the snapshot
$ cat engine/render/mod.rs
fn render() { draw(); }
$ vcs branch --zone engine
  engine/main                     7f2dc6c0097c
* engine/snapshot/milestone-1     a45ec54c3a41
$ vcs op undo
undone; recorded operation 39afc2cc199d
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
zone engine (engine/): branch main @ 7f2dc6c0097c
zone root: branch main @ 31b78928dec3
zone tools (tools/): branch main @ 7a77cf067fbc
not subscribed (not downloaded/materialized, NOT deleted): assets
clean
$ vcs zone add assets
subscribed to zone assets
$ ls assets/textures
hero.png
```

When you clone from a remote you can choose zones up front. See [chapter 5](05-remotes.md#sparse-clones).

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
