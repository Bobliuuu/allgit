#!/usr/bin/env bash
# Scenario 1: big tree. usage: huge_s1.sh <srcdir> <label>
source ~/data/huge_scale_lib.sh
SRC=$1 L=$2; W=$HOME/w/s1_$L; mkdir -p $W
$VCS config set --user user.name b >/dev/null; $VCS config set --user user.email b@x.invalid >/dev/null
modify() { # touch 1% of files (every 100th) with appended line
  find "$1" -name 'f*.txt' -path '*0/*' -print 2>/dev/null | head -0; 
  python3 - "$1" <<'PY'
import os,sys
r=sys.argv[1]; k=0
for dp,dn,fn in os.walk(r):
    if '.git' in dp or '.vcs' in dp: dn[:]=[]; continue
    for f in sorted(fn):
        k+=1
        if k%100==0:
            with open(os.path.join(dp,f),'a') as h: h.write('changed\n')
PY
}
for T in git vcs; do
  D=$W/$T; rm -rf $D; cp -a $SRC $D; cd $D
  if [ $T = git ]; then
    m s1-$L git init git init -q
    m s1-$L git add git add -A
    m s1-$L git commit git commit -qm one
    m s1-$L git status-clean git status --porcelain
    modify $D
    m s1-$L git status-1pct git status --porcelain
    m s1-$L git diff git diff --stat
    git add -A; git commit -qm two
    m s1-$L git checkout-1pct git checkout -q HEAD~1
    m s1-$L git checkout-back git checkout -q master 
    git config gc.autoDetach false
    m s1-$L git clone git clone -q --no-hardlinks $D $W/git_clone
    rm -rf $W/git_clone
    m s1-$L git gc git gc -q
    m s1-$L git add-noop git add -A
    m s1-$L git du-store du -sk .git
  else
    m s1-$L vcs init $VCS init -q
    m s1-$L vcs add $VCS add .
    m s1-$L vcs commit $VCS commit -m one
    m s1-$L vcs status-clean $VCS status
    modify $D
    m s1-$L vcs status-1pct $VCS status
    m s1-$L vcs diff $VCS diff
    $VCS add . >/dev/null; $VCS commit -m two >/dev/null
    B=$($VCS branch 2>/dev/null | head -1 | tr -d '* ')
    m s1-$L vcs checkout-1pct $VCS switch --detach HEAD~1
    m s1-$L vcs checkout-back $VCS switch main
    mkdir -p $W/vcs_remote; $VCS remote set $W/vcs_remote >/dev/null
    m s1-$L vcs publish $VCS publish
    m s1-$L vcs clone $VCS clone $W/vcs_remote $W/vcs_clone
    rm -rf $W/vcs_clone
    m s1-$L vcs gc $VCS gc --yes
    m s1-$L vcs add-noop $VCS add .
    m s1-$L vcs du-store du -sk .vcs
  fi
  cd $HOME; df -h / | tail -1
done
