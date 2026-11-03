import sys
kind, n = sys.argv[1], int(sys.argv[2])
o = sys.stdout.buffer
def w(s): o.write(s.encode() if isinstance(s,str) else s)
if kind == "deep":   # n linear commits, each touches 1 file, plus n//10 tags
    ts = 1700000000
    mark = 0
    for i in range(1, n+1):
        data = f"v{i}\n"
        w(f"blob\nmark :{2*i}\ndata {len(data)}\n{data}\n")
        msg = f"c{i}"
        w(f"commit refs/heads/main\nmark :{2*i+1}\nauthor A <a@x> {ts+i} +0000\ncommitter A <a@x> {ts+i} +0000\ndata {len(msg)}\n{msg}\n")
        if i > 1: w(f"from :{2*(i-1)+1}\n")
        w(f"M 100644 :{2*i} d{i%100}/f{i%1000}.txt\n\n")
    for t in range(0, n//10):
        w(f"reset refs/tags/t{t}\nfrom :{2*(t*10+1)+1}\n\n")
elif kind == "bigdir":  # one commit with n files in one dir
    ts = 1700000000
    for i in range(n):
        data = f"f{i}\n"
        w(f"blob\nmark :{i+1}\ndata {len(data)}\n{data}\n")
    w(f"commit refs/heads/main\nmark :{n+1}\nauthor A <a@x> {ts} +0000\ncommitter A <a@x> {ts} +0000\ndata 3\nbig\n")
    for i in range(n): w(f"M 100644 :{i+1} big/f{i}.txt\n")
    w("\n")
elif kind == "wide":  # one commit with n files over n/100 dirs (tree-like)
    ts = 1700000000
    for i in range(n):
        data = f"f{i}\n"
        w(f"blob\nmark :{i+1}\ndata {len(data)}\n{data}\n")
    w(f"commit refs/heads/main\nmark :{n+1}\nauthor A <a@x> {ts} +0000\ncommitter A <a@x> {ts} +0000\ndata 4\nwide\n")
    for i in range(n): w(f"M 100644 :{i+1} a{i//1000}/b{(i//10)%100}/f{i}.txt\n")
    w("\n")
elif kind == "tags":  # one commit, n lightweight tags
    w("blob\nmark :1\ndata 2\nx\n\ncommit refs/heads/main\nmark :2\nauthor A <a@x> 1700000000 +0000\ncommitter A <a@x> 1700000000 +0000\ndata 1\nc\nM 100644 :1 f.txt\n\n")
    for t in range(n): w(f"reset refs/tags/t{t}\nfrom :2\n\n")
