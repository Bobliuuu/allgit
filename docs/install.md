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

## Cutting a release (maintainers)

Push a tag such as `v0.1.0`. The `Release` workflow builds the four targets and creates the GitHub release with checksums.
Tags containing `-` (for example `v0.2.0-rc1`) are marked as pre-releases. Try the workflow on a fork with a throwaway tag first.

## CI and audit

CI and the scheduled `Audit` workflow (cargo-deny, config in `deny.toml`) are advisory: they are not required status checks.
