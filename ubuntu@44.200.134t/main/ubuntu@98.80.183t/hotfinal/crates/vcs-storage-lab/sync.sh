#!/bin/sh
# usage: sync.sh [command run in crates/vcs-storage-lab on the box]
rsync -az --delete --exclude .git --exclude target --exclude "run_*.sh" /Users/jhome/wt/s_index/ lambda-final:rt/s_index/ || exit 1
ssh lambda-final ". ~/rt/s_index/crates/vcs-storage-lab/labenv.sh; cd ~/rt/s_index/crates/vcs-storage-lab && $*"
