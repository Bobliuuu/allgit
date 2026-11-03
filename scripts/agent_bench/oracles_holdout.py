"""Oracle strategies for the held-out tasks (attached after the round-2 vcs changes; the questions and
answers in tasks_holdout.py predate them)."""
import tasks as TK
from tasks import gl, vj, git_zones, git_owner, git_deps, git_files, git_hash_by_subject, vcs_graph, vcs_all_commits, memo
import tasks_holdout as H
from collections import Counter


def _closure(d, z, rev=False):
    adj = d if not rev else {}
    if rev:
        for a, ds in d.items():
            for x in ds:
                adj.setdefault(x, set()).add(a)
    out, todo = set(), [z]
    while todo:
        for y in adj.get(todo.pop(), ()):
            if y not in out:
                out.add(y)
                todo.append(y)
    return out


def _node(t, name):
    return next(n for n in vcs_graph(t)["nodes"] if n["name"] == name)


def _explain(t, path):
    return vj(t, "zone", "explain", path)


def _vcs_zones_of(t, subj):
    d = memo(t, ("hist", 20), lambda: vj(t, "zone", "describe", "--fields", "path,history", "--history", "20", "--transactions", "0"))
    return {z["name"] for z in d["zones"] if any(c["subject"] == subj for c in z["history"]["recent"])}


def _git_zones_of(t, subj):
    zs = git_zones(t)
    return {git_owner(zs, p) for p in git_files(t, git_hash_by_subject(t, subj))}


def _pickaxe_vcs(t, needle, newest):
    out = [l for l in t(["vcs", "log", "--all-zones", "-S", needle, "--format", "%s"]).strip().splitlines() if not l.startswith("no matching")]
    return out[0 if newest else -1]


def _pickaxe_git(t, needle, newest):
    out = gl(t, "log", "-S", needle, "--format=%s").splitlines()
    return out[0 if newest else -1]


def attach(tasks):
    A, B = "storage: add page cache", "net+api: timeouts"
    P, Q = "api+core: shared error type", "core: ledger rounding fix"
    G, V = {}, {}
    for i in (1, 2, 3):
        path = next(x.question.split("`")[1] for x in tasks if x.id == f"h_own{i}")
        G[f"h_own{i}"] = lambda t, path=path: git_owner(git_zones(t), path)
        V[f"h_own{i}"] = lambda t, path=path: _explain(t, path)["zone"]
    G["h_deps_direct"] = lambda t: git_deps(t)["net"]
    V["h_deps_direct"] = lambda t: _node(t, "net").get("depends_on", [])
    G["h_build_api"] = lambda t: _closure(git_deps(t), "api") | {"api"}
    V["h_build_api"] = lambda t: set(_node(t, "api").get("depends_on_transitive", [])) | {"api"}
    G["h_dependents_trans"] = lambda t: _closure(git_deps(t), "core", rev=True)
    V["h_dependents_trans"] = lambda t: _node(t, "core").get("dependents_transitive", [])
    G["h_dependents_direct"] = lambda t: {a for a, ds in git_deps(t).items() if "util" in ds}
    V["h_dependents_direct"] = lambda t: _node(t, "util").get("dependents", [])
    G["h_owners"] = lambda t: TK._git_owners(t, "core")
    V["h_owners"] = lambda t: vj(t, "zone", "describe", "core", "--fields", "owners", "--history", "0", "--transactions", "0")["zones"][0]["owners"].get("codeowners", [])
    G["h_top_author"] = lambda t: Counter(gl(t, "log", "--format=%an", "--", "docs").splitlines()).most_common(1)[0][0]
    V["h_top_author"] = lambda t: vj(t, "zone", "describe", "docs", "--fields", "owners", "--history", "0", "--transactions", "0")["zones"][0]["owners"]["top_authors"][0]["name"]
    G["h_commit_count"] = lambda t: int(gl(t, "rev-list", "--count", "HEAD", "--", "crates/api").strip())
    V["h_commit_count"] = lambda t: vj(t, "zone", "describe", "api", "--fields", "history", "--history", "0", "--transactions", "0")["zones"][0]["history"]["count"]["commits"]
    G["h_commit_files"] = lambda t: git_files(t, git_hash_by_subject(t, "net+api: timeouts"))
    V["h_commit_files"] = lambda t: vcs_all_commits(t)["net+api: timeouts"][1]
    G["h_commit_zones"] = lambda t: _git_zones_of(t, P)
    V["h_commit_zones"] = lambda t: _vcs_zones_of(t, P)
    G["h_range_zone"] = lambda t: [l for l in gl(t, "diff", "--name-only", git_hash_by_subject(t, A), git_hash_by_subject(t, B), "--", "crates/net").splitlines() if l]
    V["h_range_zone"] = lambda t: TK._vcs_range(t, A, B, "net")
    G["h_find_removed"] = lambda t: _pickaxe_git(t, "legacy_index", True)
    V["h_find_removed"] = lambda t: _pickaxe_vcs(t, "legacy_index", True)
    G["h_find_added"] = lambda t: _pickaxe_git(t, "TIMEOUT_MS", False)
    V["h_find_added"] = lambda t: _pickaxe_vcs(t, "TIMEOUT_MS", False)
    G["h_merge_zones"] = lambda t: _git_zones_of(t, P) & _git_zones_of(t, Q)
    V["h_merge_zones"] = lambda t: _vcs_zones_of(t, P) & _vcs_zones_of(t, Q)
    G["h_merge_files"] = lambda t: "yes" if set(git_files(t, git_hash_by_subject(t, P))) & set(git_files(t, git_hash_by_subject(t, Q))) else "no"
    V["h_merge_files"] = lambda t: "yes" if vcs_all_commits(t)[P][1] & vcs_all_commits(t)[Q][1] else "no"
    for i, path in ((1, "crates/core/src/lib.rs"), (2, "crates/util/src/date.rs")):
        z = TK.F.zone_of(path)
        G[f"h_impact{i}"] = lambda t, z=z, path=path: {git_owner(git_zones(t), path)} | _closure(git_deps(t), z, rev=True)
        V[f"h_impact{i}"] = lambda t, path=path: _explain(t, path)["impacted"]
    for x in tasks:
        x.git, x.vcs = G[x.id], V[x.id]
    return tasks


def make():
    return attach(H.make_holdout())
