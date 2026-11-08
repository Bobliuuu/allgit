#!/usr/bin/env bash
# Runs the README Quickstart with a freshly built binary and writes a real transcript.
# usage: record_demo.sh [path/to/vcs] [out-transcript]
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VCS="${1:-$ROOT/target/release/vcs}"
OUT="${2:-$ROOT/docs/media/transcript.txt}"
W="$(cd "$(mktemp -d)" && pwd -P)"; trap 'rm -rf "$W"' EXIT
export VCS_AUTHOR_NAME=Ada VCS_AUTHOR_EMAIL=ada@example.com
vcs() { "$VCS" "$@"; }
: > "$OUT"
run() {  # echo the command, then run it, capturing output
  printf '$ %s\n' "$1" >> "$OUT"
  ( eval "$1" ) >> "$OUT" 2>&1 || true
}
cd "$W"
run 'vcs init demo'
cd demo
run 'mkdir engine assets'
run "echo 'fn main(){}' > engine/main.rs; echo bin > assets/logo.bin; echo hi > README.md"
run 'vcs zone create engine --path engine'
run 'vcs zone create assets --path assets'
run 'vcs add . && vcs commit -m "initial"'
run 'vcs zone list'
run 'vcs remote set ../shared.vcs'
run 'vcs publish'
cd ..
run 'vcs clone shared.vcs work --zone engine'
cd work
run 'vcs status'
run 'vcs zone add assets && vcs fetch --zone assets'
run 'vcs op log'
# normalise the temp dir out of the transcript
sed -i.bak "s#$W#~/demo-run#g" "$OUT" && rm -f "$OUT.bak"
