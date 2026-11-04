#!/usr/bin/env python3
"""CI-style benchmark against REAL S3 (issue #187/#313): vcs `s3://` remote (client talks to S3 directly, no server)
vs git partial clone + sparse checkout served by `git daemon` on the SAME box (git has no native S3; this is the baseline).

usage (box in the bucket's region, creds in env, release binaries on PATH via CI_REL):
  S3_URL='s3://BUCKET/real-s3/RUN?endpoint=https://s3.us-east-1.amazonaws.com&region=us-east-1' \
  scripts/s3_real_bench.py <flat|deep> <n_files> <rounds> <out.json> [--pushes 1000]
Reuses fixtures and the s1 (cold slice clone) and s2/s3 (warm fetch+checkout, PR diff/status) cells of ci_bench.py.
Adds: cold (VCS_NO_CACHE=1) vs warm (persistent client cache) clones, push latency (3-file commit) and N sequential pushes.
Credentials are read only from the environment (AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY / AWS_SESSION_TOKEN); never logged.
"""
import json, os, shutil, statistics, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ci_bench as C
from ci_bench import sh, timed, du_kib, stats, P

S3_URL = os.environ["S3_URL"]


class S3Bench(C.Bench):
    def build(self):
        w = self.w
        sh(f"rm -rf '{w}' && mkdir -p '{w}/clones' '{w}/gitrepo' '{w}/cfg'")
        sh(f"datagen {self.layout} {self.n} '{w}/raw' 42")
        os.makedirs(f"{w}/ds")
        C.regroup(f"{w}/raw", f"{w}/ds", 50)
        shutil.rmtree(f"{w}/raw")
        self.files = sh(f"cd '{w}/ds' && find . -type f | sort | sed 's|^./||'").stdout.split("\n")[:-1]
        self.in_slice = [f for f in self.files if f.startswith(self.SLICE + "/")]
        self.bare = f"{w}/gitrepo/main.git"
        sh(f"git init -q --bare -b main '{self.bare}' && git -C '{self.bare}' config uploadpack.allowFilter true && git -C '{self.bare}' config uploadpack.allowAnySHA1InWant true && git -C '{self.bare}' config http.receivepack true")
        P.start("gitd", ["git", "daemon", "--reuseaddr", "--listen=127.0.0.1", f"--port={C.P_GD}", f"--base-path={w}/gitrepo", "--export-all", "--enable=receive-pack", "--max-connections=128", f"{w}/gitrepo"], C.P_GD)
        P.start("gitd-count", [sys.executable, f"{C.HERE}/ci_tcpcount.py", str(C.P_GDC), "127.0.0.1", str(C.P_GD)], C.P_GDC)
        self.u = {"git": f"git://127.0.0.1:{C.P_GD}/main.git", "gitc": f"git://127.0.0.1:{C.P_GDC}/main.git", "file": f"file://{self.bare}", "vcs": S3_URL}
        g = f"{w}/dev_git"
        sh(f"cp -a '{w}/ds' '{g}'")
        sh("git init -q -b main && git add -A && git commit -q -m initial", cwd=g)
        v = f"{w}/dev_vcs"
        sh(f"cp -a '{w}/ds' '{v}'")
        sh("vcs init", cwd=v)
        for gi in range(50):
            sh(f"vcs zone create g{gi:02d} --path g{gi:02d}", cwd=v)
        sh("vcs add . && vcs commit -m initial", cwd=v)
        sh(f"vcs remote set '{S3_URL}'", cwd=v)
        self.dev_git, self.dev_vcs = g, v
        for i in range(1, 6):
            self.commit(i)
        self.initial = {}
        t = time.perf_counter(); sh("vcs publish", cwd=v); self.initial["vcs_publish_s3_ms"] = (time.perf_counter() - t) * 1000
        t = time.perf_counter(); sh(f"git push -q '{self.u['git']}' main", cwd=g); self.initial["git_push_ms"] = (time.perf_counter() - t) * 1000
        sh(f"git -C '{self.bare}' -c gc.auto=0 repack -adbq")
        self.base_git, self.base_vcs = self.heads()

    def clone_cells(self):
        s = self.SLICE
        sp = f" && git -C {{dst}} sparse-checkout set {s}"
        G = lambda name, url, flags, post="": dict(name=name, kind="git", url=url, cmd=f"git clone -q {flags} '{{url}}' '{{dst}}'{post}", svp=["gitd"] if url == "git" else [])
        V = f"vcs clone '{{url}}' '{{dst}}' --stats --for {s}"
        return [
            dict(name="vcs --for (slice) [S3, cold: no client cache]", kind="vcs", url="vcs", cmd="VCS_NO_CACHE=1 " + V, svp=[], maxk=0),
            dict(name="vcs --for (slice) [S3, warm client cache]", kind="vcs", url="vcs", cmd=V, svp=[], maxk=0),
            dict(name="vcs full [S3, cold]", kind="vcs", url="vcs", cmd="VCS_NO_CACHE=1 vcs clone '{url}' '{dst}' --stats", svp=[], maxk=0),
            G("git depth1 blobless sparse [git://, same box]", "git", "--depth 1 --filter=blob:none --sparse", sp),
            G("git blobless sparse [git://, same box]", "git", "--filter=blob:none --sparse", sp),
            G("git depth1 sparse [git://, same box]", "git", "--depth 1 --sparse", sp),
            G("git full [git://, same box]", "git", ""),
        ]

    WARM = ["vcs --for (slice) [S3, warm client cache]", "git depth1 blobless sparse [git://, same box]", "git blobless sparse [git://, same box]", "git depth1 sparse [git://, same box]"]

    def counts(self, c, dst):
        return None

    def pushes(self, n):
        """untimed commit, timed push; vcs publish to S3 vs git push to the daemon. Returns per-push ms lists."""
        out = {"vcs_publish_s3_ms": [], "git_push_ms": []}
        for i in range(n):
            self.ctr += 1
            self.commit(1000 + self.ctr, 3)
            r = timed("vcs publish", self.dev_vcs)
            out["vcs_publish_s3_ms"].append(r["wall_ms"])
            out["git_push_ms"].append(timed(f"git push -q '{self.u['git']}' main", self.dev_git)["wall_ms"])
            if (i + 1) % 100 == 0:
                print(f"  push {i+1}/{n}: vcs p50 {statistics.median(out['vcs_publish_s3_ms'][-100:]):.0f} ms, git p50 {statistics.median(out['git_push_ms'][-100:]):.0f} ms", flush=True)
        return out

    def s1_push(self):
        # warm runner at base, then one fetch+switch after n pushes (catch-up) cold vs warm
        pass


