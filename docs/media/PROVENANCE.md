# Media provenance

All terminal content in `demo.gif` and `explainer.mp4` is rendered from `transcript.txt`, which is the captured output
of a real run, not hand-written.

- Binary: `target/release/vcs`, built with `cargo build --release -p vcs-cli` from commit `b34ac76f723fd51b93ee6f758336b64fa4bd307b` (macOS, arm64).
- Run date: 2026-11-08
- Identity: `VCS_AUTHOR_NAME=Ada VCS_AUTHOR_EMAIL=ada@example.com`
- Run in a fresh temporary directory (path shown as `~/demo-run` in the transcript).
- Commands (README Quickstart): `vcs init demo`, `vcs zone create engine --path engine`, `vcs zone create assets --path assets`,
  `vcs add . && vcs commit -m "initial"`, `vcs zone list`, `vcs remote set ../shared.vcs`, `vcs publish`,
  `vcs clone shared.vcs work --zone engine`, `vcs status`, `vcs zone add assets && vcs fetch --zone assets`, `vcs op log`.
- Explainer diagrams are drawn schematically; they make no performance claims.

Reproduce: `cargo build --release -p vcs-cli && scripts/media/record_demo.sh && python3 scripts/media/gen_media.py all`
(needs Python Pillow and ffmpeg; fonts are macOS system fonts with DejaVu fallback).
