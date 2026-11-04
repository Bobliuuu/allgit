#!/bin/sh
# Generate docs/guide/12-command-reference.md from `vcs --help`.
# Usage: scripts/gen_cli_reference.sh [--check]
#   VCS_BIN=path/to/vcs   binary to ask (default: target/release/vcs)
# Without --check it rewrites the file. With --check it exits 1 if the committed file differs.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
VCS_BIN=${VCS_BIN:-$ROOT/target/release/vcs}
OUT=$ROOT/docs/guide/12-command-reference.md
MODE=${1:-write}

# Names listed under "Commands:" of a help text, without "help".
subcommands() {
  awk '/^Commands:/{on=1; next} /^[A-Za-z]/{on=0} on && NF && $1!="help"{print $1}'
}

emit() {  # emit <depth> <command words...>
  depth=$1; shift
  hashes=$(printf '%*s' "$depth" '' | tr ' ' '#')
  printf '\n%s `vcs%s`\n\n```text\n' "$hashes" "$( [ $# -gt 0 ] && printf ' %s' "$*" )"
  # shellcheck disable=SC2086
  "$VCS_BIN" "$@" --help
  printf '```\n'
  # shellcheck disable=SC2086
  for sub in $("$VCS_BIN" "$@" --help | subcommands); do
    emit $((depth + 1)) "$@" "$sub"
  done
}

gen() {
  cat <<'HDR'
# 12. Command reference

This page is generated from the binary. Do not edit it by hand. Regenerate it with
`scripts/gen_cli_reference.sh`. A test (`crates/vcs-cli/tests/cli_reference.rs`) fails when it is out of date.

Every command also accepts `-C <DIR>` and `--json`. `--json` works only for the commands listed in
[../cli.md](../cli.md); the others reply `--json is not supported for this command`.
HDR
  printf '\n```text\n'
  "$VCS_BIN" --version
  printf '```\n'
  printf '\n## `vcs`\n\n```text\n'
  "$VCS_BIN" --help
  printf '```\n'
  for c in $("$VCS_BIN" --help | subcommands); do
    emit 2 "$c"
  done
}

if [ "$MODE" = "--check" ]; then
  tmp=$(mktemp)
  gen > "$tmp"
  if cmp -s "$tmp" "$OUT"; then rm -f "$tmp"; echo "command reference is up to date"; exit 0; fi
  diff -u "$OUT" "$tmp" | head -40 || true
  rm -f "$tmp"
  echo "docs/guide/12-command-reference.md is stale: run scripts/gen_cli_reference.sh" >&2
  exit 1
fi
gen > "$OUT"
echo "wrote $OUT"
