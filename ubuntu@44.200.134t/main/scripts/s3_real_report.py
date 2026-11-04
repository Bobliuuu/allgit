#!/usr/bin/env python3
"""Markdown tables from s3_real_bench.py JSON files: s3_real_report.py before.json after.json  (medians, [min-max])."""
import json, statistics as st, sys


def f(v, d=0):
    v = [x for x in (v or []) if x is not None]
    return "-" if not v else f"{st.median(v):.{d}f} [{min(v):.{d}f}-{max(v):.{d}f}]"


def pct(v, p):
    v = sorted(v)
    return v[min(len(v) - 1, int(len(v) * p))]


def short(n):
    return n.replace(" [git://, same box]", " (git://, same box)")


def main(files):
    ds = [json.load(open(x)) for x in files]
    for tag, d in zip(("before", "after"), ds):
        m = d["meta"]
        print(f"\n### {tag}: {m['files']} files ({m['bytes']/1048576:.0f} MiB), slice {m['slice_files']} files, {m['rounds']} rounds, {m['date']}, {m['vcs']}, {m['git']}\n")
        print(f"initial publish of everything to S3: {m['initial']['vcs_publish_s3_ms']:.0f} ms; initial git push to daemon: {m['initial']['git_push_ms']:.0f} ms\n")
        for sec, title in (("s1_initial", "S1 clone of the job slice, base history (5 commits)"), ("s1_after_pushes", "S1' same after the push run (longer history, many small packs)")):
            if sec in d:
                print(f"**{title}** (wall ms)\n\n| cell | wall ms | requests | bytes |\n|---|---|---|---|")
                for n, x in d[sec].items():
                    print(f"| {short(n)} | {f(x['wall_ms'])} | {f(x.get('requests'))} | {f(x.get('bytes'))} |")
                print()
        if "s2" in d:
            print("**S2 warm runner: fetch the new commit and check it out** (wall ms)\n\n| cell | wall ms | requests |\n|---|---|---|")
            for n, x in d["s2"].items():
                print(f"| {short(n)} | {f(x['wall_ms'])} | {f(x.get('requests'))} |")
            print("\n**S3 PR checks** (ms)\n\n| cell | diff names | status (20 modified) |\n|---|---|---|")
            for n, x in d["s3"].items():
                print(f"| {short(n)} | {f(x['diff_ms'])} | {f(x['status_ms'])} |")
            print()
        if "pushes" in d:
            p = d["pushes"]
            print(f"**Push of a 3-file commit, {len(p['vcs_publish_s3_ms'])} sequential pushes** (ms)\n\n| | p50 | p95 | p99 | max | first 10 p50 | last 100 p50 |\n|---|---|---|---|---|---|---|")
            for k, name in (("vcs_publish_s3_ms", "vcs publish to real S3"), ("git_push_ms", "git push to git:// daemon (same box)")):
                v = p[k]
                print(f"| {name} | {pct(v,.5):.0f} | {pct(v,.95):.0f} | {pct(v,.99):.0f} | {max(v):.0f} | {st.median(v[:10]):.0f} | {st.median(v[-100:]):.0f} |")
            print()


if __name__ == "__main__":
    main(sys.argv[1:])
