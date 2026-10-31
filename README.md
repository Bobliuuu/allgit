# all-git

A native, zone-aware version control system in Rust. **Not** a Git wrapper: its own object model, commit graph, refs,
workspace and transactions. See `docs/` for the design and honest status.

```bash
cargo build --release          # binary: target/release/vcs
cargo test --workspace         # 55 tests (unit, property, integration, CLI end-to-end)

vcs init demo && cd demo
echo hello > hello.txt && vcs add hello.txt && vcs commit -m "Initial commit"
echo world >> hello.txt && vcs status && vcs diff
vcs add hello.txt && vcs commit -m "Update greeting" && vcs log

mkdir engine assets
vcs zone create engine --path engine && vcs zone create assets --path assets
vcs zone branch engine feature && vcs zone checkout engine feature
vcs status && vcs zone list
```

<!-- remotes:begin -->
### Remotes

```bash
vcs remote set /srv/shared/demo.vcs      # or file:///srv/shared/demo.vcs
vcs publish                              # atomic multi-zone publish; prints zone/branch old -> new
vcs clone /srv/shared/demo.vcs work --zone engine   # sparse: other zones listed as skipped
vcs fetch [--zone A]                     # update remote-tracking refs, local branches untouched
vcs sync                                 # fast-forward where safe (up to date / ahead / diverged / dirty)
vcs remote show
```

Only local directories and `file://` are supported in this build; other schemes (e.g. `s3://`) give a clear error.
<!-- remotes:end -->

Set `VCS_AUTHOR_NAME` / `VCS_AUTHOR_EMAIL` to control commit authorship.

Implemented: local repository, zones (independent branch heads, sparse subscription, atomic multi-zone commit),
snapshots, fsck. Not implemented yet: remote/S3, merge/rebase, operation log, packs, interop — see `docs/roadmap.md`.
