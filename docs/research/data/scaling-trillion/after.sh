cd /data/repo
while pgrep -f "run_e2e.sh wide:10000" >/dev/null; do sleep 20; done
mkdir -p /data/out2
export VCS_BIN=/data/target-new/release/vcs
OUT=/data/out2 scripts/scaling/run_e2e.sh deep:1000000 deep:5000000 >> /data/out2/driver.log 2>&1
OUT=/data/out scripts/scaling/slice.sh 1000000 /data/slice-work /data/out/slice.jsonl > /data/out/slice_1m.log 2>&1
OUT=/data/out scripts/scaling/slice.sh 10000000 /data/slice-work /data/out/slice.jsonl > /data/out/slice_10m.log 2>&1
B=/data/target-new/release/scalebench
$B idx 10000000000 /data/scaling-work/idx1e10 --packs 4 --lookups 20000 >> /data/out/pack_index_1e10.jsonl 2>> /data/out/pack_index_1e10.err
sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null
$B idx 10000000000 /data/scaling-work/idx1e10 --packs 4 --lookups 20000 >> /data/out/pack_index_1e10_cold.jsonl 2>> /data/out/pack_index_1e10.err
echo ALLDONE >> /data/out/after.done
