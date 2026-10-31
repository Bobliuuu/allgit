# Installing vcs

## From a release

Tagged versions (`vX.Y.Z`) publish tarballs for Linux and macOS (x86_64 and aarch64) plus a `SHA256SUMS` file on the
[releases page](https://github.com/Bobliuuu/allgit/releases).

```sh
tag=v0.1.0
target=aarch64-apple-darwin   # or x86_64-apple-darwin, x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu
base=https://github.com/Bobliuuu/allgit/releases/download/$tag
curl -LO "$base/vcs-$tag-$target.tar.gz"
curl -LO "$base/SHA256SUMS"
grep "vcs-$tag-$target.tar.gz" SHA256SUMS | shasum -a 256 -c -   # use sha256sum -c on Linux
tar -xzf "vcs-$tag-$target.tar.gz"
install "vcs-$tag-$target/vcs" /usr/local/bin/vcs
```

## From source

Requires Rust 1.80 or newer.

```sh
git clone https://github.com/Bobliuuu/allgit && cd allgit
cargo install --path crates/vcs-cli --locked
vcs --help
```

## Windows (experimental, compile-checked only)

**Nothing in this project has been executed on Windows.** The only verification is that
`cargo check --workspace --all-targets` passes for `x86_64-pc-windows-gnu` (cross-compiled on Linux with
mingw-w64). No tests, no binary, and no `msvc` build were run; the advisory `windows-check` CI job runs
`cargo check --target x86_64-pc-windows-msvc` on `windows-latest` and does not run tests either.

What the code intends on Windows (unverified):

- No executable bit: files are always committed and restored as regular files, never `Executable`.
- Symlink entries cannot be checked out (creating one returns an error); symlinks in history are not materialized.
  Checkout refuses to write through symlinks and also through junctions/reparse points.
- Directory fsync after writing a pack is skipped; file data is still `sync_all`ed.
- `FsBackend` create-only uses `hard_link` (NTFS only; fails on FAT/exFAT or some network shares) and CAS uses
  `fs::rename`, which replaces the target via `MoveFileExW`. Replacing a file another process holds open without
  share-delete fails with an error rather than being atomic.
- Unix-only tests (symlinks, shell-based CLI assist tests) are `cfg(unix)` and are not built there.
- `vcs-bench` `packbench` reports apparent size as allocated size.

Not supported: any claim of correctness, performance, or durability on Windows; long paths (>260 chars) and
case-only renames have not been considered.

## Cutting a release (maintainers)

Push a tag such as `v0.1.0`. The `Release` workflow builds the four targets and creates the GitHub release with checksums.
Tags containing `-` (for example `v0.2.0-rc1`) are marked as pre-releases. Try the workflow on a fork with a throwaway tag first.

## CI and audit

CI and the scheduled `Audit` workflow (cargo-deny, config in `deny.toml`) are advisory: they are not required status checks.
