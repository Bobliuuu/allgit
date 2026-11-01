#!/usr/bin/env bash
# Perforce (Helix Core) benchmark on the same generated datasets as scripts/bench_scale.sh.
# Perforce is client/server: every p4 command below talks to a local p4d over TCP on loopback.
#
# usage: scripts/bench_p4.sh <flat|deep|binary> [n=100000] [runs=5]
#   env: P4=~/p4/bin/p4  P4D=~/p4/bin/p4d  DATAGEN=~/datagen  BENCH_TMP=<scratch dir>  P4PORT_NUM=11666
#        MIN_FREE_GB (abort below this free space, default 8)
# Server: fresh root, journal OFF (-J off), no log (-L /dev/null), 127.0.0.1 only, no licence file (free tier).
# Workflows: add (reconcile -a, and `p4 -x - add`), submit of everything (initial), 1-file submit,
# status (`reconcile -n`, clean / 1% modified), log (`changes -m 50`), checkout (`sync` @1 -> @2, 1% differ),
# clone (`sync` into a brand-new empty client). Prep is untimed. The server is stopped and its root deleted at the end.
set -euo pipefail

LAYOUT="${1:-flat}"
N="${2:-100000}"
RUNS="${3:-5}"
P4="${P4:-$HOME/p4/bin/p4}"
P4D="${P4D:-$HOME/p4/bin/p4d}"
DATAGEN="${DATAGEN:-$HOME/datagen}"
PORTNUM="${P4PORT_NUM:-11666}"
MIN_FREE_GB="${MIN_FREE_GB:-8}"
PARENT="${BENCH_TMP:-$HOME/p4/work}"
mkdir -p "$PARENT"
WORK="$(mktemp -d "$PARENT/p4scale.XXXXXX")"
PROOT="$WORK/server"
W="$WORK/ws"
export P4PORT="127.0.0.1:$PORTNUM" P4USER=bench P4CLIENT=ws P4CONFIG= P4ENVIRO=/dev/null P4TICKETS="$WORK/tickets"
export P4 P4D PROOT W WORK

stop_server() {
  "$P4" admin stop >/dev/null 2>&1 || true
  local i
  for i in $(seq 1 100); do pgrep -f "$P4D -r $PROOT " >/dev/null || break; sleep 0.1; done
  pkill -f "$P4D -r $PROOT " 2>/dev/null || true
}
trap 'stop_server; rm -rf "$WORK"' EXIT

free_gb() { df -k "$WORK" | awk 'NR==2{printf "%.2f", $4/1048576}'; }
guard() {
  local f; f="$(free_gb)"
  if awk -v f="$f" -v m="$MIN_FREE_GB" 'BEGIN{exit !(f<m)}'; then
    echo "ABORT: free space ${f} GiB < ${MIN_FREE_GB} GiB; cleaning up" >&2
    exit 3
  fi
}
load() { cut -d" " -f1 /proc/loadavg; }

# Fresh server: new empty root, journal off. Creates user and client "ws" rooted at $W.
start_server() {
  stop_server
  rm -rf "$PROOT"; mkdir -p "$PROOT"
  ( cd "$PROOT" && exec "$P4D" -r "$PROOT" -J off -L /dev/null -p "$P4PORT" >/dev/null 2>&1 ) &
  local i
  for i in $(seq 1 100); do "$P4" info >/dev/null 2>&1 && break; sleep 0.1; done
  "$P4" info >/dev/null 2>&1 || { echo "p4d did not start" >&2; exit 4; }
  # p4d 2026.1 refuses all access until the (auto-created) user has a password: set one and log in (ticket in $P4TICKETS)
  printf 'Benchpass1!\nBenchpass1!\n' | "$P4" passwd >/dev/null 2>&1
  echo 'Benchpass1!' | "$P4" login >/dev/null 2>&1
  "$P4" user -o | "$P4" user -i >/dev/null 2>&1 || true
  make_client ws "$W"
}
make_client() { # name root
  P4CLIENT="$1" "$P4" client -o | sed "s|^Root:.*|Root:\t$2|" | P4CLIENT="$1" "$P4" client -i >/dev/null
}
p4d_hwm() { awk '/VmHWM/{print $2}' "/proc/$(pgrep -f "$P4D -r $PROOT " | head -1)/status"; }

echo "## Environment"
echo "date:     $(date -u +%FT%TZ)"
echo "uname:    $(uname -a)"
echo "cpu:      $(grep -m1 "model name" /proc/cpuinfo | cut -d: -f2- | xargs)  cores=$(nproc)  mem=$(awk '/MemTotal/{printf "%.0f", $2/1048576}' /proc/meminfo)GiB  fs=$(df -T "$WORK" | awk 'NR==2{print $2}')"
echo "p4:       $("$P4" -V | tail -1)"
echo "p4d:      $("$P4D" -V | grep Rev)"
echo "server:   $P4PORT, root=fresh dir, journal off (-J off), log /dev/null, no licence file"
echo "dataset:  layout=$LAYOUT files=$N seed=42; runs per cell=$RUNS; WARM cache"
echo "load1 at start: $(load)   free disk: $(free_gb) GiB"
echo

