"""Task definitions: question text, ground truth (from fixture.SPEC), and scripted oracle strategies.

An oracle strategy is `fn(t, params)` where `t(argv)` runs one tool command and returns its output
text; it returns the answer value. Oracles are what a competent, efficient agent would do with each
tool; they are written once, by hand, and are NOT tuned per tool to win.
"""
import json
import re
from collections import Counter

import fixture as F

ZONE_NAMES = sorted(F.ZONES)


def idx(subj):
    return next(i for i in range(len(F.COMMITS)) if F.COMMITS[i][1] == subj)


def paths_of(i):
    return sorted(F.changes(i))


def zones_of_commit(i):
    return sorted({F.zone_of(p) for p in F.changes(i)} - {"root"} | ({"root"} if any(F.zone_of(p) == "root" for p in F.changes(i)) else set()))


def trans_deps(z, seen=None):
    seen = set() if seen is None else seen
    for d in F.DEPS.get(z, []):
        if d not in seen:
            seen.add(d)
            trans_deps(d, seen)
    return seen


def dependents(z):
    return {a for a, ds in F.DEPS.items() if z in ds}


def trans_dependents(z):
    out, todo = set(), [z]
    while todo:
        for d in dependents(todo.pop()):
            if d not in out:
                out.add(d)
                todo.append(d)
    return out


def codeowners(z):
    path = F.ZONES[z] + "/x"
    hit = []
    for pat, owners in F.CODEOWNERS:
        if path.startswith(pat.strip("/") + "/"):
            hit = owners
    return sorted(hit)


def commits_touching(z):
    return [i for i in range(len(F.COMMITS)) if any(F.zone_of(p) == z for p in F.changes(i))]


# ---------------------------------------------------------------- tool-output helpers

def gl(t, *a):
    return t(["git", *a])


def vj(t, *a):
    out = t(["vcs", *a, "--json"])
    return json.loads(out)


def git_dirs(t):
    """Directory list; zones = children of crates/ plus tools/gen and docs (per the task preamble)."""
    out = gl(t, "ls-tree", "-r", "-d", "--name-only", "HEAD")
    return [l for l in out.splitlines() if l]


def git_zones(t):
    dirs = git_dirs(t)
    zs = {}
    for d in dirs:
        if re.fullmatch(r"crates/[^/]+", d) or d in ("tools/gen", "docs"):
            zs[d.split("/")[-1] if d != "tools/gen" else "tools"] = d
    return zs


def git_owner(zs, path):
    for z, p in zs.items():
        if path == p or path.startswith(p + "/"):
            return z
    return "root"


def git_hash_by_subject(t, subj):
    for l in gl(t, "log", "--format=%H\t%s").splitlines():
        h, s = l.split("\t", 1)
        if s == subj:
            return h
    raise KeyError(subj)


def git_files(t, h):
    return sorted(l for l in gl(t, "show", "--name-only", "--format=", h).splitlines() if l)


def git_deps(t):
    """zone -> direct deps, from `path =` lines of every Cargo.toml (one git grep)."""
    out = gl(t, "grep", "-n", "path = ", "HEAD", "--", "*Cargo.toml")
    zs = git_zones(t)
    deps = {z: set() for z in zs}
    for l in out.splitlines():
        m = re.match(r"HEAD:([^:]+)/Cargo\.toml:\d+:(\w+) = \{ path", l)
        if m and m.group(2) in zs:
            deps[git_owner(zs, m.group(1) + "/Cargo.toml")].add(m.group(2))
    return deps


def vcs_all_commits(t, n=20):
    """[(subject, timestamp, {repo path})] merged over all zones' recent history (one describe call)."""
    d = memo(t, ("hist", n), lambda: vj(t, "zone", "describe", "--fields", "path,history", "--history", str(n), "--transactions", "0"))
    merged = {}
    for z in d["zones"]:
        for c in z["history"]["recent"]:
            e = merged.setdefault(c["subject"], [c["timestamp"], set()])
            for f in c["files"]:
                e[1].add((z["path"] + "/" + f["path"]) if z["path"] else f["path"])
    return merged


def memo(t, key, fn):
    """An agent remembers what it already read: one call per distinct need within an episode."""
    m = t.__dict__.setdefault("memo", {})
    if key not in m:
        m[key] = fn()
    return m[key]


