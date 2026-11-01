#!/bin/bash
# usage: run_all.sh <outdir>   (on the Linux box; one dataset at a time)
export PATH=$PATH:/snap/bin
OUT=${1:-$HOME/wb2/out}; mkdir -p $OUT
cd $HOME/wb2
S=$(dirname "$(readlink -f "$0")")/walgit_full.py
for n in 10000 100000; do
  reps=5; [ $n = 100000 ] && reps=3
  for l in flat deep binary; do
    python3 $S $l $n $reps $OUT/res-$l-$n.json > $OUT/log-$l-$n.txt 2>&1
    python3 $S $l $n 1 $OUT/cnt-$l-$n.json --count > $OUT/logc-$l-$n.txt 2>&1
  done
done
echo ALLDONE > $OUT/done
