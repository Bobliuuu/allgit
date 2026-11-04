# Virtual and lazy filesystems for huge repositories

Status: research, issue #98. Date of writing and of every "accessed" date below: 2026-10-31.

**Verification legend.** *Verified* means the statement was read on the cited page during this research. *Unverified*
means it comes from memory, a secondary source, or a page that did not state it; do not rely on it without checking.
This document contains no performance numbers of our own. Where a source gave none, none are given.

## 1. Question

Zones already give sparse subscription: an unsubscribed zone is not downloaded and not materialized
([architecture.md](../architecture.md), "Sparse workspace"). The object store is still local-complete for subscribed
zones, and checkout writes every file of a subscribed zone to disk. Could a virtual or lazy filesystem materialize
files on demand, and what is the cheapest path from today?

## 2. Survey

### 2.1 VFS for Git (Microsoft)

- Virtualizes the working directory of a Git repo so tools see a normal tree while objects are downloaded only when
  needed; it also limits which files Git considers so `status` and `checkout` stay fast. Verified.
- Runs on Windows only (Windows 10 1607 or later) and needs Microsoft's Git fork plus a server that speaks the GVFS
  protocol. The README says "for new deployments, we strongly recommend you consider Scalar instead". The page did not
  say the project is archived. Verified.
- Kernel side is a filter driver (PrjFlt, formerly GvFlt); Microsoft's ProjFS page names VFS for Git as a provider.
  Verified (README and ProjFS page).
- Protocol: single-object GET, batched object POST (usually a packfile, optionally with trees and parents to a depth),
  a prefetch of non-blob packs resumable by timestamp, a sizes endpoint, and a config endpoint. Verified.
- Failure mode worth noting: a lazy client is unusable offline for any file not yet fetched. The sizes endpoint exists
  because file listings need sizes without content (design lesson; the reason is an inference, unverified).

Sources:
- https://github.com/microsoft/VFSForGit (accessed 2026-10-31)
- https://github.com/microsoft/VFSForGit/blob/master/Protocol.md (accessed 2026-10-31)

### 2.2 Scalar and Git's own scaling features

- Scalar began as a modification of VFS for Git and was reworked into a thin layer over Git features: partial clone,
  hourly background prefetch, sparse-checkout and a file system monitor. It now lives in `microsoft/git`. Verified.
- Git partial clone: the client passes a filter at clone/fetch, the server omits objects, filtered packs are marked
  promisor, and missing objects are fetched on demand. The docs state it "requires that the user be online", that
  dynamic fetching "tends to be slow as objects are fetched one at a time", and that `checkout` batches the blobs it
  needs. Verified.
- Git sparse-checkout: cone mode (directory names, hash matching) is recommended, non-cone is deprecated, the sparse
  index is experimental and "not completely understood by external tools", and the command itself is marked
  experimental. Other commands behave differently under sparsity (e.g. `commit -a` does not record out-of-cone paths as
  deleted). Verified.

Sources:
- https://github.com/microsoft/scalar (accessed 2026-10-31)
- https://git-scm.com/docs/partial-clone (accessed 2026-10-31)
- https://git-scm.com/docs/git-sparse-checkout (accessed 2026-10-31)

### 2.3 Sapling EdenFS (Meta)

- "A virtual filesystem designed for efficiently serving large source control repositories". Lazily fetches file data;
  speeds up status, checkout and change notification (Watchman); can return content hashes without reading file
  contents. Verified.
- Kernel interface differs per platform: FUSE on Linux, NFSv3 on macOS, Projected File System on Windows, with shared
  internal inode logic. Verified.
- Inodes are either non-materialized (backed by a 20-byte source control object id, content fetched through the object
  store) or materialized (content in the overlay). Modifying a file materializes it and all ancestors up the tree.
  A file restored to its original content may stay materialized until checkout or commit de-materializes it. No hard
  link support. Inode numbers come from a monotonic 64-bit counter. Verified.
- Caching: kernel cache, an in-memory blob LRU, a blob-metadata LRU (size and SHA-1), and no on-disk object store of
  its own; it relies on the Sapling backing store cache. The caching page is dated 2018 and may be stale. Offline and
  fetch-failure behavior is not described there. Verified (page), offline behavior unknown.
- Local modifications live only in the overlay (`clients/NAME/local/`); the docs call this data that cannot be
  recovered from the repository. Mount and unmount need root, handled by a privileged helper. Verified.
- macOS history: EdenFS moved from the macFUSE kernel extension to an NFSv3 server it implements itself; reasons given
  are kernel extension deprecation and that macFUSE "doesn't suit our needs anymore"; FileProvider was rejected
  because it relies on FSEvents; FSKit was being evaluated. Reported problems with NFSv3: no forget API so loaded file
  handles are kept, and under heavy load a "Server connections interrupted" dialog can unmount the mount. The
  macOS design text was read through a third-party mirror and the forum summary through a search snippet, so treat
  both as partly unverified.