def vcs_graph(t):
    return memo(t, "graph", lambda: vj(t, "zone", "graph"))


# ---------------------------------------------------------------- tasks

class Task:
    def __init__(self, tid, cat, q, answer, kind, git, vcs):
        self.id, self.cat, self.question, self.answer, self.kind, self.git, self.vcs = tid, cat, q, answer, kind, git, vcs


def norm(kind, v):
    if kind == "set":
        return sorted({str(x).strip().lower() for x in v})
    if kind == "int":
        return int(v)
    return str(v).strip().lower()


def grade(task, got):
    try:
        return norm(task.kind, got) == norm(task.kind, task.answer)
    except Exception:
        return False


def make_tasks():
    T = []

    def add(*a):
        T.append(Task(*a))

    # --- path ownership
    for tid, path in [("own1", "crates/net/src/retry.rs"), ("own2", "docs/guide/intro.md"), ("own3", "Makefile")]:
        def g(t, path=path):
            return git_owner(git_zones(t), path)

        def v(t, path=path):
            best = max((n for n in vcs_graph(t)["path_index"] if path == n["prefix"] or path.startswith(n["prefix"] + "/") or n["prefix"] == ""),
                       key=lambda n: len(n["prefix"]))
            return best["zone"]
        add(tid, "ownership", f"Which zone owns the path `{path}`? Answer with the zone name, or `root` if no zone owns it.",
            F.zone_of(path), "str", g, v)

    add("list", "ownership", "List the names of all zones, not counting `root`.", ZONE_NAMES, "set",
        lambda t: list(git_zones(t)),
        lambda t: [n["name"] for n in vcs_graph(t)["nodes"] if n["name"] != "root"])

    z = "storage"
    add("purpose", "ontology", f"Which zone's manifest description is \"{F.DESC[z]}\"? Answer with the zone name.", z, "str",
        lambda t: next(git_owner(git_zones(t), l.split(":")[1]) for l in gl(t, "grep", "-n", F.DESC[z], "HEAD", "--", "*Cargo.toml").splitlines()),
        lambda t: next(x["name"] for x in vj(t, "zone", "describe", "--fields", "purpose", "--history", "0", "--transactions", "0")["zones"]
                       if x["purpose"] and x["purpose"]["text"] == F.DESC[z]))

    # --- dependencies
    add("deps_direct", "dependencies", "Which zones does zone `api` depend on directly? Answer with a list of zone names.",
        F.DEPS["api"], "set", lambda t: list(git_deps(t)["api"]),
        lambda t: next(n["depends_on"] for n in vcs_graph(t)["nodes"] if n["name"] == "api"))

    def g_trans(t, z):
        d, out, todo = git_deps(t), set(), [z]
        while todo:
            for x in d.get(todo.pop(), ()):
                if x not in out:
                    out.add(x)
                    todo.append(x)
        return out
    add("build_cli", "dependencies",
        "Which zones must be subscribed (checked out) to build zone `cli`? Include `cli` itself and everything it depends on, directly or indirectly.",
        sorted(trans_deps("cli") | {"cli"}), "set", lambda t: g_trans(t, "cli") | {"cli"},
        lambda t: set(next(n["depends_on_transitive"] for n in vcs_graph(t)["nodes"] if n["name"] == "cli")) | {"cli"})
    add("build_net", "dependencies", "Which zones must be subscribed (checked out) to build zone `net`? Include `net` itself and everything it depends on.",
        sorted(trans_deps("net") | {"net"}), "set", lambda t: g_trans(t, "net") | {"net"},
        lambda t: set(next(n["depends_on_transitive"] for n in vcs_graph(t)["nodes"] if n["name"] == "net")) | {"net"})

    def g_dependents(t, z, trans):
        d = git_deps(t)
        direct = {a for a, ds in d.items() if z in ds}
        if not trans:
            return direct
        out, todo = set(), [z]
        while todo:
            x = todo.pop()
            for a, ds in d.items():
                if x in ds and a not in out:
                    out.add(a)
                    todo.append(a)
        return out
    add("dependents_direct", "dependencies", "Which zones depend directly on zone `core`?", sorted(dependents("core")), "set",
        lambda t: g_dependents(t, "core", False), lambda t: next(n["dependents"] for n in vcs_graph(t)["nodes"] if n["name"] == "core"))
    add("dependents_trans", "dependencies", "If zone `util` changes, which zones are affected, directly or indirectly (all zones that depend on it)?",
        sorted(trans_dependents("util")), "set", lambda t: g_dependents(t, "util", True),
        lambda t: next(n["dependents_transitive"] for n in vcs_graph(t)["nodes"] if n["name"] == "util"))
    leaves = sorted(set(F.ZONES) - {z for z in F.DEPS if F.DEPS[z]})  # zones with no dependencies at all
    add("no_deps", "dependencies", "Which zones (not `root`) depend on no other zone? Answer with a list of zone names.", leaves, "set",
        lambda t: {z for z, ds in git_deps(t).items() if not ds},
        lambda t: [n["name"] for n in vcs_graph(t)["nodes"] if n["name"] != "root" and not n["depends_on"]])

    # --- owners and activity
    add("owners", "ontology", "Who are the CODEOWNERS of zone `api`? Answer with a list of handles such as `@name`.", codeowners("api"), "set",
        lambda t: _git_owners(t, "api"),
        lambda t: vj(t, "zone", "describe", "api", "--fields", "owners", "--history", "0", "--transactions", "0")["zones"][0]["owners"]["codeowners"])
    cnt = Counter(F.COMMITS[i][0] for i in commits_touching("net"))
    top = F.AUTHORS[cnt.most_common(1)[0][0]]
    assert sorted(cnt.values())[-1] > (sorted(cnt.values())[-2] if len(cnt) > 1 else 0)
    add("top_author", "history", "Which author made the most commits that touch zone `net`? Answer with the author's full name.", top, "str",
        lambda t: Counter(gl(t, "log", "--format=%an", "--", "crates/net").splitlines()).most_common(1)[0][0],
        lambda t: vj(t, "zone", "describe", "net", "--fields", "owners", "--history", "0", "--transactions", "0")["zones"][0]["owners"]["top_authors"][0]["name"])
    add("commit_count", "history", "How many commits on main touch zone `storage`? Answer with an integer.", len(commits_touching("storage")), "int",
        lambda t: int(gl(t, "rev-list", "--count", "HEAD", "--", "crates/storage").strip()),
        lambda t: vj(t, "zone", "describe", "storage", "--fields", "history", "--history", "0", "--transactions", "0")["zones"][0]["history"]["count"]["commits"])

    # --- changes
    S_FILES, S_ZONES = "docs: update api reference", "util+tools: date helpers and generator"
    add("commit_files", "changes", f"Which files did the commit with subject \"{S_FILES}\" change? Answer with repo-relative paths.",
        paths_of(idx(S_FILES)), "set", lambda t: git_files(t, git_hash_by_subject(t, S_FILES)),
        lambda t: vcs_all_commits(t)[S_FILES][1])
    add("commit_zones", "changes", f"Which zones does the commit with subject \"{S_ZONES}\" touch? Answer with zone names (use `root` for files owned by no zone).",
        zones_of_commit(idx(S_ZONES)), "set",
        lambda t: {git_owner(git_zones(t), p) for p in git_files(t, git_hash_by_subject(t, S_ZONES))},
        lambda t: _vcs_commit_zones(t, S_ZONES))
    last = len(F.COMMITS) - 1
    add("latest_zones", "changes", "Which zones did the most recent commit on main touch? Answer with zone names.", zones_of_commit(last), "set",
        lambda t: {git_owner(git_zones(t), p) for p in gl(t, "show", "--name-only", "--format=", "HEAD").splitlines() if p},
        lambda t: vj(t, "zone", "describe", "--fields", "path", "--history", "0", "--transactions", "1")["transactions"][0]["zones_touched"])
    A, B = "net: add http client", "api+core: shared error type"
    lo, hi = idx(A), idx(B)
    want = sorted({p for i in range(lo + 1, hi + 1) for p in F.changes(i) if F.zone_of(p) == "api"})
    add("range_zone", "changes", f"Which files in zone `api` changed after the commit \"{A}\" up to and including the commit \"{B}\"? Answer with repo-relative paths.",
        want, "set", lambda t: [l for l in gl(t, "diff", "--name-only", git_hash_by_subject(t, A), git_hash_by_subject(t, B), "--", "crates/api").splitlines() if l],
        lambda t: _vcs_range(t, A, B, "api"))

    # --- find behavior
    add("find_func", "history", "Which commit introduced the function `retry_with_backoff`? Answer with the commit subject.", F.COMMITS[idx("net: add retry_with_backoff")][1], "str",
        lambda t: gl(t, "log", "-S", "retry_with_backoff", "--format=%s").splitlines()[-1],
        lambda t: _vcs_pickaxe(t, "retry_with_backoff"))
    add("find_const", "history", "Which commit changed `MAX_RETRIES` to 5? Answer with the commit subject.", "net: raise MAX_RETRIES to 5", "str",
        lambda t: gl(t, "log", "-S", "MAX_RETRIES: u32 = 5", "--format=%s").splitlines()[-1],
        lambda t: _vcs_pickaxe(t, "MAX_RETRIES: u32 = 5"))

    # --- safe merge
    for tid, P, Q in [("merge_a", "net+api: timeouts", "cli+api: version flag"), ("merge_b", "storage: fsync on commit", "core: ledger rounding fix")]:
        zp = {F.zone_of(p) for p in F.changes(idx(P))}
        zq = {F.zone_of(p) for p in F.changes(idx(Q))}
        common = sorted(zp & zq)
        add(tid + "_zones", "merge", f"Change P is the commit \"{P}\" and change Q is the commit \"{Q}\". Which zones are touched by BOTH changes (the intersection of the zones each change touches; an empty list if none)? Answer with zone names.",
            common, "set",
            lambda t, P=P, Q=Q: _git_common_zones(t, P, Q), lambda t, P=P, Q=Q: _vcs_common_zones(t, P, Q))
        same = sorted(set(F.changes(idx(P))) & set(F.changes(idx(Q))))
        add(tid + "_files", "merge", f"Change P is the commit \"{P}\" and change Q is the commit \"{Q}\". Do they modify any same file, so that merging them could conflict? Answer `yes` or `no`.",
            "yes" if same else "no", "str",
            lambda t, P=P, Q=Q: "yes" if set(git_files(t, git_hash_by_subject(t, P))) & set(git_files(t, git_hash_by_subject(t, Q))) else "no",
            lambda t, P=P, Q=Q: "yes" if vcs_all_commits(t)[P][1] & vcs_all_commits(t)[Q][1] else "no")
    return T


