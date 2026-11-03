# Sparse-clone object cache

Status: **policy + lazy fetch + fsck classification.** Eviction itself and the size cap are specified here but not
implemented: no command deletes cached objects yet (see docs/gc.md for why deletion needs care). Tracks issue #34.

## Policy

| Class | Definition | May be evicted? |
|---|---|---|
| Pinned | Reachable from a local root: local branches, snapshots, the index, merge state, the operation chain (the same roots as docs/gc.md) | No. Local-only objects exist nowhere else. |
| Cache | Not pinned, and reachable from a remote-tracking head (`remote-refs`), whose whole closure was verified when fetched | Yes. The remote has it. |
| Unknown | Neither | No. |

An object that is both pinned and published is still safe to refetch, but is not chosen for eviction first.
Size cap (not implemented): when the store exceeds the cap, evict Cache objects oldest first, never Pinned ones. If
the pinned set alone is over the cap, the cap is exceeded rather than losing data.

## Lazy fetch

A command that finds an object absent (`StoreError::NotFound`) fetches exactly that object from the remote,
verifies `hash(bytes) == id` and canonical decode **before** anything is written, stores it, syncs, and reruns.

- CLI: `run_lazy` in `crates/vcs-cli/src/main.rs`. Library: `Repo::with_lazy_fetch` / `Repo::fetch_missing`.
- A corrupt remote object is rejected and never stored (`vcs_remote::fetch_object` returns an error first).
- A corrupt **local** object (`StoreError::Corrupt`) is never silently replaced; `vcs fsck` reports it.
- An id that is still missing after being fetched fails the command, so the retry loop terminates.
- Fetching is one object per round trip; an evicted subtree costs one fetch per missing node. Batching belongs with
  the packfiles work.
- The command is rerun from the start, so output printed before the missing object was hit may appear twice.

## fsck

`vcs fsck` separates:

- **Evictable cache miss**: an object referenced but absent, and reachable from a remote-tracking head. Reported as a
  count, exit 0.
- **Problem**: absent and not known to be on the remote, or present but failing hash/decode. Exit non-zero.

fsck is offline, so "known to be on the remote" means "reachable from a remote-tracking head through objects we hold".
A missing object below another missing object is not visible until the parent is refetched.
