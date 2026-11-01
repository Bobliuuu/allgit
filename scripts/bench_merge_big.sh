#!/bin/bash
# Benchmark merge / cherry-pick / revert / stash / rebase on a big tree, vcs vs git, interleaved.
# Usage: scripts/bench_merge_big.sh <workdir> <vcs-binary>... [-- runs=5 files=100000 commits=200]
# Prints uptime (load), then per scenario: median wall ms and median user+sys CPU ms per tool.
# Each run operates on a fresh `cp -a` of a prebuilt template repo (template build is not timed).
set -eu
W=$(realpath -m "$1"); shift
BINS=(); while [ $# -gt 0 ] && [ "$1" != "--" ]; do BINS+=("$(realpath "$1")"); shift; done
[ "${1:-}" = "--" ] && shift
RUNS=5; FILES=100000; COMMITS=200
for kv in "$@"; do eval "${kv%%=*}=${kv#*=}"; done
RUNS=${runs:-$RUNS}; FILES=${files:-$FILES}; COMMITS=${commits:-$COMMITS}
export VCS_AUTHOR_NAME=bench VCS_AUTHOR_EMAIL=b@x GIT_AUTHOR_NAME=b GIT_AUTHOR_EMAIL=b@x GIT_COMMITTER_NAME=b GIT_COMMITTER_EMAIL=b@x
mkdir -p "$W"; cd "$W"
echo "host=$(hostname) cores=$(nproc) files=$FILES commits=$COMMITS runs=$RUNS"; uptime
# --- template: FILES files in 10x10x(FILES/1000) dirs, 10 files each... layout d<a>/d<b>/d<c>/f<n>.txt
gen_tree() { python3 - "$1" "$FILES" <<'PY'
import os,sys
root,n=sys.argv[1],int(sys.argv[2])
per=10
for i in range(n):
    d=i//per
    p=os.path.join(root,"d%d"%(d%10),"d%d"%((d//10)%10),"d%d"%(d//100))
    os.makedirs(p,exist_ok=True)
    with open(os.path.join(p,"f%d.txt"%(i%per)),"w") as f:
        f.write("".join("line %d of file %d\n"%(k,i) for k in range(20)))
PY
}
# regions: feat edits [0,F/4), stash [F/4,F/2) , main [F/2,3F/4), long [3F/4,F): no overlap, so merges are clean
# edit file number $1 : append a line tagged $2
fpath() { local i=$(( $1 % FILES )) per=10; local d=$((i/per)); echo "d$((d%10))/d$(((d/10)%10))/d$((d/100))/f$((i%per)).txt"; }
edit() { echo "$2" >> "$(fpath "$1")"; }
if [ ! -f tmpl.done ]; then
  rm -rf tmplv tmplg; mkdir tmplv; gen_tree tmplv
  cp -a tmplv tmplg
  # vcs template: main + feat(5 commits) ; main +5 commits ; plus pick/rebase branches
  (cd tmplv; "${BINS[0]}" init >/dev/null; "${BINS[0]}" add . >/dev/null; "${BINS[0]}" commit -m init >/dev/null)
  (cd tmplg; git init -q -b main; git config gc.auto 0; git add -A; git commit -q -m init)
  touch tmpl.done
fi
build_history() { # $1 = vcs|git, dir
  local tool=$1 d=$2; cd "$d"
  V() { if [ "$tool" = vcs ]; then "${BINS[0]}" "$@" >/dev/null; else git "$@" >/dev/null 2>&1; fi; }
  C() { if [ "$tool" = vcs ]; then "${BINS[0]}" add . >/dev/null; "${BINS[0]}" commit -m "$1" >/dev/null; else git add -A; git commit -q -m "$1"; fi; }
  local sw=switch; [ "$tool" = git ] && sw=switch
  V branch feat; V branch long; V branch pick
  V $sw feat; for c in 1 2 3 4 5; do for k in 1 2 3 4; do edit $(( (c*37+k) % (FILES/4) )) "feat $c $k"; done; C "feat$c"; done
  V $sw main
  for c in 1 2 3 4 5; do for k in 1 2 3 4; do edit $(( FILES/2 + (c*41+k) % (FILES/4) )) "main $c $k"; done; C "main$c"; done
  V $sw long; for c in $(seq 1 $COMMITS); do for k in 1 2 3; do edit $(( 3*FILES/4 + (c*7919+k*104729) % (FILES/4) )) "long $c $k"; done; C "long$c"; done
  V $sw main
  cd "$W"
}
if [ ! -f hist.done ]; then
  # cp -a copies preserve mtimes; histories differ only by path, so build in place once
  build_history vcs "$W/tmplv"; build_history git "$W/tmplg"; touch hist.done
fi
ms() { echo $(( $1 / 1000000 )); }
# run one timed command in dir; echo "wall_ms cpu_ms"
timed() { local d=$1; shift
  local o; o=$( cd "$d"; /usr/bin/time -f "%e %U %S %x" "$@" 2>&1 >/dev/null | tail -1 )
  # a failing command would make the timing meaningless: record it loudly
  if [ "$(echo "$o" | awk '{print $4}')" != 0 ]; then echo "FAILED rc=$(echo "$o" | awk '{print $4}'): $* (in $d)" >> "$W/fail.log"; fi
  echo "$o" | awk '{printf "%d %d\n",$1*1000,($2+$3)*1000}'
}
med() { local d=$1; shift
  local o; o=$( cd "$d"; /usr/bin/time -f "%e %U %S" "$@" 2>&1 >/dev/null | tail -1 )
  echo "$o" | awk '{printf "%d %d\n",$1*1000,($2+$3)*1000}'
}
med() { sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}'; }
declare -A WALL CPU
# scenarios: name | prepare(vcs) | cmd(vcs) | prepare(git) | cmd(git)
scen() { # name
  local name=$1
  for r in $(seq 1 "$RUNS"); do
    for t in git "${BINS[@]}"; do
      local tag d src
      if [ "$t" = git ]; then tag=git; src=tmplg; else tag=$(basename "$(dirname "$t")")/$(basename "$t"); src=tmplv; fi
      d=$W/run-$$; rm -rf "$d"; cp -a "$W/$src" "$d"
      local out
      case $name in
        stash)
          ( cd "$d"; for i in $((FILES/4+7)) $((FILES/4+8)) $((FILES/4+9)); do echo wip >> "$(fpath $i)"; done )
          if [ "$t" = git ]; then
            o1=$(timed "$d" git stash push -m x); o2=$(timed "$d" git stash pop)
          else
            o1=$(timed "$d" "$t" stash push -m x); o2=$(timed "$d" "$t" stash pop)
          fi
          out=$(echo "$o1 $o2" | awk '{print $1+$3, $2+$4}');;
        merge)
          if [ "$t" = git ]; then out=$(timed "$d" git merge --no-edit -q feat); else out=$(timed "$d" "$t" merge feat); fi;;
        cherry-pick)
          if [ "$t" = git ]; then out=$(timed "$d" git cherry-pick feat); else out=$(timed "$d" "$t" cherry-pick feat); fi;;
        revert)
          if [ "$t" = git ]; then out=$(timed "$d" git revert --no-edit main); else out=$(timed "$d" "$t" revert main); fi;;
        rebase)
          if [ "$t" = git ]; then ( cd "$d"; git switch -q long ); out=$(timed "$d" git rebase main)
          else ( cd "$d"; "$t" switch long >/dev/null ); out=$(timed "$d" "$t" rebase main); fi;;
      esac
      echo "$name $tag $out" >> results.$$.txt
      rm -rf "$d"
    done
  done
}
rm -f results.$$.txt fail.log
for s in ${scenarios:-merge cherry-pick revert stash rebase}; do scen "$s"; done
echo "scenario tool wall_ms_median cpu_ms_median (n=$RUNS, interleaved)"
awk '{k=$1" "$2; w[k]=w[k]" "$3; c[k]=c[k]" "$4} END{for(k in w){print k"|"w[k]"|"c[k]}}' results.$$.txt | sort | while IFS='|' read k w c; do
  mw=$(echo $w | tr ' ' '\n' | med); mc=$(echo $c | tr ' ' '\n' | med); echo "$k wall=$mw cpu=$mc  (wall runs:$w)"
done
uptime
[ -s "$W/fail.log" ] && { echo "WARNING: failing commands:"; sort "$W/fail.log" | uniq -c; } || echo "all timed commands exited 0"
