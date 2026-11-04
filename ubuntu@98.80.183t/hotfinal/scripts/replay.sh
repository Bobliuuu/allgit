#!/bin/sh
# Real-history merge replay (issue #108). Needs git (only here, never in the core crates) and a cargo toolchain.
# Usage: scripts/replay.sh <bare-clone-dir-or-url> <rev> [max-merges]
#   Exports the history reachable from <rev> with `git fast-export`, imports it with `vcs import-git`,
#   then runs the replay example over the branch. Prints the URL (if any) and the revision replayed.
set -e
SRC="$1"; REV="$2"; MAX="${3:-300}"
WORK="${REPLAY_WORK:-$(mktemp -d)}"; mkdir -p "$WORK"
FEATURES="structural,structural-python,structural-typescript,structural-go"
if [ -d "$SRC" ]; then BARE="$SRC"; else BARE="$WORK/src.git"; git clone -q --bare "$SRC" "$BARE"; fi
SHA=$(git -C "$BARE" rev-parse "$REV^{commit}")
git -C "$BARE" update-ref refs/heads/replay "$SHA"
echo "source=$SRC revision=$SHA"
git -C "$BARE" fast-export --show-original-ids refs/heads/replay > "$WORK/stream"
cargo build -q --release -p vcs-cli
cargo build -q --release -p vcs-repo --features "$FEATURES" --example replay
VCS="$(cargo metadata --format-version 1 --no-deps | sed 's/.*"target_directory":"\([^"]*\)".*/\1/')/release"
rm -rf "$WORK/repo"; "$VCS/vcs" init "$WORK/repo" >/dev/null
( cd "$WORK/repo" && "$VCS/vcs" import-git "$WORK/stream" | tail -3 )
"$VCS/examples/replay" "$WORK/repo" replay --max "$MAX"
