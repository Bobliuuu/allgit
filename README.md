<div align="center">

# 🌳 allgit

**Version control for big repos where code, art, and docs shouldn't share one history.**

[![CI](https://github.com/Bobliuuu/allgit/actions/workflows/ci.yml/badge.svg)](https://github.com/Bobliuuu/allgit/actions/workflows/ci.yml)
![Rust](https://img.shields.io/badge/rust-1.80%2B-orange)
![Status](https://img.shields.io/badge/status-early%20research-yellow)
![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)

</div>

allgit is a version control tool (the command is `vcs`) written in Rust, for teams whose repository mixes very different things (say, game code, shaders, and gigabytes of art) and who want to download and work on only the parts they need. It is **not** a Git wrapper: it has its own storage and never calls Git.

> ⚠️ Early research project. It works and is tested, but expect rough edges and no server yet. See [what works today](#-what-works-today).

## 💡 The big idea

Imagine a game studio. Programmers touch `engine/`, artists touch `assets/`. With plain Git, everyone shares one history and downloads everything.

With allgit, each part of the repo is a **zone**:

- 🧩 Each zone has its **own branches and history**, so an artist's busy week doesn't clutter the programmers' log.
- 📦 You **subscribe** only to the zones you need. Skipped zones are never downloaded (and never "deleted", just not there).
- ⚛️ One commit can still change several zones **atomically**: either all of them move, or none do.

## 🚀 Try it in 60 seconds

You need [Rust](https://rustup.rs) 1.80+. Every command and output below was run for real.

**1. Build and make a repo**

```bash
cargo build --release
export PATH="$PWD/target/release:$PATH"
export VCS_AUTHOR_NAME=Ada VCS_AUTHOR_EMAIL=ada@example.com   # who you are

vcs init demo && cd demo
```
```text
Initialized empty vcs repository in /path/to/demo/.vcs
```

**2. Commit, change, and look at the history**

```bash
echo hello > hello.txt
vcs add hello.txt && vcs commit -m "Initial commit"
echo world >> hello.txt
vcs status && vcs diff
vcs add hello.txt && vcs commit -m "Update greeting"
vcs log
```
```text
staged 1 file(s), 0 removal(s)
[root/main 96d49f6fc5b5] Initial commit
zone root: branch main @ 96d49f6fc5b5
  not staged:
    modified:  hello.txt
diff --vcs a/hello.txt b/hello.txt
--- a/hello.txt
+++ b/hello.txt
@@ -1,1 +1,2 @@
 hello
+world
staged 1 file(s), 0 removal(s)
[root/main e670fef25d9b] Update greeting
commit e670fef25d9b260392538cebb7f2eb9ac9170237e51c90ab1019bc7a5eb23927
change e53b19aa3c7a90559906ac5a611c9321   zone root
Author: Ada <ada@example.com>
Date:   1793477306
    Update greeting

commit 96d49f6fc5b58621a3005c7c8508e00f5f64504fb533f219630c7954cd1e8850
change 58ff190e27c4bafe507dbedca2a01929   zone root
Author: Ada <ada@example.com>
Date:   1793477306
    Initial commit
```

**3. Split the repo into zones** (a commit touching both is one atomic transaction)

```bash
mkdir engine assets
echo 'fn main(){}' > engine/main.rs && echo bin > assets/logo.bin
vcs zone create engine --path engine
vcs zone create assets --path assets
vcs add engine assets && vcs commit -m "Add engine and assets"
vcs zone branch engine feature       # branch just the engine
vcs zone checkout engine feature     # assets stays on main
vcs zone list
vcs snapshot create v1 && vcs snapshot checkout v1   # every subscribed zone back to its pinned commit
vcs zone rename old new                 # zones without history only; vcs zone delete <name> [--force]
vcs branch -d feature                   # refuses the checked-out branch and unmerged work (--force to override)
```
```text
created zone engine owning engine/
created zone assets owning assets/
staged 2 file(s), 0 removal(s)
[assets/main 764099d40c74] Add engine and assets
[engine/main d4fbb8635c1d] Add engine and assets
atomic transaction 218e46ade70a covering 2 zones
created engine/feature at d4fbb8635c1d
zone engine: switched to feature
assets         assets               subscribed, on main; 1 branch(es)
engine         engine               subscribed, on feature; 2 branch(es)
root           (rest of repo)       subscribed, on main; 1 branch(es)
```

**4. Share it, then clone only the engine**

```bash
vcs zone checkout engine main        # back to main; publish sends every branch
vcs remote set ~/shared.vcs          # any directory works as a remote
vcs publish
cd .. && vcs clone ~/shared.vcs work --zone engine
cd work && vcs status
```
```text
zone engine: switched to main
remote set to /Users/you/shared.vcs
assets/main (none) -> dfd635917b38
engine/main (none) -> fd4d2ccc4144
published atomically (remote generation 1)
Cloned /Users/you/shared.vcs into work
downloaded zones: engine
skipped zones (not subscribed): assets, root
  add one later with `vcs zone add <name>` then `vcs fetch --zone <name>`
zone engine (engine/): branch main @ fd4d2ccc4144
not subscribed (not downloaded/materialized, NOT deleted): assets, root
clean
```

That's it: `work/` contains only `engine/`. Later, `vcs fetch` downloads updates and `vcs sync` fast-forwards when safe (it reports dirty or diverged zones instead of touching them). A publish that would overwrite someone else's work is refused with guidance.

**5. Everyday commands** (run inside `work/engine`; all verified)

```bash
vcs tag v1.0 && vcs tag               # lightweight tag, then list tags
vcs show                              # HEAD commit with its diff
vcs blame main.rs                     # per-line commit attribution
echo x >> main.rs
vcs stash push && vcs stash pop       # set changes aside, bring them back
vcs restore main.rs                   # discard working-tree changes (--staged to unstage)
vcs mv main.rs lib.rs && vcs commit -m mv
# also available: rm, revert, cherry-pick, rebase, resolve (see `vcs --help`)
vcs log --json                        # --json works on status, log, diff, branch, zone list, op log, conflict list
vcs gc --dry-run                      # report what GC would delete; deletes nothing
vcs repack && vcs fsck                # loose objects into one pack, then verify
vcs op log                            # operation log; `vcs op undo` reverts the last local operation
vcs lock --lease-secs 60 main.rs && vcs locks   # advisory lock recorded on the remote
```

> Hashes and timestamps differ on every run, and the outputs above are trimmed to the relevant lines. Step 4's output came from a separate fresh run of steps 1 to 3 (the hashes differ from step 3's).

## ✅ What works today

✅ done and tested · 🟡 partial or limited · ⬜ not built

| Area | Status | Notes |
|---|---|---|
| Content-addressed objects | ✅ | BLAKE3, domain-separated, canonical codec, strict decode, proptests |
| Local VCS | ✅ | init, add, commit, status, diff, log, branch, switch, fsck, tag (lightweight/annotated), show, blame, restore, rm, mv |
| Zones and snapshots | ✅ | non-overlapping, independent heads, sparse subscribe/unsubscribe, atomic multi-zone commit, rename/delete |
| Remote protocol | ✅ | single CAS manifest, idempotent publish, **model-checked** (~1.7M crash/interleaving schedules) |
| Backends | 🟡 | in-memory and filesystem (the CLI uses a directory); S3-compatible backend exists in `vcs-store-s3` (MinIO only, HTTP, TLS behind a feature) but is **not wired into the CLI** |
| Sparse fetch / clone | ✅ | tests assert an unsubscribed zone's bytes are never read |
| Merge | 🟡 | `merge` (fast-forward, three-way, conflict state, `--abort`), `conflict`, `resolve`; no rename detection; criss-cross and file/directory conflicts are errors ([merge.md](docs/merge.md)) |
| History rewriting | 🟡 | `cherry-pick`, `revert`, `rebase` (keeps change ids, linear ranges only), `stash` |
| Operation log / undo | ✅ | `op log/show/undo/restore`, local branches and workspace only ([operations.md](docs/operations.md)) |
| Packfiles | 🟡 | `repack` writes one pack plus index; no delta compression or chunking |
| Garbage collection | 🟡 | `gc --dry-run` reports only. **Nothing is ever deleted**; destructive GC is design only ([gc.md](docs/gc.md)) |
| Locks | 🟡 | `lock`, `unlock`, `locks` work and are model-checked, but the **client** validates them. **Not server-enforced**: anyone with manifest write access bypasses them ([locks.md](docs/locks.md)) |
| Git interop | 🟡 | `import-git` (fast-export stream) and `export-git` (prototype, one zone branch) ([import-git.md](docs/import-git.md), [interop.md](docs/interop.md)) |
| Perforce interop | ⬜ | design only |
| Semantic / AI-assisted merge | ⬜ | research notes only ([structural-merge](docs/research/structural-merge.md)) |
| Auth, validating server | ⬜ | none; see [security.md](docs/security.md) for what is hardened |
| Benchmarks | 🟡 | first baseline in [benchmarks.md](docs/benchmarks.md); `add` was measured **much slower than Git** (#21) |

Nothing here is claimed to work without an executable test. Known gaps are listed in [docs/architecture.md](docs/architecture.md#known-limitations-honest-list).

## 🛡️ Why you can trust it

- **Integrity:** every read re-hashes the data; each object has exactly one valid encoding.
- **Atomic publishing:** a publish becomes visible at one instant. An executable model tries every interleaving of two writers with crashes and lost messages injected, and checks nothing is lost or applied twice.
- **Untrusted input:** paths like `..` or `.vcs` are rejected, and checkout won't write through symlinks or overwrite untracked files.

Details: [architecture](docs/architecture.md).

## 🗺️ How the pieces fit

```mermaid
flowchart LR
  W["Your sparse workspace<br/>engine only"] -->|publish: one atomic step| R[("Remote<br/>object store + manifest")]
  R -->|fetch only subscribed zones| W
```

## 🧱 Code layout

| Crate | Role |
|---|---|
| `vcs-objects` | ids, canonical codec, object types, name/path validation (no I/O) |
| `vcs-store` | `ObjectStore` trait; memory + loose-object filesystem stores |
| `vcs-repo` | zones, refs, index, workspace, commit/log/diff/status, snapshots, operation log, packs, locks, git import/export, fsck, publish/fetch/sync/clone |
| `vcs-remote` | `Backend` trait + conformance suite, manifest, publish step machine, sparse fetch, model checker |
| `vcs-store-s3` | S3-compatible backend (hand-rolled SigV4, blocking, no async runtime) |
| `vcs-merge` | merge-base, diff3 text merge, three-way tree merge with structured conflicts |
| `vcs-bench` | seeded dataset generator and criterion benchmarks |
| `vcs-cli` | the `vcs` binary |

## 📚 Learn more

- Start here: [CLI reference](docs/cli.md) · [Architecture](docs/architecture.md) · [Object model](docs/object-model.md) · [Roadmap](docs/roadmap.md) · [Install](docs/install.md)
- Features: [Zones](docs/zones.md) · [Merge](docs/merge.md) · [Rebase](docs/rebase.md) · [Operations](docs/operations.md) · [Locks](docs/locks.md) · [Git import](docs/import-git.md) · [Interop](docs/interop.md) · [GC](docs/gc.md) · [Cache](docs/cache.md) · [S3 backend](docs/s3.md)
- Quality: [Security](docs/security.md) · [Benchmarks](docs/benchmarks.md)
- Research (written from memory, unverified): [overview](docs/research.md) · [commit graph](docs/research/commit-graph.md) · [performance](docs/research/performance.md) · [server](docs/research/server.md) · [structural merge](docs/research/structural-merge.md) · [virtual fs](docs/research/virtual-fs.md)

## 🛠️ Contributing

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                 # includes the ~20 s protocol model checker
VCS_TEST_S3_ENDPOINT=http://127.0.0.1:9000 cargo test -p vcs-store-s3   # MinIO, see docs/s3.md
```

Work is tracked in [issues](https://github.com/Bobliuuu/allgit/issues); PRs use Conventional Commits and the template in `.github/`.

## 📄 License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option. Copyright (c) 2026 Jerry Zhu.
