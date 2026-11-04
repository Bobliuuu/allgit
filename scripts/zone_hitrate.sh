#!/bin/sh
# Hit rate of the zone predictor on real histories (issue #465). Needs git and cargo; downloads commits and trees only.
# Usage: scripts/zone_hitrate.sh [work-dir] [k]     (default: a temp dir, k = 6)
# Runs this repository's own history (hour buckets: it was written in a few days) and the replay repos of docs/merge.md.
# Output is markdown on stdout; the numbers and what they mean are in docs/research/zone-profile-hitrate.md.
set -e
WORK="${1:-$(mktemp -d)}"; K="${2:-6}"; mkdir -p "$WORK"
cargo build -q -p vcs-repo --example zone_hitrate
BIN="$(cargo metadata --format-version 1 --no-deps | sed 's/.*"target_directory":"\([^"]*\)".*/\1/')/debug/examples/zone_hitrate"
"$BIN" "$(git rev-parse --show-toplevel)" --label "allgit (this repository, 2 h buckets)" --k "$K" --bucket-hours 2 --cochange-refresh-hours 1
for r in clap-rs/clap gin-gonic/gin nestjs/nest typeorm/typeorm psf/requests spf13/cobra; do
  n=$(basename "$r")
  [ -d "$WORK/$n.git" ] || git clone -q --bare --filter=blob:none "https://github.com/$r.git" "$WORK/$n.git"
  echo; "$BIN" "$WORK/$n.git" --label "$n" --k "$K"
done
