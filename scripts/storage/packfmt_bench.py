# Usage: packfmt_bench.py <path-to-vcs> <repo-with-imported-history>. Copies the repo, repacks to each format (3 runs),
# then times fsck and log -p/--stat (5 runs, medians). Local, warm page cache. See docs/research/storage/delta-gate.md.
import subprocess,sys,time,statistics,shutil,os
V=sys.argv[1]; src=sys.argv[2]
def run(c,cwd):
    s=time.time(); r=subprocess.run([V]+c,cwd=cwd,capture_output=True,text=True); return time.time()-s,r
def du(d):
    return sum(os.path.getsize(os.path.join(a,f)) for a,_,fs in os.walk(os.path.join(d,".vcs/objects")) for f in fs)
for f in ("v2","v3","v4"):
    ws=[]
    for i in range(3):
        shutil.rmtree("gf",ignore_errors=True); subprocess.run(["cp","-R",src,"gf"])
        t,r=run(["repack","--format",f],"gf"); ws.append(t)
    row=[f,"repack %.2fs"%statistics.median(ws),"bytes %d"%du("gf")]
    for name,c in (("fsck",["fsck"]),("log-p",["log","-p","--oneline"]),("log-stat",["log","--stat","--oneline"])):
        ts=[]
        for i in range(5):
            t,r=run(c,"gf"); ts.append(t)
        row.append("%s %.3fs"%(name,statistics.median(ts)))
    print(*row)
