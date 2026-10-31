#!/bin/sh
# Reproduces the datasets in the research docs. Repos are cloned into $R beforehand (see docs for URLs/revisions).
R=${R:-$HOME/rt/lab_data/repos}; D=${D:-$HOME/rt/lab_data}
S=crates/vcs-compress-lab/run_lab.sh
for ds in "$@"; do
case $ds in
 ripgrep) $S ripgrep git:$R/ripgrep:20 ;;
 tokio) $S tokio git:$R/tokio:20 ;;
 django) $S django git:$R/django:20 ;;
 godot_demo) $S godot_demo git:$R/godot-demo-projects:10 ;;
 linux_fs) $S linux_fs git:$R/linux:12:fs ;;
 synth_assets) $S synth_assets dirrevs:$D/synth_assets ;;
 synth_sparse) $S synth_sparse dirrevs:$D/synth_sparse ;;
 synth_ml) $S synth_ml dirrevs:$D/synth_ml ;;
esac
done
echo ALLDONE
