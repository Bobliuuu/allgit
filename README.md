# all-git

A native, area-aware version control system in Rust. **Not** a Git wrapper: its own object model, commit graph, refs,
workspace and transactions. See `docs/` for the design and honest status.

```bash
cargo build --release          # binary: target/release/vcs
cargo test --workspace         # 55 tests (unit, property, integration, CLI end-to-end)

vcs init demo && cd demo
echo hello > hello.txt && vcs add hello.txt && vcs commit -m "Initial commit"
echo world >> hello.txt && vcs status && vcs diff
vcs add hello.txt && vcs commit -m "Update greeting" && vcs log

mkdir engine assets
vcs area create engine --path engine && vcs area create assets --path assets
vcs area branch engine feature && vcs area checkout engine feature
vcs status && vcs area list
```

<!-- remotes:begin -->
### Remotes

```bash
vcs remote set /srv/shared/demo.vcs      # or file:///srv/shared/demo.vcs
vcs publish                              # atomic multi-area publish; prints area/branch old -> new
vcs clone /srv/shared/demo.vcs work --area engine   # sparse: other areas listed as skipped
vcs fetch [--area A]                     # update remote-tracking refs, local branches untouched
vcs sync                                 # fast-forward where safe (up to date / ahead / diverged / dirty)
vcs remote show
```

Only local directories and `file://` are supported in this build; other schemes (e.g. `s3://`) give a clear error.
<!-- remotes:end -->

Set `VCS_AUTHOR_NAME` / `VCS_AUTHOR_EMAIL` to control commit authorship.

Implemented: local repository, areas (independent branch heads, sparse subscription, atomic multi-area commit),
snapshots, fsck. Not implemented yet: remote/S3, merge/rebase, operation log, packs, interop — see `docs/roadmap.md`.
