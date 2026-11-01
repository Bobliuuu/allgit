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
[assets/main a2cba3a7dff5] first playable
[engine/main 4266ff8fbdbb] first playable
[root/main 38f9381dc661] first playable
[tools/main 2cea13ae2d54] first playable
atomic transaction f445c832840d covering 4 zones
$ vcs log --zone engine --oneline
4266ff8fbdbb first playable
$ vcs log --zone assets --oneline
a2cba3a7dff5 first playable
$ vcs log --oneline
38f9381dc661 first playable
```

The same files, a second time. This change touches `engine` and `assets` only, so only two commits are made
and the transaction covers two zones.

```text
$ echo "fn render() { draw(); }" > engine/render/mod.rs && echo "PNGDATA-v2" > assets/textures/hero.png
$ vcs add .
staged 4 file(s), 0 removal(s)
$ vcs commit -m "hero gets a new look and a new draw call"
[assets/main e787b60393f7] hero gets a new look and a new draw call
[engine/main 8455b6481cb5] hero gets a new look and a new draw call
atomic transaction 465078b9066e covering 2 zones
$ vcs log --zone engine --oneline
8455b6481cb5 hero gets a new look and a new draw call
4266ff8fbdbb first playable
$ vcs log --zone assets --oneline
e787b60393f7 hero gets a new look and a new draw call
a2cba3a7dff5 first playable
$ vcs zone branches engine
* engine/main                     8455b6481cb5
```

Each zone has its own log. `vcs log` without `--zone` shows the zone that contains your current directory
(here `root`). A command run inside `engine/` works on `engine`.

## Branches per zone

Branches belong to a zone. The artists make theirs without touching anyone else.

```text
$ vcs zone branch assets art-wip
created assets/art-wip at e787b60393f7
$ vcs zone checkout assets art-wip
zone assets: switched to art-wip
$ echo "PNGDATA-v3-wip" > assets/textures/hero.png && vcs add assets && vcs commit -m "wip hero"
staged 1 file(s), 0 removal(s)
[assets/art-wip 0aad00eec57c] wip hero
$ vcs status
zone assets (assets/): branch art-wip @ 0aad00eec57c
zone engine (engine/): branch main @ 8455b6481cb5
zone root: branch main @ 38f9381dc661
zone tools (tools/): branch main @ 2cea13ae2d54
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
snapshot milestone-1 = e987d2997cfb
$ vcs snapshot list
milestone-1          e987d2997cfb
$ vcs snapshot show milestone-1
snapshot milestone-1 (e987d2997cfb1c55894ca7f02c775e8f299d38a6f82145e421da5bbd9b7112c2)
  assets         e787b60393f739a0e439c16807f349ed150e3cb75a5805b65cc9bc3061794f51
  engine         8455b6481cb5b645968cc383db44d3905b1c2ad0d0836cde3031316a35d36149
  root           38f9381dc661252168afebdb38030e1c733a9e2c066aa481745d8131e6177ead
  tools          2cea13ae2d542b7445861cbfaf82580a124ed1a7887cd601db739a628383d61b
$ echo "fn render() { draw(); post(); }" > engine/render/mod.rs && vcs add . && vcs commit -m "post-processing"
staged 4 file(s), 0 removal(s)
[engine/main 5f5466f5c9d5] post-processing
$ vcs snapshot checkout milestone-1
zone engine: now on snapshot/milestone-1 at 8455b6481cb5
zone assets: already at the snapshot
zone root: already at the snapshot
zone tools: already at the snapshot
$ cat engine/render/mod.rs
fn render() { draw(); }
$ vcs branch --zone engine
  engine/main                     5f5466f5c9d5
* engine/snapshot/milestone-1     8455b6481cb5
$ vcs op undo
undone; recorded operation b57269e3c9f2
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
zone engine (engine/): branch main @ 5f5466f5c9d5
zone root: branch main @ 38f9381dc661
zone tools (tools/): branch main @ 2cea13ae2d54
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
