cd /data/repo
while [ ! -f /data/out/after.done ]; do sleep 30; done
. ~/.cargo/env
CARGO_TARGET_DIR=/data/target-new cargo build --release -p vcs-cli -p vcs-bench --features sharded > /data/build-new.log 2>&1
export VCS_BIN=/data/target-new/release/vcs
mkdir -p /data/out3
OUT=/data/out3 scripts/scaling/run_e2e.sh deep:1000000 >> /data/out3/driver.log 2>&1
python3 scripts/scaling/e2e.py wide 30000000 /data/e2e-work/wide3e7 /data/out2/e2e.jsonl --no-git --timeout 7200 > /data/out2/e2e_wide30000000.log 2>&1
OUT=/data/out2 scripts/scaling/run_e2e.sh deep:10000000 >> /data/out2/driver.log 2>&1
B=/data/target-new/release/scalebench
rm -rf /data/scaling-work/idx1e10
$B idx 100000000000 /data/scaling-work/idx1e11 --packs 40 --lookups 20000 >> /data/out/pack_index_1e11.jsonl 2>> /data/out/pack_index_1e11.err
rm -rf /data/scaling-work/idx1e11
$B cgraph 1000000000 /data/scaling-work/cg9 >> /data/out/commit_graph.jsonl 2>> /data/out/cg9.err
rm -rf /data/scaling-work/cg9
python3 scripts/scaling/e2e.py wide 100000000 /data/e2e-work/wide1e8 /data/out2/e2e.jsonl --no-git --timeout 14400 > /data/out2/e2e_wide100000000.log 2>&1
echo DONE2 >> /data/out/after2.done
