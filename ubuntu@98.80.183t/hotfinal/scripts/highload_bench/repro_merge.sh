#!/bin/sh
# Reproduce: repeated sync+merge on a shared branch with several clones.
export PATH=$HOME/rt/highload/target/release:$PATH VCS_TOKEN=tok1 VCS_PRINCIPAL=bench VCS_AUTHOR_NAME=b VCS_AUTHOR_EMAIL=b@x.invalid
W=$HOME/hl/repro; rm -rf $W; mkdir -p $W/root; cd $W
vcs-server --root $W/root --tokens $W/tokens --listen 127.0.0.1:19400 >$W/srv.log 2>&1 &
vcs-server hash-token tok1 | sed 's/^/bench write * /' > $W/tokens; kill %1; sleep 0.3
vcs-server --root $W/root --tokens $W/tokens --listen 127.0.0.1:19400 >$W/srv.log 2>&1 &
sleep 1
U=http://127.0.0.1:19400
mkdir a && cd a && vcs init >/dev/null && vcs config set --user user.name b && vcs config set --user user.email b@x.invalid; echo 0 > s; vcs add . && vcs commit -m init && vcs remote set $U && vcs publish; cd ..
for c in b c d; do vcs clone $U $c >/dev/null; done
for r in 1 2 3; do
  for c in b c d; do
    (cd $c; echo $c$r > f_$c$r; vcs add f_$c$r >/dev/null; vcs commit -m $c$r >/dev/null
     for t in 1 2 3 4 5; do vcs publish >/dev/null 2>&1 && break; vcs sync >/dev/null 2>&1; vcs merge --remote main || { echo "MERGE FAIL $c r$r t$t"; break; }; done)
  done
done
kill %1 2>/dev/null; pkill -f "listen 127.0.0.1:19400"