Sources:
- https://github.com/facebook/sapling/blob/main/eden/fs/docs/Overview.md (accessed 2026-10-31)
- https://github.com/facebook/sapling/blob/main/eden/fs/docs/Inodes.md (accessed 2026-10-31)
- https://github.com/facebook/sapling/blob/main/eden/fs/docs/Caching.md (accessed 2026-10-31)
- https://github.com/facebook/sapling/blob/main/eden/fs/docs/Process_State.md (accessed 2026-10-31)
- https://github.com/facebook/sapling/blob/main/eden/fs/docs/macOS.md via mirror github.xiaohuzi123231346.workers.dev (accessed 2026-10-31, unverified mirror)
- https://developer.apple.com/forums/thread/766793 (surfaced by search, not opened; unverified)

### 2.4 Google Piper and CitC

- Piper is Google's centralized source system; the CACM article by Potvin and Levenberg (2016, vol. 59, pp. 78-87)
  describes it. Verified only that the article exists and its abstract.
- The following comes from a Wikipedia summary that cites the article, not from the article: CitC ("Clients in the
  Cloud") uses a cloud backend and a local FUSE filesystem to present changes overlaid on the full repository, stores
  only modified files in the workspace, and the average local copy is under ten files. Unverified (secondary).
  We could not read the paper, and Piper/CitC are internal, so their failure modes and platform support are unknown.
  The useful lesson (an overlay holding only modified files over a remote tree) is the same shape as EdenFS.

Sources:
- https://research.google/pubs/why-google-stores-billions-of-lines-of-code-in-a-single-repository/ (accessed 2026-10-31, abstract only)
- https://en.wikipedia.org/wiki/Piper_(source_control_system) (surfaced by search, accessed 2026-10-31, secondary)

### 2.5 Mercurial sparse and narrow

- The `sparse` extension limits the working directory to a subset of files via a config of includes, excludes and
  `%include`d profiles, and is marked experimental. Verified. It restricts the working copy only; history is still
  complete. (That last point is general knowledge, unverified.)
- Mercurial narrow clones, which also limit history/storage, were not researched. Unverified, open item.

Source: https://sources.debian.org/data/main/m/mercurial/7.1-2/doc/html/ext-sparse.html (accessed 2026-10-31)

### 2.6 Kernel interfaces and platform support

| Platform | Options | Notes |
|---|---|---|
| Linux | FUSE (Rust crate `fuser`, MIT, a rewrite of libfuse's low-level interface; docs page listed only Linux) | EdenFS uses FUSE here. Verified. |
| Windows | Projected File System (ProjFS); WinFsp (FUSE-compatible layer, GPLv3 with FLOSS exception, commercial license available) | ProjFS is built for fast backing stores and has no progress reporting or online/offline state; Microsoft points slow stores to the Cloud Files API. Verified. WinFsp licensing needs legal review before we depend on it. |
| macOS | macFUSE (kernel extension), NFSv3 loopback server (EdenFS), FSKit (user-space file system extension), FileProvider | Which macOS version FSKit needs and its limits could not be verified: the Apple page returned no body. macFUSE's current requirements were not verified. |

Sources:
- https://docs.rs/fuser/latest/fuser/ (accessed 2026-10-31)
- https://learn.microsoft.com/en-us/windows/win32/projfs/projected-file-system (accessed 2026-10-31)
- https://github.com/winfsp/winfsp (accessed 2026-10-31)
- https://github.com/macfuse/macfuse/wiki (accessed 2026-10-31)
- https://developer.apple.com/documentation/fskit (accessed 2026-10-31, no content retrieved)

## 3. Comparison

| | VFS for Git | Scalar / Git | EdenFS | CitC | Hg sparse |
|---|---|---|---|---|---|
| Mechanism | filter driver, lazy objects | partial clone + sparse + fsmonitor, no virtual FS | user-space FS daemon, inode overlay | FUSE + cloud overlay | working-copy include/exclude |
| Platforms | Windows | Git platforms (not verified per OS) | Linux, macOS, Windows | internal (unknown) | Hg platforms |
| Offline | only fetched files | only fetched objects | not documented | unknown | full history local |
| Main failure risk | server/protocol coupling, Windows only | slow one-by-one fetch | privileged daemon, per-OS kernel quirks, overlay holds unrecoverable edits | unknown | experimental, partial-state confusion |
| Needs a daemon | yes | no (background maintenance only) | yes | yes | no |

Common failure modes across the virtual designs (from the sources above): daemon crash or stale mount leaves the tree
unusable; local edits that exist only in an overlay are not recoverable from the repository; tools that scan the whole
tree trigger mass hydration; network stalls surface as filesystem hangs with no progress reporting (ProjFS states it
offers none); per-OS kernel interfaces diverge.

## 4. How our design interacts

Facts from this repo ([architecture.md](../architecture.md), [zones.md](../zones.md)):

- Fetch is already per zone and hash-verified; `fetch_heads(zones)` reads only objects reachable from requested zone
  heads. Unsubscribed zones are never read (tested).
- Trees are whole-tree per zone (decision D9), with "subtree-skipping + lazy loading" planned after measurement.
- Status trusts `(size, mtime, mode)` under the racy rule and has no watcher; a watcher must be only a hint with
  periodic full verification (architecture.md). Issue #102 covers this.
- Packs, range reads and chunking are not done (#16, #87).
- Ownership is unambiguous because overlapping zones are forbidden (D4). A zone is therefore a natural unit: one
  mount, one overlay, one cache, one fetch scope, one lock domain.

## 5. Recommendation

Do not build a virtual filesystem first. Get lazy behavior from the object layer, where it is portable, testable
without a kernel driver, and also useful to a later VFS. Treat a kernel-backed VFS as an optional late stage, justified
only by measurements that stages 1 and 2 are insufficient. This follows the same arc Microsoft reports for itself:
VFS for Git was succeeded by Scalar built on Git-native features (sources 2.1, 2.2).

### Stage 0: baseline (today)
Zone-granular sparse clone and checkout. Add a measurement harness (see follow-ups) so later stages are judged by
measured request counts, bytes and wall time, not assumptions.

### Stage 1: lazy objects inside a subscribed zone (no VFS)
- Fetch a zone's trees eagerly and blobs lazily; materialize with batched blob prefetch (Git's `checkout` batching is
  the precedent) rather than one-by-one fetches, which Git documents as slow.
- Content-addressed blobs verified on arrival, as today. Requires range/batch reads (#16).
- Define offline behavior explicitly: a command needing a missing blob fails with a named error and a `vcs prefetch`
  hint; it never reports a missing blob as a deletion or as corruption.

### Stage 2: path-level sparsity inside a zone
- A per-zone path set (directories, cone-style, in the spirit of Git's recommended cone mode) limiting what is
  materialized and fetched. Out-of-set paths are reported like unsubscribed zones: "not materialized (NOT deleted)".
- Directory hash cache and optional watcher (#102) so status cost follows the materialized set.
- Hydration commands (`vcs hydrate <path>`) and, optionally, tool-triggered hydration via a hook.

### Stage 3 (optional, only if measurements justify): zone-aware VFS daemon
- One mount per subscribed zone; overlay stored per zone and documented as the only copy of uncommitted edits, so it
  must be covered by `fsck`/backup guidance and by the operation log.
- Non-materialized entries backed by the zone tree's object ids (as EdenFS does); modification materializes the entry
  and its ancestors.
- Platform order: Linux first (FUSE, `fuser`); Windows via ProjFS (no WinFsp unless licensing is cleared); macOS last
  and re-researched, since macFUSE, loopback NFS and FSKit all have documented or unverified drawbacks.
- Keep the real-directory mode as the supported default; the VFS must be removable without data loss (`vcs vfs
  detach` materializes everything the user has touched).

## 6. Risks

| Risk | Mitigation |
|---|---|
| Whole-tree scans (IDE, indexer, antivirus) hydrate everything | stage 2 path sets first; measure; keep ignore rules and a hydration budget |
| Fetch failure mid-operation leaves a half-materialized tree | preflight lists missing blobs before the first write (matches existing checkout safety rules) |
| Overlay is the only copy of edits | stage 3 only; documented, fsck-covered, detach command |
| Watcher misses events | hint only, periodic full verification (already a rule in architecture.md) |
| One-by-one fetch latency | batch requests, prefetch, pack/range reads (#16) |
| macOS kernel interface churn | defer macOS; do not depend on macFUSE or unverified FSKit details |
| Licensing (WinFsp GPLv3 exception) | prefer ProjFS; legal check before any dependency |
| Case-insensitive filesystems and symlinks in a virtual layer | tie to #89; reject at preflight as today |

## 7. Open questions (not answered by this research)

- Real numbers: how many files a typical zone checkout touches versus a developer actually opens. Needs our own
  measurement (stage 0 harness); none are claimed here.
- Mercurial narrow clones, and the actual CitC design from the primary paper.
- FSKit availability and limits; macFUSE current requirements; EdenFS offline behavior.
- Whether tests can exercise a FUSE mount in CI (needs privileges); likely needs a trait boundary with an in-memory
  fake so the logic is tested without a kernel.

## 8. Follow-up issues

- #116 measure remote requests and bytes for sparse clone and checkout (stage 0).
- #117 lazy blob fetch with batched prefetch (stage 1).
- #118 path-level sparse sets inside a zone (stage 2).
- #119 virtual filesystem trait boundary with an in-memory fake (stage 3 preparation, optional).

A kernel-backed mount itself is intentionally not filed until the stage 1 and 2 measurements exist.
