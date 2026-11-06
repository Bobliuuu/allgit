#!/usr/bin/env python3
# ab.py N cwd "cmd args" binA binB [prep] [post]   interleaved, prints median/MAD wall + cpu per binary
import sys,os,subprocess,time,random,statistics as st,resource
n=int(sys.argv[1]); cwd=sys.argv[2]; args=sys.argv[3].split(); bins=sys.argv[4:6]
prep=sys.argv[6] if len(sys.argv)>6 else None; post=sys.argv[7] if len(sys.argv)>7 else None
res={b:([],[]) for b in bins}
def sh(c,b):
    if c: subprocess.run(c.replace("{B}",b),shell=True,cwd=cwd,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
for i in range(n+1):
    order=bins[:]; random.shuffle(order)
    for b in order:
        sh(prep,b)
        r0=resource.getrusage(resource.RUSAGE_CHILDREN); t=time.perf_counter()
        p=subprocess.run([b]+args,cwd=cwd,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        w=(time.perf_counter()-t)*1000; r1=resource.getrusage(resource.RUSAGE_CHILDREN)
        c=((r1.ru_utime-r0.ru_utime)+(r1.ru_stime-r0.ru_stime))*1000
        sh(post,b)
        if i>0: res[b][0].append(w); res[b][1].append(c)
def m(x): 
    md=st.median(x); return md, st.median([abs(v-md) for v in x])
out=[]
for b in bins:
    (w,wm),(c,cm)=m(res[b][0]),m(res[b][1]); out.append((w,wm,c,cm))
    print(f"{os.path.basename(b):10s} wall {w:8.1f} (MAD {wm:5.1f})  cpu {c:8.1f} (MAD {cm:5.1f})")
print(f"ratio B/A wall {out[1][0]/out[0][0]:.2f} cpu {out[1][2]/out[0][2]:.2f}  noise-band {'INSIDE' if abs(out[1][0]-out[0][0])<max(0.05*out[0][0],out[0][1]+out[1][1]) else 'outside'}")
