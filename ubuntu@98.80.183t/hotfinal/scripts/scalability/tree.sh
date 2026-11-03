V=~/rt/scalability/target/release/vcs
kind=$1; n=$2; W=~/rt/scalability-probe/$kind-$n; rm -rf $W; mkdir -p $W; cd $W
python3 ../gen.py $kind $n > s.stream
$V init r >/dev/null; cd r
/usr/bin/time -f "import $kind $n: %e s %M KB" $V import-git ../s.stream >/dev/null
du -sh .vcs | cut -f1 | sed 's/^/.vcs /'; ls | head -3
for c in "status" "ls-files" "log --oneline -n 1" "show --stat HEAD" "gc"; do /usr/bin/time -f "$c: %e s %M KB" $V $c >/dev/null; done
echo y >> $(ls -d */ | head -1)$(ls $(ls -d */ | head -1) | head -1) 2>/dev/null
/usr/bin/time -f "status after 1 edit: %e s %M KB" $V status >/dev/null
$V config set user.name a >/dev/null; $V config set user.email a@b >/dev/null
$V add -A >/dev/null 2>&1 || $V add . >/dev/null
/usr/bin/time -f "commit after 1 edit: %e s %M KB" $V commit -m e >/dev/null
