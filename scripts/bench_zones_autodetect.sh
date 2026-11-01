#!/usr/bin/env bash
# Bytes fetched by `vcs clone --for` versus a full clone, on a generated multi-zone monorepo.
#
# usage: scripts/bench_zones_autodetect.sh [files=100000] [runs=5]
#   env: VCS_BIN (default target/release/vcs), BENCH_TMP (scratch parent dir)
# Generates ~files files in 50 Cargo packages (crates/pNN, a dependency chain p00 <- p01 <- ... in
# groups of 4), 20 npm packages (web/wNN) and a docs tree, commits once, runs `vcs zone suggest`
# and `vcs zone apply --yes` for every suggestion (this is the auto-detect path under test),
# publishes to a directory remote, then alternates full clone / clone --for, `runs` times each.
# Reports per-run bytes and requests (CountingBackend), medians, wall ms, and the machine load.
# Bytes are deterministic; wall time is only meaningful on a quiet machine.
set -euo pipefail
N="${1:-100000}"
RUNS="${2:-5}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VCS="${VCS_BIN:-$ROOT/target/release/vcs}"
WORK="$(mktemp -d "${BENCH_TMP:-${TMPDIR:-/tmp}}/zones-autodetect.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
export VCS_AUTHOR_NAME=b VCS_AUTHOR_EMAIL=b@x.invalid
load() { if [ -r /proc/loadavg ]; then cut -d" " -f1 /proc/loadavg; else sysctl -n vm.loadavg | tr -d "{}" | awk '{print $1}'; fi; }
now_ms() { python3 -c 'import time;print(int(time.time()*1000))'; }
echo "## Environment"
echo "date: $(date -u +%FT%TZ)  uname: $(uname -srm)  cores: $(nproc 2>/dev/null || sysctl -n hw.ncpu)  load1 at start: $(load)"
echo "vcs: $("$VCS" --version)  files requested: $N  runs per cell: $RUNS"

python3 - "$WORK/mono" "$N" <<'PY'
import os, sys, random
root, n = sys.argv[1], int(sys.argv[2])
random.seed(42)
os.makedirs(root)
def w(p, c):
    p = os.path.join(root, p); os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, "w") as f: f.write(c)
words = ["alpha","beta","gamma","delta","omega","sigma","kappa","theta","lambda","zeta","ledger","cursor","packet","socket","buffer","vector","matrix","kernel"]
def body(i):
    return "\n".join(" ".join(random.choice(words) + str(random.randrange(10**6)) for _ in range(8)) for _ in range(30)) + "\n"
npk, nweb, ndocs = 50, 20, n // 20
per = (n - ndocs) // (npk + nweb)
w("Cargo.toml", '[workspace]\nmembers = ["crates/*"]\n')
for i in range(npk):
    name = f"p{i:02d}"
    deps = f'{"p%02d" % (i-1)} = {{ path = "../p%02d" }}\n' % (i-1) if i % 4 else ""
    w(f"crates/{name}/Cargo.toml", f'[package]\nname = "{name}"\nversion = "0.1.0"\n\n[dependencies]\n{deps}')
    for j in range(per - 1): w(f"crates/{name}/src/m{j//100}/f{j}.rs", body(j))
for i in range(nweb):
    name = f"w{i:02d}"
    w(f"web/{name}/package.json", '{"name":"%s","version":"1.0.0"}\n' % name)
    for j in range(per - 1): w(f"web/{name}/src/c{j//100}/f{j}.js", body(j))
for j in range(ndocs): w(f"docs/d{j//100}/p{j}.md", body(j))
w("CODEOWNERS", "/web/w03/ @alice\n/crates/p10/ @bob\n")
w("README", "monorepo\n")
PY
cd "$WORK/mono"
echo "generated files: $(find . -type f | wc -l)  size: $(du -sh . | cut -f1)"
"$VCS" init . >/dev/null 2>&1 || true
# NATIVE=1: create the zones up front with `zone create` (no split of existing history) instead of
# using suggest + apply, to separate the cost of the split machinery from the cost of selection.
if [ "${NATIVE:-0}" = 1 ]; then
  for d in crates/p?? web/w??; do "$VCS" zone create "$(basename "$d")" --path "$d" >/dev/null; done