timeit() { python3 -c 'import subprocess,sys,time
t=time.perf_counter();r=subprocess.run(["bash","-c",sys.argv[1]],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);print("%.1f"%((time.perf_counter()-t)*1000), file=sys.stdout);sys.exit(0)' "$1"; }

cell() {
  local name="$1" prep="$2" cmd="$3" ts=() i l0 l1
  l0="$(load)"
  for ((i = 0; i < RUNS; i++)); do
    guard
    bash -c "$prep" >/dev/null 2>&1 || { echo "prep failed for $name (run $i)" >&2; }
    ts+=("$(timeit "$cmd")")
  done
  l1="$(load)"
  printf '| %-30s | %s | %s | load1 %s -> %s |\n' "$name" \
    "$(printf '%s ' "${ts[@]}")" \
    "$(printf '%s\n' "${ts[@]}" | sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}')" "$l0" "$l1"
}

# Same selection as bench_scale.sh: every 100th file (sorted). Files are read-only after submit, so chmod first.
modify_1pct() {
  python3 -I - "$1" <<'E'
import os, sys
tag = sys.argv[1]
fs = []
for d, ds, files in os.walk("."):
    fs += [os.path.join(d, f) for f in files]
fs.sort()
for i, p in enumerate(fs[::100]):
    os.chmod(p, 0o644)
    with open(p, "wb") as fh:
        fh.write((f"modified {tag} {i}\n").encode() * 8)
E
}
# Print path of history file number K (as touch_one in bench_scale.sh); caller does `p4 edit` then writes it.
pick_one() {
  python3 -I - "$1" <<'E'
import os, sys
k = int(sys.argv[1])
fs = []
for d, ds, files in os.walk("."):
    fs += [os.path.join(d, f) for f in files]
fs.sort()
print(fs[k * 997 % len(fs)])
E
}
edit_one() { # K: p4 edit + rewrite file K (untimed prep for a 1-file submit)
  local f; f="$(pick_one "$1")"
  "$P4" -q edit "$f" >/dev/null
  printf 'history %s\n' "$1" > "$f"
}

mkdir -p "$W"
guard
"$DATAGEN" "$LAYOUT" "$N" "$W" >/dev/null
echo "(workspace size: $(du -sk "$W" | cut -f1) KiB; files: $(find "$W" -type f | wc -l))" >&2
cd "$W"
export -f edit_one pick_one 2>/dev/null || true

echo "## Results (ms; raw runs, median, load average)"
echo "| workflow                       | raw runs (ms) | median | load |"
echo "|--------------------------------|---------------|--------|------|"

start_server
cell "add: reconcile -a"       "$P4 revert -k //... ; true"             "$P4 -q reconcile -a //..."
cell "add: p4 -x - add"        "$P4 revert -k //... ; true"             "find . -type f | $P4 -q -x - add"
echo "(opened files after add: $("$P4" opened //... 2>/dev/null | wc -l))" >&2

# initial submit: fresh server each run so every run is a true first submit of N files
cell "submit all (initial)"    "cd $W; $(declare -f start_server stop_server make_client); start_server; $P4 -q reconcile -a //..." "$P4 submit -d c1"
# the last run left the new server with change 1 submitted
echo "(changes after initial submit: $("$P4" changes | wc -l); p4d VmHWM: $(p4d_hwm) KiB)" >&2
sleep 1
DB_KB=$(cd "$PROOT" && du -ck db.* 2>/dev/null | tail -1 | cut -f1)
TOT_KB=$(du -sk "$PROOT" | cut -f1)
DEPOT_KB=$(du -sk "$PROOT/depot" 2>/dev/null | cut -f1)
echo "(server root after initial submit: total ${TOT_KB} KiB = depot ${DEPOT_KB} + db.* ${DB_KB}; workspace $(du -sk "$W" | cut -f1) KiB; free $(free_gb) GiB)" >&2

cell "status clean (reconcile -n)" "true"                               "$P4 reconcile -n //..."
echo "(reconcile -n lines on clean tree: $("$P4" reconcile -n //... 2>&1 | wc -l))" >&2
modify_1pct a
cell "status 1% mod (reconcile -n)" "true"                              "$P4 reconcile -n //..."
echo "(reconcile -n lines with 1% modified: $("$P4" reconcile -n //... 2>&1 | wc -l))" >&2
"$P4" -q reconcile -e //... >/dev/null 2>&1
"$P4" submit -d c2 >/dev/null 2>&1                                    # change 2: 1% differs from change 1
echo "(changes now: $("$P4" changes | wc -l))" >&2

n=0
cell "submit 1 file (edit untimed)" "$(declare -f edit_one pick_one); P4=$P4; n=\$((RANDOM)); edit_one \$n" "$P4 submit -d one"
for k in $(seq 1 50); do edit_one "$k"; "$P4" submit -d h$k >/dev/null 2>&1; done
echo "(changes now: $("$P4" changes | wc -l))" >&2
cell "log (changes -m 50)"      "true"                                "$P4 changes -m 50"
cell "checkout (sync @1->@2, 1%)" "$P4 -q sync //...@1"               "$P4 -q sync //...@2"
"$P4" -q sync //... >/dev/null 2>&1

# clone: brand-new empty client each run, full sync of head
rm -f "$WORK/clone.n"
cell "clone (sync into empty ws)" "c=\$(( \$(cat $WORK/clone.n 2>/dev/null || echo 0) + 1 )); echo \$c > $WORK/clone.n; rm -rf $WORK/clone*/ ; mkdir -p $WORK/clone\$c; $(declare -f make_client); P4=$P4; make_client cl\$c $WORK/clone\$c" "c=\$(cat $WORK/clone.n); P4CLIENT=cl\$c $P4 -q sync //..."
c=$(cat "$WORK/clone.n")
echo "(cloned files in last clone: $(find "$WORK/clone$c" -type f 2>/dev/null | wc -l))" >&2
echo
echo "free disk at end: $(free_gb) GiB; load1 at end: $(load)"
