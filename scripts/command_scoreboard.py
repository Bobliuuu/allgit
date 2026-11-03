#!/usr/bin/env python3
"""Per-command scoreboard driver: vcs vs git (and vcs vs Perforce) on a 100k flat tree.

  setup  --work DIR --vcs BIN --datagen BIN [--n 100000] [--p4 BIN --p4d BIN]   untimed: builds repos
  run    --work DIR --vcs BIN --group G [--runs N] --out FILE.json              timed, interleaved
  list                                                                          scenario ids and groups

Every scenario is run for each tool once per round, in random order (seeded), so all tools see the same
machine load. Wall and CPU (user+sys from wait4 rusage; for p4 also the p4d server's CPU delta) are
recorded per run with the exit code. Prep/post steps are untimed. A cell whose timed command exits non-zero
for any run is reported as FAILED by scripts/command_scoreboard_render.py, never given a ratio.
Run the timed part under `flock ~/bench.lock`. See docs/research/command-scoreboard.md.
"""
import argparse, json, os, random, shlex, shutil, subprocess, sys, time

BASE_ENV = dict(os.environ, GIT_AUTHOR_NAME="b", GIT_AUTHOR_EMAIL="b@x.invalid", GIT_COMMITTER_NAME="b",
                GIT_COMMITTER_EMAIL="b@x.invalid", GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null",
                VCS_AUTHOR_NAME="b", VCS_AUTHOR_EMAIL="b@x.invalid", VCS_NO_HOOKS="1")
NCLK = os.sysconf("SC_CLK_TCK")


def load1():
    return float(open("/proc/loadavg").read().split()[0])


def files(root):
    out = []
    for d, ds, names in os.walk(root):
        ds[:] = [x for x in ds if x not in (".git", ".vcs")]
        out += [os.path.relpath(os.path.join(d, f), root) for f in names]
    out.sort()
    return out


def mod_files(root, rels, tag):
    for i, p in enumerate(rels):
        with open(os.path.join(root, p), "wb") as fh:
            fh.write(f"modified {tag} {i}\n".encode() * 8)


def proc_cpu(pid):
    try:
        f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        return (int(f[11]) + int(f[12]) + int(f[13]) + int(f[14])) / NCLK * 1000  # own + reaped children (p4d forks per connection)
    except OSError:
        return 0.0