fi
"$VCS" add . >/dev/null; "$VCS" commit -m init >/dev/null
if [ "${NATIVE:-0}" = 1 ]; then echo "NATIVE=1: zones created up front; suggest/apply skipped"; else
t0=$(now_ms); "$VCS" --json zone suggest > "$WORK/suggest.json"; t1=$(now_ms)
echo "zone suggest: $((t1-t0)) ms for $("$VCS" ls-files | wc -l) files; zones proposed: $(python3 -c 'import json;print(len(json.load(open("'"$WORK"'/suggest.json"))["zones"]))')"
paths=$(python3 -c 'import json;print("\n".join(z["path"] for z in json.load(open("'"$WORK"'/suggest.json"))["zones"]))')
t0=$(now_ms); for p in $paths; do "$VCS" zone apply "$p" --yes >/dev/null; done; t1=$(now_ms)
echo "zone apply x$(echo "$paths" | wc -l): $((t1-t0)) ms total"; fi
"$VCS" fsck | tail -1
"$VCS" remote set "$WORK/remote" >/dev/null
t0=$(now_ms); "$VCS" publish >/dev/null; t1=$(now_ms); echo "publish: $((t1-t0)) ms; remote size: $(du -sh "$WORK/remote" | cut -f1)"
cd "$WORK"
echo "dry-run estimate for --for crates/p10/src/m0/f1.rs:"
"$VCS" --json clone "$WORK/remote" dry --for crates/p10/src/m0/f1.rs --dry-run | python3 -c 'import json,sys;d=json.load(sys.stdin);print(" zones:",[z["name"] for z in d["zones"]]," files:",d["totals"]["files_selected"],"/",d["totals"]["files_all"]," est bytes:",d["totals"]["bytes_selected"],"/",d["totals"]["bytes_all"]," planning:",d["planning_cost"])'
echo
echo "## runs (interleaved): kind bytes requests wall_ms files_on_disk"
fb=(); fr=(); ft=(); sb=(); sr=(); st=()
for r in $(seq 1 "$RUNS"); do
  t0=$(now_ms); out=$("$VCS" clone "$WORK/remote" "full$r" --stats | grep '^requests:'); t1=$(now_ms)
  b=$(echo "$out" | sed 's/.*downloaded: \([0-9]*\) bytes.*/\1/'); q=$(echo "$out" | sed 's/requests: \([0-9]*\),.*/\1/')
  fb+=("$b"); fr+=("$q"); ft+=("$((t1-t0))"); echo "full $b $q $((t1-t0)) $(find full$r -type f -not -path '*/.vcs/*' | wc -l)"
  t0=$(now_ms); out=$("$VCS" --json clone "$WORK/remote" "sel$r" --for crates/p10/src/m0/f1.rs --stats 2>&1 >"$WORK/sel.json" | grep 'requests:'); t1=$(now_ms)
  b=$(echo "$out" | sed 's/.*downloaded: \([0-9]*\) bytes.*/\1/'); q=$(echo "$out" | sed 's/.*requests: \([0-9]*\),.*/\1/')
  pb=$(python3 -c 'import json;print(json.load(open("'"$WORK"'/sel.json"))["planning_cost"]["bytes"])'); pq=$(python3 -c 'import json;print(json.load(open("'"$WORK"'/sel.json"))["planning_cost"]["requests"])')
  sb+=("$b"); sr+=("$q"); st+=("$((t1-t0))")
  echo "for  $b $q $((t1-t0)) $(find sel$r -type f -not -path '*/.vcs/*' | wc -l)   (of which planning: $pb bytes/$pq req)"
  rm -rf "full$r" "sel$r"
done
med() { printf '%s\n' "$@" | sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}'; }
echo
echo "## medians over $RUNS runs"
echo "full clone: bytes $(med "${fb[@]}")  requests $(med "${fr[@]}")  wall_ms $(med "${ft[@]}")"
echo "clone --for (total transfer, planning included): bytes $(med "${sb[@]}")  requests $(med "${sr[@]}")  wall_ms $(med "${st[@]}")"
echo "load1 at end: $(load)"
