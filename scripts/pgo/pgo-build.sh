#!/usr/bin/env bash
# Reproducible PGO release build of vcs / vcs-watchd / vcs-server on top of the fat-LTO release profile.
#
#   scripts/pgo/pgo-build.sh [OUT_DIR]        (default OUT_DIR: target/pgo-dist)
#
# Needs: rustup component add llvm-tools   (provides llvm-profdata matching rustc's LLVM; a system
# llvm-profdata of another major version fails with "unsupported instrumentation profile format version").
# Steps: 1) instrumented build, 2) training run = scripts/pgo/ab.py scenarios on a generated repo
# (status, dirty status, add, diff, log, clone, gc, client+server over HTTP; PGO_FILES files, default 100000),
# 3) llvm-profdata merge, 4) optimized build with -Cprofile-use.  Takes ~3 full release builds of CPU time
# (about 25-40 min on a loaded laptop). Nothing is written outside target/ and the work dir.
# Never commit the .profdata: it is tied to the exact source and rustc, so regenerate it on every release.
set -euo pipefail
cd "$(dirname "$0")/../.."
ROOT=$PWD
OUT=${1:-$ROOT/target/pgo-dist}
FILES=${PGO_FILES:-100000}
WORK=${PGO_WORK:-$ROOT/target/pgo-work}
HOST=$(rustc -vV | sed -n 's/^host: //p')
SYSROOT=$(rustc --print sysroot)
PROFDATA=$SYSROOT/lib/rustlib/$HOST/bin/llvm-profdata
[ -x "$PROFDATA" ] || { echo "missing $PROFDATA: run 'rustup component add llvm-tools'" >&2; exit 1; }
export CARGO_INCREMENTAL=0
PKGS=(-p vcs-cli -p vcs-watchd -p vcs-server -p vcs-bench)
GEN=$ROOT/target/pgo-gen
rm -rf "$WORK" "$OUT"; mkdir -p "$WORK/raw" "$OUT"

echo "== 1/4 instrumented build"
# Explicit --target keeps build scripts / proc macros un-instrumented; output lands in target/pgo-gen/$HOST/release.
RUSTFLAGS="-Cprofile-generate=$WORK/raw" cargo build --release --target "$HOST" --target-dir "$GEN" "${PKGS[@]}"
IB=$GEN/$HOST/release

echo "== 2/4 training run ($FILES files)"
export LLVM_PROFILE_FILE="$WORK/raw/%m_%p.profraw"
python3 scripts/pgo/ab.py setup --work "$WORK/repo" --datagen "$IB/datagen" --vcs "$IB/vcs" --n "$FILES"
python3 scripts/pgo/ab.py run --work "$WORK/repo" --variant "train=$IB" --runs "${PGO_RUNS:-3}" >/dev/null
unset LLVM_PROFILE_FILE

echo "== 3/4 merge"
"$PROFDATA" merge -o "$WORK/merged.profdata" "$WORK"/raw/*.profraw
ls -l "$WORK/merged.profdata"

echo "== 4/4 optimized build"
RUSTFLAGS="-Cprofile-use=$WORK/merged.profdata -Cllvm-args=-pgo-warn-missing-function" \
  cargo build --release --target "$HOST" --target-dir "$ROOT/target/pgo-use" "${PKGS[@]}"
cp "$ROOT/target/pgo-use/$HOST/release/"{vcs,vcs-watchd,vcs-server} "$OUT/"
echo "done: $OUT/{vcs,vcs-watchd,vcs-server}  (profile: $WORK/merged.profdata)"
