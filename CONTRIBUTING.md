# Contributing to vcs

Thanks for looking. vcs is early research software; small, tested changes are the easiest to review.

## Build and test

```sh
cargo build --release                      # the vcs binary lands in target/release/vcs
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                     # includes the protocol model checker (about 20 s)
cargo test -p vcs-merge --features structural
```

Prefer a targeted `cargo test -p <crate> --test <file>` while iterating, and the full suite once before you push.
Tests that need an S3-compatible server are gated on `VCS_TEST_S3_ENDPOINT` (see [docs/s3.md](docs/s3.md)).

After changing any CLI command or flag, run `scripts/gen_cli_reference.sh` and commit the regenerated chapter 12 of the
guide, or `cargo test --workspace` fails.

## Pull requests

* One topic per PR, with a regression test for a bug fix.
* Commit messages are Conventional Commits with an emoji, for example `🐛 fix(gc): release the lock on a failed probe`.
* The PR description follows [.github/pull_request_template.md](.github/pull_request_template.md): what, why, how you
  checked it, and the caveats.
* CI runs fmt, clippy and the test suite on Linux and macOS. The MinIO job and the Windows compile check are advisory.
* Benchmark numbers in the docs come from the scripts under `scripts/` and the raw data under `docs/research/data/`;
  if you change a number, change the data it came from too.

## Reporting bugs

Open an issue with the command you ran, what you expected and what happened. For anything security related, see
[SECURITY.md](SECURITY.md) instead.

## License

By contributing you agree that your work is licensed under the repository's terms, `MIT OR Apache-2.0`
([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)).
