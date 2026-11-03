"""Held-out task variants. Questions and ground truth were written BEFORE the round-2 vcs changes
(cross-zone log, zone changes, zone explain, compact output) were designed or tuned; see git history.
Oracle strategies for them are attached afterwards in oracles_holdout.py (live agents never use oracles).
"""
import fixture as F
import tasks as TK
from tasks import Task, idx, paths_of, zones_of_commit, trans_deps, trans_dependents, dependents, codeowners, commits_touching
from collections import Counter


def make_holdout():
    T = []

    def add(tid, cat, q, ans, kind):
        T.append(Task("h_" + tid, cat, q, ans, kind, None, None))

    for tid, path in [("own1", "crates/storage/src/cache.rs"), ("own2", "tools/gen/src/main.rs"), ("own3", "CODEOWNERS")]:
        add(tid, "ownership", f"Which zone owns the path `{path}`? Answer with the zone name, or `root` if no zone owns it.", F.zone_of(path), "str")
    add("deps_direct", "dependencies", "Which zones does zone `net` depend on directly? Answer with a list of zone names.", F.DEPS["net"], "set")
    add("build_api", "dependencies",
        "Which zones must be subscribed (checked out) to build zone `api`? Include `api` itself and everything it depends on, directly or indirectly.",
        sorted(trans_deps("api") | {"api"}), "set")
    add("dependents_trans", "dependencies", "If zone `core` changes, which zones are affected, directly or indirectly (all zones that depend on it)?",
        sorted(trans_dependents("core")), "set")
    add("dependents_direct", "dependencies", "Which zones depend directly on zone `util`?", sorted(dependents("util")), "set")
    add("owners", "ontology", "Who are the CODEOWNERS of zone `core`? Answer with a list of handles such as `@name`.", codeowners("core"), "set")
    cnt = Counter(F.COMMITS[i][0] for i in commits_touching("docs"))
    add("top_author", "history", "Which author made the most commits that touch zone `docs`? Answer with the author's full name.",
        F.AUTHORS[cnt.most_common(1)[0][0]], "str")
    add("commit_count", "history", "How many commits on main touch zone `api`? Answer with an integer.", len(commits_touching("api")), "int")
    s = "net+api: timeouts"
    add("commit_files", "changes", f"Which files did the commit with subject \"{s}\" change? Answer with repo-relative paths.", paths_of(idx(s)), "set")
    s = "api+core: shared error type"
    add("commit_zones", "changes", f"Which zones does the commit with subject \"{s}\" touch? Answer with zone names (use `root` for files owned by no zone).",
        zones_of_commit(idx(s)), "set")
    A, B = "storage: add page cache", "net+api: timeouts"
    want = sorted({p for i in range(idx(A) + 1, idx(B) + 1) for p in F.changes(i) if F.zone_of(p) == "net"})
    add("range_zone", "changes", f"Which files in zone `net` changed after the commit \"{A}\" up to and including the commit \"{B}\"? Answer with repo-relative paths.", want, "set")
    add("find_removed", "history", "Which commit removed the function `legacy_index`? Answer with the commit subject.", "storage: drop legacy index", "str")
    add("find_added", "history", "Which commit introduced the constant `TIMEOUT_MS`? Answer with the commit subject.", "net+api: timeouts", "str")
    P, Q = "api+core: shared error type", "core: ledger rounding fix"
    add("merge_zones", "merge", f"Change P is the commit \"{P}\" and change Q is the commit \"{Q}\". Which zones are touched by BOTH changes (the intersection of the zones each change touches; an empty list if none)? Answer with zone names.",
        sorted({F.zone_of(p) for p in F.changes(idx(P))} & {F.zone_of(p) for p in F.changes(idx(Q))}), "set")
    add("merge_files", "merge", f"Change P is the commit \"{P}\" and change Q is the commit \"{Q}\". Do they modify any same file, so that merging them could conflict? Answer `yes` or `no`.",
        "yes" if set(F.changes(idx(P))) & set(F.changes(idx(Q))) else "no", "str")
    for tid, path in [("impact1", "crates/core/src/lib.rs"), ("impact2", "crates/util/src/date.rs")]:
        z = F.zone_of(path)
        add(tid, "impact", f"If the file `{path}` changes, which zones could be affected? Include the zone that owns the file and every zone that depends on it, directly or indirectly.",
            sorted({z} | trans_dependents(z)), "set")
    return T