def timed(argv, cwd, env, srv_pid=None):
    s0 = proc_cpu(srv_pid) if srv_pid else 0
    t = time.perf_counter()
    p = subprocess.Popen(argv, cwd=cwd, env=dict(env, PWD=cwd), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    _, status, ru = os.wait4(p.pid, 0)
    wall = (time.perf_counter() - t) * 1000
    cpu = (ru.ru_utime + ru.ru_stime) * 1000
    if srv_pid:
        cpu += proc_cpu(srv_pid) - s0
    return wall, cpu, os.waitstatus_to_exitcode(status)


def sh(cmd, cwd, env):
    return subprocess.run(["bash", "-c", cmd], cwd=cwd, env=dict(env, PWD=cwd), stdout=subprocess.DEVNULL,
                          stderr=subprocess.DEVNULL).returncode


# ---------------------------------------------------------------- scenarios
# S(id, group, git, vcs, prep=(g, v), post=(g, v), reps=None)
# git / vcs: timed command (shlex). prep/post: bash snippets (untimed), "{B}" = tool binary, or a (g, v) tuple.
# Tokens in a snippet/command: {F} {F2} tracked files, {NEEDLE}. Python helpers as prep/post: "@mod1", "@mod1pct".
SC = []


def S(sid, grp, g, v, prep=None, post=None, reps=None, note=""):
    SC.append(dict(id=sid, grp=grp, g=g, v=v, prep=prep, post=post, reps=reps, note=note))


RESTORE_F = "{B} restore --staged {F}; {B} restore {F}"
UNDO = ("git reset -q --hard base", "{B} reset --hard base")

# group "fresh": destructive, own scratch repos
S("init", "fresh", "git init -q ../xinit", "{B} init ../xinit", prep="rm -rf ../xinit")
S("add_all", "fresh", "git add .", "{B} add .", prep=("rm -rf .git; git init -q -b main", "rm -rf .vcs; {B} init"), reps=3)
S("commit_all", "fresh", "git commit -q -m c", "{B} commit -m c",
  prep=("rm -rf .git; git init -q -b main; git add .", "rm -rf .vcs; {B} init; {B} add ."), reps=3)

# group "clean": read-only on the canonical clean tree (main checked out, 32 commits, branches side/base/sidebase, tag v1)
for sid, g, v in [
    ("status", "git status", "{B} status"),
    ("status_s", "git status -s", "{B} git status -s"),
    ("status_porcelain", "git status --porcelain", "{B} git status --porcelain"),
    ("diff_clean", "git diff", "{B} diff"),
    ("diff_staged_clean", "git diff --staged", "{B} diff --staged"),
    ("diff_rev", "git diff HEAD~1 HEAD", "{B} git diff HEAD~1 HEAD"),
    ("diff_branches", "git diff main..side", "{B} git diff main side"),
    ("diff_path", "git diff -- {F}", "{B} git diff -- {F}"),
    ("log5", "git log -n 5 --oneline", "{B} log -n 5 --oneline"),
    ("log_all", "git log --oneline", "{B} log -n 1000000000 --oneline"),
    ("log_graph", "git log --graph --oneline", "{B} log -n 1000000000 --oneline --graph"),
    ("log_author", "git log --author=b --since=2020-01-01 --oneline", "{B} log -n 1000000000 --author b --since 2020-01-01 --oneline"),
    ("log_grep", "git log --grep=c --oneline", "{B} log -n 1000000000 --grep c --oneline"),
    ("log_S", "git log -S{NEEDLE} --oneline", "{B} log -n 1000000000 -S {NEEDLE} --oneline"),
    ("log_follow", "git log --follow --oneline -- {F}", "{B} log -n 1000000000 --follow --oneline -- {F}"),
    ("log_namestatus", "git log --name-status --oneline", "{B} log -n 1000000000 --name-status --oneline"),
    ("log_format", "git log --format=%h", "{B} log -n 1000000000 --format=%h"),
    ("log_path", "git log main --oneline -- {F}", "{B} log -n 1000000000 main --oneline -- {F}"),
    ("log_range", "git log --oneline main..side", "{B} log -n 1000000000 --oneline main..side"),
    ("log_sym", "git log --oneline main...side", "{B} log -n 1000000000 --oneline main...side"),
    ("log_stat", "git log --stat --oneline -n 5", "{B} git log --stat --oneline -n 5"),
    ("revlist_count", "git rev-list --count main..side", "{B} rev-list --count main..side"),
    ("revlist_all", "git rev-list --all", "{B} rev-list --all"),
    ("shortlog", "git shortlog -sn", "{B} shortlog -s -n"),
    ("show", "git show", "{B} show"),
    ("show_stat", "git show --stat HEAD", "{B} show --stat HEAD"),
    ("show_file", "git show HEAD:{F}", "{B} cat-file -p HEAD:{F}"),
    ("catfile_p", "git cat-file -p HEAD:{F}", "{B} cat-file -p HEAD:{F}"),
    ("catfile_t", "git cat-file -t HEAD", "{B} cat-file -t HEAD"),
    ("revparse", "git rev-parse HEAD", "{B} rev-parse HEAD"),
    ("revparse_short", "git rev-parse --short HEAD", "{B} rev-parse --short HEAD"),
    ("revparse_top", "git rev-parse --show-toplevel", "{B} rev-parse --show-toplevel"),
    ("branch_list", "git branch", "{B} branch"),
    ("tag_list", "git tag", "{B} tag"),
    ("describe", "git describe --tags", "{B} describe --tags"),
    ("describe_always", "git describe --always", "{B} describe --always"),
    ("lsfiles", "git ls-files", "{B} ls-files"),
    ("lsfiles_s", "git ls-files -s", "{B} ls-files -s"),
    ("lstree", "git ls-tree -r --name-only HEAD", "{B} ls-files --rev HEAD"),
    ("lstree_flat", "git ls-tree HEAD", "{B} ls-files -s --rev HEAD --depth 1"),
    ("mergebase", "git merge-base main side", "{B} merge-base main side"),
    ("difftree", "git diff-tree -r main side", "{B} diff-tree main side"),
    ("blame", "git blame {F}", "{B} blame {F}"),
    ("blame_rev", "git blame HEAD~1 -- {F}", "{B} blame --rev HEAD~1 {F}"),
    ("grep", "git grep -n {NEEDLE}", "{B} grep -n {NEEDLE}"),
    ("grep_rev", "git grep -i -e {NEEDLE} HEAD", "{B} grep -i {NEEDLE} HEAD"),
    ("grep_path", "git grep -F {NEEDLE} -- {FD}", "{B} grep -F {NEEDLE} -- {FD}"),
    ("config_get", "git config user.name", "{B} config get user.name"),
    ("config_list", "git config --list", "{B} config list"),
    ("remote_v", "git remote -v", "{B} remote show"),
    ("reflog", "git reflog", "{B} op log"),
    ("reflog5", "git reflog -n 5", "{B} op log -n 5"),
    ("cherry", "git cherry main side", "{B} cherry main side"),
    ("worktree_list", "git worktree list", "{B} worktree list"),
    ("stash_list", "git stash list", "{B} stash list"),
    ("clean_n", "git clean -n", "{B} clean -n"),
    ("formatpatch", "git format-patch -1 --stdout", "{B} format-patch -n 1 --stdout"),
    ("archive", "git archive -o ../out/a.tar HEAD", "{B} archive -o ../out/a.tar HEAD"),
    ("bundle_create", "git bundle create ../out/f.bundle main", "{B} bundle create ../out/f.bundle main"),
    ("bundle_verify", "git bundle verify ../out/f.bundle", "{B} bundle verify ../out/f.bundle"),
    ("fsck", "git fsck", "{B} fsck"),
    ("fastexport", "git fast-export main", "{B} export-git"),
]:
    S(sid, "clean", g, v, reps=3 if sid in ("fsck", "archive", "bundle_create", "revlist_all", "fastexport") else None,
      prep=("mkdir -p ../out; rm -f ../out/a.tar" + ("; git bundle create ../out/f.bundle main >/dev/null 2>&1" if sid == "bundle_verify" else ""),
            "mkdir -p ../out; rm -f ../out/a.tar" + ("; {B} bundle create ../out/f.bundle main >/dev/null 2>&1" if sid == "bundle_verify" else "")))

# group "dirty": 1% of files modified (unstaged), restored by each scenario that changes them
S("status_dirty", "dirty", "git status", "{B} status")
S("status_dirty_s", "dirty", "git status -s", "{B} git status -s")
S("diff_dirty", "dirty", "git diff", "{B} diff")
S("diff_stat", "dirty", "git diff --stat", "{B} diff --stat")
S("diff_nameonly", "dirty", "git diff --name-only", "{B} diff --name-only")
S("diff_namestatus", "dirty", "git diff --name-status", "{B} diff --name-status")
S("diff_head", "dirty", "git diff HEAD", "{B} git diff HEAD")
S("add_u", "dirty", "git add -u", "{B} git add -u", post="{B} reset", reps=3)
S("commit_a", "dirty", "git commit -q -a -m x", "{B} git commit -am x", post=("git reset -q HEAD~1", "{B} reset --mixed HEAD~1"), reps=3)
S("stash_push", "dirty", "git stash push -q", "{B} stash push", post=("git stash drop -q; @mod1pct", "{B} stash drop; @mod1pct"), reps=3)
S("restore_all", "dirty", "git restore .", "{B} restore .", post="@mod1pct", reps=3)

# group "untracked"
S("clean_f", "untracked", "git clean -fdq", "{B} clean -f -d", post="@untracked", reps=3)

# group "small": small mutations; each prep/post returns the tree to clean main
S("add_file", "small", "git add {F}", "{B} add {F}", prep="@mod1", post=RESTORE_F)
S("add_dot_1", "small", "git add .", "{B} add .", prep="@mod1", post=RESTORE_F)
S("rm_file", "small", "git rm -q {F}", "{B} rm {F}", post=RESTORE_F)
S("rm_cached", "small", "git rm -q --cached {F}", "{B} rm --cached {F}", post="{B} restore --staged {F}")
S("mv_file", "small", "git mv {F} {F}.moved", "{B} mv {F} {F}.moved", post="{B} mv {F}.moved {F}")
S("commit_1", "small", "git commit -q -m x", "{B} commit -m x", prep="@mod1; {B} add {F}", post=UNDO)
S("commit_amend", "small", "git commit -q --amend -m y", "{B} git commit --amend -m y", prep="@mod1; {B} add {F}; {B} commit -m x", post=UNDO, reps=3)
S("restore_file", "small", "git restore {F}", "{B} restore {F}", prep="@mod1")
S("restore_staged", "small", "git restore --staged {F}", "{B} restore --staged {F}", prep="@mod1; {B} add {F}", post="{B} restore {F}")
S("reset_file", "small", "git reset HEAD -- {F}", "{B} reset HEAD -- {F}", prep="@mod1; {B} add {F}", post="{B} restore {F}")
S("branch_create", "small", "git branch feat", "{B} branch feat", post=("git branch -q -D feat", "{B} branch -d feat"))
S("branch_delete", "small", "git branch -q -d feat", "{B} branch -d feat", prep="{B} branch feat")
S("branch_rename", "small", "git branch -m feat feat2", "{B} branch -m feat feat2", prep="{B} branch feat", post=("git branch -q -D feat2", "{B} branch -d feat2"))
S("tag_create", "small", "git tag v2", "{B} tag v2", post="{B} tag -d v2")
S("tag_annot", "small", "git tag -a v2 -m rel", "{B} tag v2 -m rel", post="{B} tag -d v2")
S("tag_delete", "small", "git tag -d v2", "{B} tag -d v2", prep="{B} tag v2")
S("switch_side", "small", "git switch -q side", "{B} switch side", post="{B} switch main", reps=5, note="1% of files differ")
S("switch_main", "small", "git switch -q main", "{B} switch main", prep="{B} switch side", reps=5, note="1% of files differ")
S("switch_c", "small", "git switch -q -c topic", "{B} switch -c topic", post=("git switch -q main; git branch -q -D topic", "{B} switch main; {B} branch -d topic"))
S("switch_detach", "small", "git switch -q --detach HEAD~1", "{B} switch --detach HEAD~1", post="{B} switch main")
S("checkout_b", "small", "git checkout -q -b topic", "{B} git checkout -b topic", post=("git switch -q main; git branch -q -D topic", "{B} switch main; {B} branch -d topic"))
S("reset_soft", "small", "git reset -q --soft HEAD~1", "{B} reset --soft HEAD~1", post="{B} reset --soft base")
S("reset_mixed", "small", "git reset -q --mixed HEAD~1", "{B} reset --mixed HEAD~1", post="{B} reset --soft base")
S("reset_hard", "small", "git reset -q --hard HEAD~1", "{B} reset --hard HEAD~1", post=UNDO)
S("revert", "small", "git revert --no-edit HEAD", "{B} revert HEAD", post=UNDO, reps=3)
S("cherry_pick", "small", "git cherry-pick side", "{B} cherry-pick side", post=UNDO, reps=3)
S("merge", "small", "git merge -q --no-edit side", "{B} merge side", post=UNDO, reps=3, note="diverged, 3-way")
S("merge_noff", "small", "git merge -q --no-ff -m m side", "{B} merge --no-ff -m m side", post=UNDO, reps=3)
S("merge_squash", "small", "git merge -q --squash side", "{B} merge --squash side", post=UNDO, reps=3)
S("rebase", "small", "git rebase -q main", "{B} rebase main", prep="{B} switch side", post=("git reset -q --hard sidebase; git switch -q main", "{B} reset --hard sidebase; {B} switch main"), reps=3)
S("worktree_add", "small", "git worktree add -q ../wt side", "{B} worktree add ../wt side", prep="rm -rf ../wt", post=("git worktree remove --force ../wt", "{B} worktree remove ../wt"), reps=3)
S("gc", "small", "git gc -q", "{B} gc", reps=3)
S("repack", "small", "git repack -q -ad", "{B} repack", reps=3)

# group "remote": file remote; cumulative commits on main, no undo (last group)
S("clone", "remote", "git clone -q ../remote dst", "{B} clone ../remote dst", prep="rm -rf dst", reps=3)
S("clone_zone", "remote", "git clone -q ../remote dst", "{B} clone ../remote dst", prep="rm -rf dst", reps=1, note="not run: no equivalent scenario")
S("push", "remote", "git push -q origin main", "{B} publish", prep="@mod1; {B} add {F}; {B} commit -m x")
S("fetch", "remote", "git fetch -q origin", "{B} fetch", prep="@srcpush")
S("pull", "remote", "git pull -q --ff-only", "{B} sync", prep="@srcpush")
SC[:] = [s for s in SC if s["id"] != "clone_zone"]


def sid_map():
    return {s["id"]: s for s in SC}


# ---------------------------------------------------------------- environment
class Env:
    def __init__(self, a):
        self.a = a
        self.work = os.path.abspath(a.work)
        self.vcs = os.path.abspath(a.vcs)
        self.tools = {"git": "git", "vcs": self.vcs}
        self.state = json.load(open(os.path.join(self.work, "state.json"))) if os.path.exists(os.path.join(self.work, "state.json")) else {}

    def d(self, t, sub="repo"):
        return os.path.join(self.work, t, sub)

    def fmt(self, s, t):
        st = self.state
        return s.replace("{B}", "git" if t == "git" else shlex.quote(self.vcs)).replace("{F2}", st.get("F2", "")) \
                .replace("{FD}", os.path.dirname(st.get("F", "")) or ".").replace("{F}", st.get("F", "")).replace("{NEEDLE}", st.get("needle", ""))

    def pick(self, x, t):
        return x[0 if t == "git" else 1] if isinstance(x, tuple) else x

    def helpers(self, snippet, t, cwd):
        """Run a bash snippet with @helpers expanded."""
        for part in [p.strip() for p in snippet.split(";") if p.strip()]:
            if part == "@mod1":
                mod_files(cwd, [self.state["F"]], f"one{time.time_ns()}")
            elif part == "@mod1pct":
                mod_files(cwd, files_1pct(self.state), "pct")
            elif part == "@untracked":
                os.makedirs(os.path.join(cwd, "untr"), exist_ok=True)
                for i in range(1000):
                    open(os.path.join(cwd, "untr", f"u{i}.txt"), "w").write("u\n")
            elif part == "@srcpush":
                srcpush(self, t)
            else:
                sh(self.fmt(part, t), cwd, BASE_ENV)


def files_1pct(state):
    return state["pct"]


def srcpush(env, t):
    """Make one new commit in the source repo and publish it to the remote (untimed)."""
    cwd = env.d(t)
    mod_files(cwd, [env.state["F"]], f"src{time.time_ns()}")
    sh(env.fmt("{B} add {F}; {B} commit -m x; " + ("git push -q origin main" if t == "git" else "{B} publish"), t), cwd, BASE_ENV)


def setup(a):
    work = os.path.abspath(a.work)
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work)
    vcs = os.path.abspath(a.vcs)
    st = {}
    for t in ("git", "vcs"):
        r = os.path.join(work, t, "repo")
        os.makedirs(r)
        subprocess.run([a.datagen, "flat", str(a.n), r], check=True, stdout=subprocess.DEVNULL)
    fs = files(os.path.join(work, "git", "repo"))
    st["F"], st["F2"] = fs[len(fs) // 3], fs[len(fs) // 2]
    st["pct"] = fs[::100]
    txt = open(os.path.join(work, "git", "repo", st["F"])).read()
    st["needle"] = txt[40:56]
    st["n"] = a.n
    json.dump(st, open(os.path.join(work, "state.json"), "w"))
    env = Env(argparse.Namespace(work=a.work, vcs=a.vcs))
    for t in ("git", "vcs"):
        r = env.d(t)
        B = "git" if t == "git" else shlex.quote(vcs)
        init = "git init -q -b main" if t == "git" else f"{B} init -q 2>/dev/null || {B} init"
        sh(init, r, BASE_ENV)
        if t == "git":
            sh("git config user.name b; git config user.email b@x.invalid", r, BASE_ENV)
        if t == "vcs":
            sh(f"{B} config set user.name b; {B} config set user.email b@x.invalid", r, BASE_ENV)
        sh(f"{B} add . && {B} commit -q -m c0 || {B} commit -m c0", r, BASE_ENV)
        for k in range(1, 30):
            fi = fs[k * 997 % len(fs)]
            open(os.path.join(r, fi), "w").write(f"history {k}\n")
            sh(f"{B} add {shlex.quote(fi)}; {B} commit -q -m h{k} || {B} commit -m h{k}", r, BASE_ENV)
        # side: diverged from main by one commit that changes 1% of the files
        sh(f"{B} branch side main~1", r, BASE_ENV)
        sh(f"{B} switch -q side || {B} switch side", r, BASE_ENV)
        mod_files(r, st["pct"], "side")
        sh(f"{B} add . ; {B} commit -q -m side || {B} commit -m side", r, BASE_ENV)
        sh(f"{B} branch sidebase side; {B} switch -q main || {B} switch main; {B} branch base main; {B} tag v1 main~5", r, BASE_ENV)
        # remote
        rem = os.path.join(work, t, "remote")
        if t == "git":
            sh(f"git init -q --bare -b main {rem}; git remote add origin {rem}; git push -q origin main side", r, BASE_ENV)
        else:
            sh(f"{B} remote set {rem}; {B} publish", r, BASE_ENV)
    print("setup done", file=sys.stderr)


def run_clean_check(env, t):
    cwd = env.d(t)
    B = "git" if t == "git" else shlex.quote(env.vcs)
    sh(f"{B} reset -q --hard base 2>/dev/null || {B} reset --hard base; {B} switch -q main 2>/dev/null || true", cwd, BASE_ENV)


def run(a):
    env = Env(a)
    rng = random.Random(1)
    groups = a.group.split(",")
    res = {"meta": {"git": subprocess.run(["git", "--version"], capture_output=True, text=True).stdout.strip(),
                    "vcs": subprocess.run([env.vcs, "--version"], capture_output=True, text=True).stdout.strip(),
                    "n": env.state["n"], "layout": "flat", "host": os.uname().nodename, "cores": os.cpu_count(),
                    "load1_start": load1(), "start": time.strftime("%FT%TZ", time.gmtime()), "groups": groups},
           "cells": {}}
    if "remote" in groups:  # sync both repos to the remote (earlier groups and the p4 phase moved local main)
        sh("git fetch -q origin; git reset -q --hard origin/main; git clean -fdq", env.d("git"), BASE_ENV)
        rem = os.path.join(env.work, "vcs", "remote")
        shutil.rmtree(rem, ignore_errors=True)
        for t in ("git", "vcs"):
            shutil.rmtree(os.path.join(env.work, t, "dstrepo"), ignore_errors=True)
        sh(env.fmt("{B} merge --abort; {B} reset --hard; {B} clean -f -d; {B} remote set " + shlex.quote(rem) + "; {B} publish", "vcs"), env.d("vcs"), BASE_ENV)
    else:
        for t in ("git", "vcs"):
            run_clean_check(env, t)
    for s in SC:
        if s["grp"] not in groups or (a.only and s["id"] not in a.only.split(",")):
            continue
        reps = min(s["reps"] or a.runs, a.runs)
        rows = {"git": [], "vcs": []}
        loads = []
        if s["grp"] == "dirty" and s["id"] == "status_dirty":
            for t in ("git", "vcs"):
                mod_files(env.d(t), env.state["pct"], "pct")
        for _ in range(reps):
            order = ["git", "vcs"]
            rng.shuffle(order)
            l0 = load1()
            for t in order:
                cwd = env.d(t) if s["grp"] != "fresh" or s["id"] == "init" or True else env.d(t)
                if s["grp"] == "fresh" and s["id"] != "init":
                    cwd = env.d(t, "fresh")
                    os.makedirs(cwd, exist_ok=True)
                    if not os.path.exists(os.path.join(cwd, "d0000")):
                        subprocess.run(["bash", "-c", f"cp -a '{env.d(t)}'/d0* ."], cwd=cwd, check=True)
                if s["grp"] == "fresh" and s["id"] == "init":
                    cwd = env.d(t, "fresh")
                    os.makedirs(cwd, exist_ok=True)
                if s["grp"] == "remote" and s["id"] in ("fetch", "pull"):
                    dst = os.path.join(env.work, t, "dstrepo")
                    if not os.path.exists(dst):
                        sh(env.fmt("{B} clone remote dstrepo", t), os.path.join(env.work, t), BASE_ENV)
                    tcwd = dst
                else:
                    tcwd = cwd
                if s["prep"]:
                    env.helpers(env.pick(s["prep"], t), t, cwd)
                cmd = shlex.split(env.fmt(env.pick((s["g"], s["v"]), t), t))
                w, c, rc = timed(cmd, tcwd, BASE_ENV)
                rows[t].append((w, c, rc))
                if s["post"]:
                    env.helpers(env.pick(s["post"], t), t, cwd)
            loads.append((l0, load1()))
        res["cells"][s["id"]] = {"rows": rows, "load": loads, "grp": s["grp"], "cmds": {"git": s["g"], "vcs": s["v"]}}
        f = lambda t: sorted(r[1] for r in rows[t])[len(rows[t]) // 2]
        print(f"{s['id']}: git {f('git'):.0f}ms vcs {f('vcs'):.0f}ms cpu rc={[r[2] for r in rows['git']]}/{[r[2] for r in rows['vcs']]}", file=sys.stderr, flush=True)
        if s["grp"] in ("dirty", "untracked") and s["id"] in ("restore_all", "clean_f", "stash_push"):
            pass
        if a.out:
            json.dump(res, open(a.out, "w"), indent=1)
    if "dirty" in groups:
        for t in ("git", "vcs"):
            run_clean_check(env, t)
    res["meta"]["load1_end"] = load1()
    if a.out:
        json.dump(res, open(a.out, "w"), indent=1)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["setup", "run", "list"])
    ap.add_argument("--work", default="sbwork")
    ap.add_argument("--vcs")
    ap.add_argument("--datagen")
    ap.add_argument("--n", type=int, default=100000)
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--group", default="clean")
    ap.add_argument("--only")
    ap.add_argument("--out")
    a = ap.parse_args()
    if a.cmd == "list":
        for s in SC:
            print(s["id"], s["grp"])
    elif a.cmd == "setup":
        setup(a)
    else:
        run(a)


if __name__ == "__main__":
    main()