def main(argv):
    layout, n, rounds, out = argv[0], int(argv[1]), int(argv[2]), argv[3]
    npush = int(argv[argv.index("--pushes") + 1]) if "--pushes" in argv else 0
    os.makedirs(C.WB, exist_ok=True)
    b = S3Bench(layout, n, rounds, [], 0)
    meta = dict(layout=layout, n=n, rounds=rounds, s3_prefix=S3_URL.split("?")[0].split("/", 3)[-1], date=time.strftime("%FT%TZ", time.gmtime()),
                git=sh("git --version").stdout.strip(), vcs=sh("vcs --version").stdout.strip(), cores=os.cpu_count())
    try:
        b.build()
        meta.update(files=len(b.files), slice_files=len(b.in_slice), bytes=sum(os.path.getsize(f'{b.w}/ds/{f}') for f in b.files), initial=b.initial)
        print("fixtures ready", b.initial, flush=True)
        ck = lambda: json.dump(dict(meta=meta, **b.res), open(out, "w"), indent=1)
        b.s1(); ck()
        b.s2_s3(); ck()
        if npush:
            b.res["pushes"] = b.pushes(npush); ck()
            # catch-up after the pushes: cold full-slice clone of the new head vs git blobless sparse
            b.res["s1_initial"] = b.res.pop("s1"); b.s1(); b.res["s1_after_pushes"] = b.res.pop("s1"); ck()
        print("wrote", out)
    finally:
        P.stop_all()
        shutil.rmtree(b.w, ignore_errors=True)


if __name__ == "__main__":
    main(sys.argv[1:])
