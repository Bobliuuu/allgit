cd /data/repo
while [ ! -f /data/out/after2.done ]; do sleep 30; done
rm -rf /data/scaling-work/idx1e11
B=/data/target-new/release/scalebench
$B cgraph 1000000000 /data/scaling-work/cg9 >> /data/out/commit_graph.jsonl 2>> /data/out/cg9.err
rm -rf /data/scaling-work/cg9
export VCS_BIN=/data/target-new/release/vcs
python3 scripts/scaling/e2e.py wide 100000000 /data/e2e-work/wide1e8 /data/out2/e2e.jsonl --no-git --timeout 14400 > /data/out2/e2e_wide100000000.log 2>&1
echo DONE3 >> /data/out/after3.done
