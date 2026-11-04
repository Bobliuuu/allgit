V=~/rt/scalability/target/release/vcs
n=$1; W=~/rt/scalability-probe/tags-$n; rm -rf $W; mkdir -p $W; cd $W
python3 ../gen.py tags $n > s.stream
$V init r >/dev/null; cd r
/usr/bin/time -f "import $n tags %e s %M KB" $V import-git ../s.stream >/dev/null
ls -l .vcs/refs | awk '{print "refs bytes",$5}'
for c in "tag nt1 HEAD" "status" "tag" "branch b1" "log --oneline -n 1" "gc"; do /usr/bin/time -f "$c: %e s %M KB" $V $c >/dev/null; done
