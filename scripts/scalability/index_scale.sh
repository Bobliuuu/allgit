#!/bin/bash
# usage: index_scale.sh N...   index-op cost vs file count (release vcs + datagen via $V and $D)
# Run on a quiet box under `flock ~/bench.lock`; scratch under ~/rt/scaledocs-idx.
V=${V:-~/rt/scaledocs/target/release/vcs}; D=${D:-~/rt/scaledocs/target/release/datagen}
export GIT_AUTHOR_NAME=b GIT_AUTHOR_EMAIL=b@x.invalid GIT_COMMITTER_NAME=b GIT_COMMITTER_EMAIL=b@x.invalid
RUNS=5
ms(){ python3 -c 'import subprocess,sys,time
t=time.perf_counter();r=subprocess.run(["bash","-c",sys.argv[1]],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);print("%.1f%s"%((time.perf_counter()-t)*1000,"" if r.returncode==0 else "FAIL"))' "$1"; }
med(){ sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}'; }
cell(){ # name prep cmd
  local ts=""; for i in $(seq $RUNS); do bash -c "$2" >/dev/null 2>&1; ts="$ts$(ms "$3")\n"; done
  printf '%s: %s ms median (runs: %s)\n' "$1" "$(printf "$ts" | med)" "$(printf "$ts" | tr '\n' ' ')"; }
for N in $@; do
  W=~/rt/scaledocs-idx/n$N; rm -rf $W; mkdir -p $W; cd $W
  $D flat $N $W/r >/dev/null; cd r
  echo "## N=$N load1=$(cut -d' ' -f1 /proc/loadavg)"
  s=$(date +%s.%N); $V init >/dev/null; $V config set user.name b >/dev/null; $V config set user.email b@x.invalid >/dev/null; $V add . >/dev/null; $V commit -m c >/dev/null; echo "init+add+commit: $(echo "$(date +%s.%N)-$s"|bc) s"
  ls -l .vcs/index 2>/dev/null | awk '{print "index bytes",$5}'; head -c4 .vcs/index; echo
  F=$(ls | head -1); [ -d "$F" ] && F=$F/$(ls $F | head -1)
  echo "file=$F"
  $V status >/dev/null
  cell "status clean" true "$V status"
  cell "add one edited file" "echo x$RANDOM >> $F" "$V add $F"
  $V commit -m e >/dev/null 2>&1
  cell "rm one file (staged)" "$V restore --staged $F; $V restore $F" "$V rm $F"
  $V restore --staged $F >/dev/null 2>&1; $V restore $F >/dev/null 2>&1
  cell "restore --staged one file" "echo y$RANDOM >> $F; $V add $F" "$V restore --staged $F"
  head -c4 .vcs/index; echo " (index magic after patch ops)"
  $V restore $F >/dev/null 2>&1
  cell "commit after 1 edit" "echo z$RANDOM >> $F; $V add $F" "$V commit -m c2"
  cd ~; rm -rf $W
done