def _git_owners(t, z):
    zs = git_zones(t)
    rules = [l.split() for l in gl(t, "show", "HEAD:CODEOWNERS").splitlines() if l.strip()]
    hit = []
    for pat, *owners in rules:
        if (zs[z] + "/x").startswith(pat.strip("/") + "/"):
            hit = owners
    return hit


def _vcs_commit_zones(t, subj):
    d = memo(t, ("hist", 20), lambda: vj(t, "zone", "describe", "--fields", "path,history", "--history", "20", "--transactions", "0"))
    return {z["name"] for z in d["zones"] if any(c["subject"] == subj for c in z["history"]["recent"])}


def _git_common_zones(t, P, Q):
    zs = git_zones(t)
    f = lambda s: {git_owner(zs, p) for p in git_files(t, git_hash_by_subject(t, s))}
    return f(P) & f(Q)


def _vcs_common_zones(t, P, Q):
    return _vcs_commit_zones(t, P) & _vcs_commit_zones(t, Q)


def _vcs_range(t, A, B, zone):
    d = memo(t, ("hist", 20), lambda: vj(t, "zone", "describe", "--fields", "path,history", "--history", "20", "--transactions", "0"))
    times = {}
    for z in d["zones"]:
        for c in z["history"]["recent"]:
            times[c["subject"]] = c["timestamp"]
    lo, hi = times[A], times[B]
    out = set()
    for z in d["zones"]:
        if z["name"] == zone:
            for c in z["history"]["recent"]:
                if lo < c["timestamp"] <= hi:
                    out |= {z["path"] + "/" + f["path"] for f in c["files"]}
    return out


def _vcs_pickaxe(t, needle):
    for n in vcs_graph(t)["nodes"]:
        out = t(["vcs", "log", "--zone", n["id"], "-S", needle, "--format", "%s"]).strip().splitlines()
        out = [l for l in out if not l.startswith("no matching")]
        if out:
            return out[-1]
    return ""
