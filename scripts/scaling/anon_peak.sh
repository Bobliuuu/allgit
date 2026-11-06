#!/bin/bash
# usage: anon.sh LABEL cmd... ; samples RssAnon/RssFile peak (kB) of the process every 0.5s
label=$1; shift
t0=$(date +%s.%N)
"$@" > /tmp/anon.$$.out 2>&1 &
pid=$!
pa=0; pf=0
while kill -0 $pid 2>/dev/null; do
  r=$(grep -E 'RssAnon|RssFile' /proc/$pid/status 2>/dev/null | awk '{print $2}' | tr '\n' ' ')
  set -- $r
  a=${1:-0}; f=${2:-0}
  [ "$a" -gt "$pa" ] && pa=$a; [ "$f" -gt "$pf" ] && pf=$f
  sleep 0.5
done
wait $pid; rc=$?
t1=$(date +%s.%N)
echo "{\"label\":\"$label\",\"wall_s\":$(echo "$t1 - $t0" | bc),\"peak_anon_kb\":$pa,\"peak_file_kb\":$pf,\"rc\":$rc}"
tail -2 /tmp/anon.$$.out >&2; rm -f /tmp/anon.$$.out
