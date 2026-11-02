#!/usr/bin/env python3
"""Render docs/research/command-scoreboard.md and docs/img/scoreboard-{git,p4,walgit}.svg.

  python3 -I scripts/command_scoreboard_render.py [--data docs/research/data/command-scoreboard]
         [--out-md docs/research/command-scoreboard.md] [--img docs/img]

Rows come from the command tables in docs/git-to-vcs.md and docs/p4-to-vcs.md (every row, none dropped), so a
command that vcs does not support is listed as MISSING, and one with no benchmark scenario as NOT-MEASURED
with the reason. Timings come from the JSON written by command_scoreboard.py / command_scoreboard_p4.py and, for
walgit, from docs/research/data/walgit-full-boxc/res-flat-100000.json (reused, see the report). No number is
produced here that is not in those files.
"""
import argparse, glob, html, json, os, re, statistics

TIE = 0.10      # |ratio - 1| <= 10% on BOTH wall and CPU (medians of per-round pairs) is a TIE
ABS_MS = 2.0    # and any wall difference under 2 ms is a TIE (process start-up noise)

# ---- git command text (as in docs/git-to-vcs.md) -> scenario id
GIT = {
    "git init": "init", "git init proj": "init",
    "git clone ssh://host/app.git proj": "clone", "git clone /srv/app.git": "clone",
    "git add f.txt": "add_file", "git add .": "add_all", "git add -A": "add_all", "git add -u": "add_u",
    "git rm f.txt": "rm_file", "git rm --cached f.txt": "rm_cached", "git mv a.txt b.txt": "mv_file",
    "git status": "status", "git status -s": "status_s", "git status --porcelain": "status_porcelain",
    "git diff": "diff_dirty", "git diff --staged": "diff_staged_clean", "git diff --cached": "diff_staged_clean",
    "git diff --stat": "diff_stat", "git diff --name-only": "diff_nameonly", "git diff --name-status": "diff_namestatus",
    "git diff HEAD~1 HEAD": "diff_rev", "git diff main..feature": "diff_branches", "git diff HEAD": "diff_head",
    "git diff -- f.txt": "diff_path",
    "git commit -m \"msg\"": "commit_1", "git commit -a -m \"msg\"": "commit_a",
    "git commit --amend -m \"msg\"": "commit_amend", "git commit --amend --no-edit": "commit_amend",
    "git log": "log_all", "git log -n 5 --oneline": "log5", "git log -5": "log5",
    "git log --graph --oneline": "log_graph", "git log --author=Dev --since=2024-01-01": "log_author",
    "git log --grep=fix": "log_grep", "git log -S needle": "log_S", "git log --follow -- f.txt": "log_follow",
    "git log --name-status": "log_namestatus", "git log --format=%h": "log_format", "git log main -- f.txt": "log_path",
    "git log --stat": "log_stat", "git log main..feature": "log_range", "git log main...feature": "log_sym",
    "git log ^main feature": "log_range", "git log feature --not main": "log_range",
    "git rev-list --count main..feature": "revlist_count", "git rev-list --all": "revlist_all",
    "git shortlog -sn": "shortlog",
    "git show": "show", "git show HEAD~1": "show", "git show --stat HEAD": "show_stat", "git show HEAD:f.txt": "show_file",
    "git branch": "branch_list", "git branch feature": "branch_create", "git branch -d feature": "branch_delete",
    "git branch -m main trunk": "branch_rename",
    "git switch main": "switch_main", "git switch -c feature": "switch_c", "git switch --detach HEAD~1": "switch_detach",
    "git checkout main": "switch_main", "git checkout -b feature": "checkout_b", "git checkout -- f.txt": "restore_file",
    "git checkout --detach v1.0": "switch_detach",
    "git restore f.txt": "restore_file", "git restore --staged f.txt": "restore_staged",
    "git reset": "reset_mixed", "git reset --soft HEAD~1": "reset_soft", "git reset --mixed HEAD~1": "reset_mixed",
    "git reset --hard HEAD~1": "reset_hard", "git reset HEAD -- f.txt": "reset_file",
    "git revert HEAD": "revert", "git revert --no-edit HEAD~2": "revert",
    "git cherry-pick feature": "cherry_pick", "git cherry main feature": "cherry",
    "git merge feature": "merge", "git merge --no-ff feature": "merge_noff", "git merge --squash feature": "merge_squash",
    "git rebase main": "rebase",
    "git stash": "stash_push", "git stash push -m wip": "stash_push", "git stash list": "stash_list",
    "git tag": "tag_list", "git tag v1": "tag_create", "git tag -a v1 -m rel": "tag_annot", "git tag -d v1": "tag_delete",
    "git remote -v": "remote_v", "git fetch": "fetch", "git fetch origin": "fetch", "git pull": "pull", "git push": "push",
    "git push -u origin main": "push",
    "git blame f.txt": "blame", "git blame HEAD~1 -- f.txt": "blame_rev",
    "git reflog": "reflog", "git reflog -n 5": "reflog5", "git clean -n": "clean_n", "git clean -fd": "clean_f",
    "git grep -n needle": "grep", "git grep -i -e needle HEAD": "grep_rev", "git grep -F a.b -- src": "grep_path",
    "git describe --tags": "describe", "git describe --always": "describe_always",
    "git archive -o out.tar HEAD": "archive", "git worktree add ../wt feature": "worktree_add",
    "git worktree list": "worktree_list", "git config user.name": "config_get", "git config --list": "config_list",
    "git ls-files": "lsfiles", "git ls-files -s": "lsfiles_s", "git ls-tree -r --name-only HEAD": "lstree",
    "git ls-tree HEAD": "lstree_flat", "git cat-file -p HEAD:f.txt": "catfile_p", "git cat-file -t HEAD": "catfile_t",
    "git rev-parse HEAD": "revparse", "git rev-parse --short HEAD": "revparse_short",
    "git rev-parse --show-toplevel": "revparse_top", "git shortlog -sn": "shortlog",
    "git merge-base main feature": "mergebase", "git diff-tree -r A B": "difftree", "git fast-export": "fastexport",
    "git bundle create f.bundle main": "bundle_create", "git bundle verify f.bundle": "bundle_verify",
    "git gc": "gc", "git fsck": "fsck", "git repack -ad": "repack", "git format-patch -1 --stdout": "formatpatch",
}
EXTRA_GIT = [  # scenarios measured but not a row of the doc under that exact spelling
    ("git status (1% of files modified)", "vcs status", "status_dirty"),
    ("git diff (clean tree)", "vcs diff", "diff_clean"),
    ("git commit (all 100k files staged, initial)", "vcs commit -m", "commit_all"),
    ("git log (full, all 32 commits)", "vcs log -n 1000000000", "log_all"),
    ("git restore . (1% modified)", "vcs restore .", "restore_all"),
    ("git switch side (1% differ)", "vcs switch side", "switch_side"),
    ("git commit (1 staged file)", "vcs commit -m", "commit_1"),
]
P4 = {
    "client -o / workspace -o": None, "clients / workspaces": "clients", "depots": "depots", "sync": "clone_sync",
    "sync -n": "sync_n", "sync path@rev / @label / @N": "sync_1pct", "edit": "edit_file", "add": "add_file",
    "delete": "delete_file", "revert": "revert_file", "diff": "diff_file", "opened": "opened",
    "reconcile [-a -e -d -n]": "add_all", "status": "status_clean", "submit [-d msg]": "submit_all",
    "changes [-m -s -u -l]": "changes", "describe [-s]": "describe", "filelog": "filelog", "annotate": "annotate",
    "files": "files", "fstat": "fstat", "print": "print", "have": "have", "labels": "labels", "branches / streams": "branches",
    "where": "where", "info": "info", "users": "users", "grep": "grep", "verify": "verify", "init / fetch / push / clone": "clone_sync",
}
P4_EXTRA = [("p4 reconcile -n //... (1% modified)", "vcs status", "status_1pct"),
            ("p4 submit (1 edited file)", "vcs p4 submit -d one", "submit_1")]
REASON = [
    (r"--continue|--abort|--skip", "continuation of an interrupted operation; needs a conflicted state"),
    (r"^git rebase -i|--autosquash| -x |--onto|--root", "interactive or multi-step rebase; no single-state scenario"),
    (r"git am|git apply|format-patch|patch", "needs patch files; not benchmarked"),
    (r"bisect", "multi-step search; not benchmarked"),
    (r"-S |-s |signed|verify-tag|tag -v|-S\b", "signing; needs a key setup; not benchmarked"),
    (r"push (--|origin)|--force|--delete|--mirror|--prune|--tags", "remote ref-update variant; only plain push is measured"),
    (r"git commit --no-verify", "hook switch; no timing scenario"),
    (r"git config", "config write; trivial, not benchmarked"),
    (r"git remote|git merge origin", "remote config or remote-tracking merge; not benchmarked"),
    (r"git stash (pop|apply|drop)", "needs an existing stash entry per run; not benchmarked"),
    (r"git worktree remove|git branch -[Mr]", "not benchmarked"),
    (r"git (checkout|restore) .*--(ours|source)|git checkout HEAD", "not benchmarked"),
    (r"git cherry-pick A..B|git log .*--reverse|git log -n 2|git log --all|git log -p", "variant of a measured command; not run separately"),
]
P4_NM = {
    "client -o / workspace -o": "client spec edit; no timed counterpart",
    "sync -f": "forced re-sync; not benchmarked", "revert -a": "vcs has nothing to do (no-op by design)",
    "diff2": "not benchmarked", "opened -a": "needs a second user with open files; not benchmarked",
    "move / rename": "not benchmarked", "integrate / merge": "needs branch specs; not benchmarked",
    "copy source-branch": "not benchmarked", "resolve [-at -ay -af -n]": "needs a conflict; not benchmarked",
    "shelve [-c name] [-d]": "needs vcs-server for shelves; not benchmarked",
    "unshelve -s name": "needs vcs-server for shelves; not benchmarked",
    "label / tag / labelsync": "not benchmarked", "branch / stream": "not benchmarked", "switch": "not benchmarked",
    "lock": "needs vcs-server; not benchmarked", "unlock [-f]": "needs vcs-server; not benchmarked",
    "locks": "needs vcs-server; not benchmarked", "login / logout": "auth round trip; not benchmarked",
    "clean": "not benchmarked", "change / changelist": "numbered changelists are a local ledger; not benchmarked",
    "change -m desc / -i / -d N / -o N": "ledger; not benchmarked", "add / edit / delete / revert / opened -c N": "ledger; not benchmarked",
    "reopen -c N": "ledger; not benchmarked", "submit -c N": "ledger; not benchmarked", "describe N / @N": "ledger; not benchmarked",
}
WALGIT_CLI = [  # walgit-cli subcommands (crates/walgit-cli/src/lib.rs @ 80e9a20): server administration, not a VCS client
    ("walgit serve", "vcs-server --root ...", "server start-up; the loopback server cells below are the comparable part"),
    ("walgit compact", "vcs gc --remote --compact --yes", None),
    ("walgit repo create/list/info/policy/settings", "vcs-server config files", "server admin; no vcs client counterpart to time"),
    ("walgit wal ...", "(none)", "walgit write-ahead-log inspection; vcs has no WAL"),
    ("walgit synth ...", "datagen", "synthetic data generator; not a VCS command"),
    ("walgit import ...", "vcs import-git", "bulk import into the object store; not benchmarked"),
    ("walgit mirror ...", "(none)", "mirrors a git remote into the store; vcs has no counterpart"),
    ("walgit config ...", "vcs config", "server config; not benchmarked"),
]
WALGIT_CELLS = [  # (label, vcs-side command, walgit/git-side command, key)
    ("initial push (100k files)", "vcs publish", "git push", "init_push"),
    ("push of 1 changed file", "vcs publish", "git push", "inc_push"),
    ("full clone, warm", "vcs clone", "git clone", "clone_full_warm"),
    ("full clone, cold", "vcs clone", "git clone", "clone_full_cold"),
    ("partial/zone clone, warm", "vcs clone --zone", "git clone (partial)", "clone_partial_warm"),
    ("partial/zone clone, cold", "vcs clone --zone", "git clone (partial)", "clone_partial_cold"),
    ("fetch of 1 commit, warm", "vcs fetch", "git fetch", "fetch_1_warm"),
    ("fetch of 1 commit, cold", "vcs fetch", "git fetch", "fetch_1_cold"),
    ("fetch no-op, warm", "vcs fetch", "git fetch", "fetch_noop_warm"),
    ("fetch no-op, cold", "vcs fetch", "git fetch", "fetch_noop_cold"),
    ("fetch of 1 commit after maintenance, warm", "vcs fetch", "git fetch", "post_fetch_1_warm"),
    ("full clone after maintenance, warm", "vcs clone", "git clone", "post_clone_full_warm"),
]


def med(a):
    return statistics.median(a)


def verdict(wv, cv, wr, cr):
    """wv/cv: lists of vcs wall/cpu per round; wr/cr: lists of reference. Returns (verdict, wall_ratio, cpu_ratio, note)."""
    rw = med([x / max(y, 0.001) for x, y in zip(wv, wr)])
    rc = med([x / max(y, 0.001) for x, y in zip(cv, cr)])
    dw = med(wv) - med(wr)
    note = ""
    if abs(dw) < ABS_MS:
        v = "TIE"
    elif rw <= 1 - TIE and rc <= 1 - TIE:
        v = "WIN"
    elif rw >= 1 + TIE and rc >= 1 + TIE:
        v = "LOSS"
    else:
        v = "TIE"
        if rw >= 1 + TIE or rc >= 1 + TIE:
            note = "mixed: " + ("wall" if rw >= 1 + TIE else "cpu") + " slower"
        elif rw <= 1 - TIE or rc <= 1 - TIE:
            note = "mixed: " + ("wall" if rw <= 1 - TIE else "cpu") + " faster"
    return v, rw, rc, note


def cell_stats(c, ref):
    rows = c["rows"]
    if any(r[2] != 0 for t in rows for r in rows[t]):
        bad = [t for t in rows if any(r[2] != 0 for r in rows[t])]
        return {"v": "NOT-MEASURED", "note": "command failed (non-zero exit) for " + "+".join(bad) + "; no ratio given"}
    wv, cv = [r[0] for r in rows["vcs"]], [r[1] for r in rows["vcs"]]
    wr, cr = [r[0] for r in rows[ref]], [r[1] for r in rows[ref]]
    v, rw, rc, note = verdict(wv, cv, wr, cr)
    return {"v": v, "rw": rw, "rc": rc, "note": note, "n": len(wv), "wv": med(wv), "wr": med(wr), "cv": med(cv), "cr": med(cr),
            "load": max(max(l) for l in c["load"]) if c.get("load") else None}


def parse_table(path, ncols):
    rows, hdr = [], None
    for line in open(path):
        if not line.startswith("|"):
            continue
        cells = [x.strip() for x in line.strip().strip("|").split("|")]
        if len(cells) < ncols or set(cells[0]) <= set("-: "):
            continue
        if cells[0] in ("git", "p4", "Perforce", "p4 command"):
            continue
        rows.append([c.strip("`") if c.count("`") == 2 and c.startswith("`") and c.endswith("`") else c.replace("`", "") for c in cells])
    return rows


def reason_for(cmd):
    for rx, why in REASON:
        if re.search(rx, cmd):
            return why
    return "no benchmark scenario built for this row (not run)"


def fmt_ms(x):
    return f"{x:.0f}" if x >= 100 else f"{x:.1f}"


LIGHT = {"WIN": "#1a7f37", "TIE": "#9a6700", "LOSS": "#cf222e", "NM": "#6e7781", "MIS": "#8250df"}


def svg(title, subtitle, rows, path):
    """rows: [(label, verdict, ratio_text)] drawn as a tile grid (3 columns), light/dark aware."""
    order = {"LOSS": 0, "TIE": 1, "WIN": 2, "NOT-MEASURED": 3, "MISSING": 4}
    rows = sorted(rows, key=lambda r: (order[r[1]], r[0]))
    cols, w, h = 3, 420, 20
    nrows = (len(rows) + cols - 1) // cols
    H = 110 + nrows * h + 30
    W = cols * w + 40
    cnt = {k: sum(1 for r in rows if r[1] == k) for k in order}
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" role="img" aria-label="{html.escape(title)}">',
           '<style>:root{--bg:#fff;--fg:#1b1f24;--mut:#57606a;--win:#1a7f37;--tie:#9a6700;--loss:#cf222e;--nm:#6e7781;--mis:#8250df;--pill:#f6f8fa}'
           '@media (prefers-color-scheme:dark){:root{--bg:#0d1117;--fg:#e6edf3;--mut:#9da7b3;--win:#3fb950;--tie:#d29922;--loss:#ff7b72;--nm:#8b949e;--mis:#bc8cff;--pill:#161b22}}'
           'text{font:12px ui-monospace,SFMono-Regular,Menlo,monospace;fill:var(--fg)}.t{font:600 18px system-ui,sans-serif}.s{font:12px system-ui,sans-serif;fill:var(--mut)}'
           '.bg{fill:var(--bg)}.WIN{fill:var(--win)}.TIE{fill:var(--tie)}.LOSS{fill:var(--loss)}.NM{fill:var(--nm)}.MIS{fill:var(--mis)}.r{fill:var(--mut)}</style>',
           f'<rect width="{W}" height="{H}" fill="#ffffff" class="bg"/>',
           f'<text x="20" y="30" class="t" fill="#1b1f24">{html.escape(title)}</text>',
           f'<text x="20" y="50" class="s" fill="#57606a">{html.escape(subtitle)}</text>']
    lx = 20
    for k, cls, nm in (("WIN", "WIN", "WIN"), ("TIE", "TIE", "TIE"), ("LOSS", "LOSS", "LOSS"), ("NOT-MEASURED", "NM", "NOT-MEASURED"), ("MISSING", "MIS", "MISSING")):
        out.append(f'<rect x="{lx}" y="66" width="12" height="12" rx="3" class="{cls}" fill="{LIGHT[cls]}"/><text x="{lx+18}" y="77" fill="#1b1f24">{nm} {cnt[k]}</text>')
        lx += 20 + 9 * len(nm) + 40
    cls_of = {"WIN": "WIN", "TIE": "TIE", "LOSS": "LOSS", "NOT-MEASURED": "NM", "MISSING": "MIS"}
    for i, (lab, v, rt) in enumerate(rows):
        x = 20 + (i % cols) * w
        y = 100 + (i // cols) * h
        lab = lab if len(lab) <= 40 else lab[:39] + "…"
        out.append(f'<rect x="{x}" y="{y}" width="10" height="14" rx="2" class="{cls_of[v]}" fill="{LIGHT[cls_of[v]]}"/>')
        out.append(f'<text x="{x+16}" y="{y+11}" fill="#1b1f24">{html.escape(lab)}</text>')
        out.append(f'<text x="{x+w-28}" y="{y+11}" text-anchor="end" class="r" fill="#57606a">{html.escape(rt)}</text>')
    out.append('</svg>')
    open(path, "w").write("\n".join(out))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", default="docs/research/data/command-scoreboard")
    ap.add_argument("--walgit", default="docs/research/data/walgit-full-boxc/res-flat-100000.json")
    ap.add_argument("--out-md", default="docs/research/command-scoreboard.md")
    ap.add_argument("--img", default="docs/img")
    a = ap.parse_args()
    os.makedirs(a.img, exist_ok=True)

    gcells, gmeta, pcells, pmeta = {}, {}, {}, {}
    for f in sorted(glob.glob(os.path.join(a.data, "git-*.json"))):
        d = json.load(open(f))
        gcells.update(d["cells"])
        gmeta[os.path.basename(f)] = d["meta"]
    pf = os.path.join(a.data, "p4.json")
    if os.path.exists(pf):
        d = json.load(open(pf))
        pcells, pmeta = d["cells"], d["meta"]

    md = []
    # ---- git
    grows = parse_table("docs/git-to-vcs.md", 4)
    grows = [r for r in grows if r[0].startswith("git ")]
    g_out, g_svg = [], []
    for r in grows:
        cmd, vcmd, status = r[0], r[1], r[2]
        sid = GIT.get(cmd)
        if vcmd.strip() == "none" or status == "missing":
            res = {"v": "MISSING", "note": r[3] if len(r) > 3 else ""}
        elif sid and sid in gcells:
            res = cell_stats(gcells[sid], "git")
        elif sid:
            res = {"v": "NOT-MEASURED", "note": f"scenario `{sid}` has no data in this run"}
        else:
            res = {"v": "NOT-MEASURED", "note": reason_for(cmd)}
        res.update(cmd=cmd, vcs=vcmd, sid=sid, doc=status)
        g_out.append(res)
    for lab, vc, sid in EXTRA_GIT:
        if sid in gcells:
            res = cell_stats(gcells[sid], "git")
            res.update(cmd=lab + " *", vcs=vc, sid=sid, doc="extra")
            g_out.append(res)

    def tbl(rs, ref):
        L = [f"| command | vcs | {ref} wall/cpu ms | vcs wall/cpu ms | ratio wall/cpu | verdict | note |", "|---|---|---|---|---|---|---|"]
        for r in rs:
            if "rw" in r:
                L.append(f"| `{r['cmd']}` | `{r['vcs']}` | {fmt_ms(r['wr'])} / {fmt_ms(r['cr'])} | {fmt_ms(r['wv'])} / {fmt_ms(r['cv'])} | {r['rw']:.2f} / {r['rc']:.2f} | **{r['v']}** | {r['note']}{' (n=%d)' % r['n']} |")
            else:
                L.append(f"| `{r['cmd']}` | `{r['vcs']}` | | | | {r['v']} | {r['note'].replace('|', '/')} |")
        return L

    def counts(rs):
        return {k: sum(1 for r in rs if r["v"] == k) for k in ("WIN", "TIE", "LOSS", "NOT-MEASURED", "MISSING")}

    gc = counts(g_out)
    # ---- p4
    prow = parse_table("docs/p4-to-vcs.md", 4)
    prow = [r for r in prow if len(r) >= 4 and r[2] in ("exact", "renamed", "different-semantics", "missing")]
    p_out = []
    for r in prow:
        cmd, vcmd, status = r[0], r[1], r[2]
        sid = P4.get(cmd)
        if status == "missing" or vcmd.strip() in ("(none)",) or vcmd.startswith("(none"):
            res = {"v": "MISSING", "note": "no vcs equivalent: " + vcmd}
        elif sid and sid in pcells:
            res = cell_stats(pcells[sid], "p4")
        elif sid:
            res = {"v": "NOT-MEASURED", "note": f"scenario `{sid}` has no data in this run"}
        else:
            res = {"v": "NOT-MEASURED", "note": P4_NM.get(cmd, "no benchmark scenario built for this row (not run)")}
        res.update(cmd="p4 " + cmd, vcs=vcmd, sid=sid, doc=status)
        p_out.append(res)
    for lab, vc, sid in P4_EXTRA:
        if sid in pcells:
            res = cell_stats(pcells[sid], "p4")
            res.update(cmd=lab + " *", vcs=vc, sid=sid, doc="extra")
            p_out.append(res)
    pc = counts(p_out)

    # ---- walgit
    w_out = []
    try:
        wd = json.load(open(a.walgit))
        res_w, wm = wd["res"], wd["meta"]
    except OSError:
        res_w, wm = None, {}
    if res_w:
        def runs(k, tool):
            c = res_w[tool][k]
            n = len(c["wall_ms"]["runs"])
            g = lambda k: c[k]["runs"] if c.get(k) else [0.0] * n
            return c["wall_ms"]["runs"], [(x + y + z) * 1000 for x, y, z in zip(g("client_cpu_s"), g("srv_cpu_s"), g("rf_cpu_s"))]
        for lab, vc, gc_, key in WALGIT_CELLS:
            for ref, rl in (("walgit", "walgit"), ("git", "git (local bare, file://)")):
                wv, cv = runs(key, "vcs_def")
                wr, cr = runs(key, ref)
                v, rw, rc, note = verdict(wv, cv, wr, cr)
                w_out.append(dict(cmd=f"{lab} [{ 'walgit' if ref == 'walgit' else 'git file://'}]", vcs=vc, ref=ref, v=v, rw=rw, rc=rc, note=note,
                                  wv=med(wv), cv=med(cv), wr=med(wr), cr=med(cr), n=len(wv)))
        mv, mw = res_w["vcs_def"]["maint"]["compact"], res_w["walgit"]["maint"]["ops_compact_force_base"]
        wv, cv = [mv["wall_ms"]], [((mv["client_cpu_s"] or 0) + (mv["srv_cpu_s"] or 0) + (mv["rf_cpu_s"] or 0)) * 1000]
        wr, cr = [mw["wall_ms"]], [((mw["client_cpu_s"] or 0) + (mw["srv_cpu_s"] or 0) + (mw["rf_cpu_s"] or 0)) * 1000]
        v, rw, rc, note = verdict(wv, cv, wr, cr)
        w_out.append(dict(cmd="walgit compact (force, base) [walgit]", vcs="vcs gc --remote --compact --yes", ref="walgit", v="NOT-MEASURED", rw=rw, rc=rc,
                          note=f"ratio {rw:.2f}x wall / {rc:.2f}x cpu shown but no verdict: 1 run only; forced base compaction vs vcs compaction of 10 packs: not the same amount of work", wv=wv[0], cv=cv[0], wr=wr[0], cr=cr[0], n=1))
    for c, vc, why in WALGIT_CLI:
        if why:
            w_out.append(dict(cmd=c, vcs=vc, v="NOT-MEASURED", note=why))
    wc = counts(w_out)

    # ---- markdown
    md += ["# Per-command scoreboard: vcs against git, Perforce and walgit", "",
           "Generated by `scripts/command_scoreboard_render.py` from the JSON in `docs/research/data/command-scoreboard/` "
           "(`scripts/command_scoreboard.py`, `scripts/command_scoreboard_p4.py`) and, for walgit, "
           "`docs/research/data/walgit-full-boxc/`. Images: `docs/img/scoreboard-git.svg`, `scoreboard-p4.svg`, `scoreboard-walgit.svg`.", "",
           "## How to read this", "",
           f"* **Verdict rule (fixed before looking at results).** ratio = vcs / reference, the median over the interleaved rounds of the per-round "
           f"wall ratio and of the per-round CPU ratio (CPU = user+sys of the command from `wait4`; for p4 also the p4d CPU delta; for walgit the sum of client, front-server and object-store CPU). "
           f"**WIN**: both ratios <= {1-TIE:.2f}. **LOSS**: both ratios >= {1+TIE:.2f}. **TIE**: everything else, including any cell whose wall-clock median difference is under {ABS_MS:.0f} ms "
           f"(process start-up noise). A TIE where only one of wall/CPU is beyond the band is flagged `mixed` in the note.",
           "* **MISSING**: the doc table says vcs has no equivalent (listed, not omitted). **NOT-MEASURED**: vcs has the command but no scenario was run, "
           "or the scenario failed (non-zero exit), or the reference tool was not usable; the reason is in the note. No number is invented for those.",
           "* Rows marked `*` are extra scenarios that are not a row of the doc table under that spelling.",
           "* Several doc rows share one scenario (for example `git add .` and `git add -A`). The scenario is stated in the JSON under `cmds`.", ""]
    gm = next(iter(gmeta.values()), {})
    md += ["## Where and how it was run", ""]
    if gm:
        loads = [f"{k.replace('git-','').replace('.json','')}: load1 {m['load1_start']:.1f}->{m.get('load1_end', 0):.1f}" for k, m in gmeta.items()]
        md += [f"* Machine: {gm.get('host')} (AWS c6i.4xlarge timing box, {gm.get('cores')} vCPU), git: `{gm.get('git')}`, vcs: `{gm.get('vcs')}`, "
               f"dataset `datagen flat {gm.get('n')}` (seed 42). Timing under `flock ~/bench.lock`, one group at a time, tools interleaved per round in random order, warm cache.",
               "* **The box was not quiet**: other agents' builds and tests ran on it during these runs (load per group: " + "; ".join(loads) + "). Wall times for slow cells carry that noise; the per-round load is in the JSON.",
               "* Runs per cell: 5 (3 for slow cells), the `n=` in each row says how many. Medians only; min/max are in the JSON. No significance test: the 10% band is a practical threshold, a cell at 0.95 to 1.05 is not distinguishable from noise with 3 to 5 rounds."]
    if pmeta:
        md += [f"* Perforce: `{pmeta.get('p4')}` client, `{pmeta.get('p4d')}` server (binaries copied from the earlier Perforce benchmark, SHA256 re-checked against docs/research/perforce-benchmark.md), "
               "p4d on loopback, journal off, no log, free tier, a fresh server root for the initial add/submit; 1 user, 1 client. p4 is client/server, vcs is local: the comparison is p4 client plus server work "
               "against what vcs does locally; the vcs side of submit also publishes to a local-path remote."]
    md += ["", "Not run, stated plainly: deep and binary layouts (this scoreboard is flat 100k only), any real network or S3, multi-user Perforce, interactive/continuation commands.", ""]

    have = sorted(k.replace("git-", "").replace(".json", "") for k in gmeta) + (["p4"] if pcells else [])
    md += [f"Data groups present in this render: {', '.join(have) or 'none'} (of fresh, clean, dirty, untracked, small, remote, p4). Rows of a missing group show NOT-MEASURED.", ""]
    md += ["## Summary", "", "| tool | WIN | TIE | LOSS | NOT-MEASURED | MISSING |", "|---|---|---|---|---|---|",
           f"| git | {gc['WIN']} | {gc['TIE']} | {gc['LOSS']} | {gc['NOT-MEASURED']} | {gc['MISSING']} |",
           f"| Perforce | {pc['WIN']} | {pc['TIE']} | {pc['LOSS']} | {pc['NOT-MEASURED']} | {pc['MISSING']} |",
           f"| walgit | {wc['WIN']} | {wc['TIE']} | {wc['LOSS']} | {wc['NOT-MEASURED']} | {wc['MISSING']} |", ""]

    md += ["## LOSS list (hand to optimization agents)", ""]
    loss = [("git", r) for r in g_out if r["v"] == "LOSS"] + [("p4", r) for r in p_out if r["v"] == "LOSS"] + [("walgit/git", r) for r in w_out if r["v"] == "LOSS"]
    if loss:
        md += ["| vs | command | vcs | ratio wall/cpu | reference ms | vcs ms |", "|---|---|---|---|---|---|"]
        for t, r in sorted(loss, key=lambda x: -x[1]["rw"]):
            md.append(f"| {t} | `{r['cmd']}` | `{r['vcs']}` | {r['rw']:.2f} / {r['rc']:.2f} | {fmt_ms(r['wr'])} wall / {fmt_ms(r['cr'])} cpu | {fmt_ms(r['wv'])} wall / {fmt_ms(r['cv'])} cpu |")
    else:
        md.append("No LOSS cells in the measured set.")
    mixed = [(t, r) for t, rs in (("git", g_out), ("p4", p_out), ("walgit/git", w_out)) for r in rs if r.get("note", "").startswith("mixed: ") and "slower" in r["note"]]
    if mixed:
        md += ["", "Mixed (TIE by the rule, but one of wall/CPU is >= 10% slower than the reference; worth a look):", "",
               "| vs | command | ratio wall/cpu | note |", "|---|---|---|---|"]
        for t, r in mixed:
            md.append(f"| {t} | `{r['cmd']}` | {r['rw']:.2f} / {r['rc']:.2f} | {r['note']} |")
    md.append("")

    md += ["## git: every row of docs/git-to-vcs.md", ""] + tbl(g_out, "git") + [""]
    md += ["## Perforce: every row of docs/p4-to-vcs.md", ""] + tbl(p_out, "p4") + [""]
    md += ["## walgit", "",
           "walgit's CLI (`walgit-cli`) is server administration; the commands a user runs against a walgit server are plain git (`git push`, `git clone`, `git fetch` over HTTP). "
           "These cells are therefore the remote workflows, **reused unchanged** from `docs/research/data/walgit-full-boxc/` (flat 100k, box c, 3 reps, "
           f"walgit `{wm.get('walgit_rev','')[:12]}`, vcs `{wm.get('vcs_rev','')}`, {wm.get('git','git')}, load1 {wm.get('load_start',['?'])[0]} -> {wm.get('load_end',['?'])[0]}). "
           "That vcs build is `84f3fa2`, older than current main, and the vcs side is `vcs default` against `vcs-server` on loopback; walgit is against a local RustFS, git against a local bare repository. "
           "Nothing was re-run for this section; if the vcs build matters, re-run `scripts/walgit_bench/walgit_full.py`.", ""]
    L = ["| workflow | vcs | ref wall/cpu ms | vcs wall/cpu ms | ratio wall/cpu | verdict | note |", "|---|---|---|---|---|---|---|"]
    for r in w_out:
        if "rw" in r:
            L.append(f"| {r['cmd']} | `{r['vcs']}` | {fmt_ms(r['wr'])} / {fmt_ms(r['cr'])} | {fmt_ms(r['wv'])} / {fmt_ms(r['cv'])} | {r['rw']:.2f} / {r['rc']:.2f} | **{r['v']}** | {r['note']} (n={r['n']}) |")
        else:
            L.append(f"| `{r['cmd']}` | `{r['vcs']}` | | | | {r['v']} | {r['note']} |")
    md += L + [""]

    # failures / unmeasured in the git+p4 data
    md += ["## Reproduce", "",
           "```sh",
           "# on box c, under the lock, one group at a time (see the header of scripts/command_scoreboard.py)",
           "python3 -I scripts/command_scoreboard.py setup --work W --vcs target/release/vcs --datagen target/release/datagen --n 100000",
           "for g in fresh clean dirty untracked small; do flock ~/bench.lock python3 -I scripts/command_scoreboard.py run --work W --vcs target/release/vcs --group $g --out git-$g.json; done",
           "flock ~/bench.lock python3 -I scripts/command_scoreboard_p4.py --work W --vcs target/release/vcs --p4 P4 --p4d P4D --datagen target/release/datagen --out p4.json",
           "flock ~/bench.lock python3 -I scripts/command_scoreboard.py run --work W --vcs target/release/vcs --group remote --out git-remote.json",
           "python3 -I scripts/command_scoreboard_render.py",
           "```", ""]
    open(a.out_md, "w").write("\n".join(md))

    def rt(r):
        return f"{r['rw']:.2f}x" if "rw" in r else ""
    svg("vcs vs git, per command (docs/git-to-vcs.md)", f"ratio = vcs/git wall; WIN/LOSS need wall and CPU beyond {int(TIE*100)}%; flat 100k, c6i.4xlarge", [(r["cmd"].replace("git ", "", 1), r["v"], rt(r)) for r in g_out], os.path.join(a.img, "scoreboard-git.svg"))
    svg("vcs vs Perforce, per command (docs/p4-to-vcs.md)", "ratio = vcs/p4 wall (p4 includes the p4d server work); flat 100k, c6i.4xlarge", [(r["cmd"].replace("p4 ", "", 1), r["v"], rt(r)) for r in p_out], os.path.join(a.img, "scoreboard-p4.svg"))
    svg("vcs vs walgit (remote workflows and walgit CLI)", "ratio = vcs/reference wall; reused from walgit-full-boxc (vcs build 84f3fa2)", [(r["cmd"], r["v"], rt(r)) for r in w_out], os.path.join(a.img, "scoreboard-walgit.svg"))
    print("git", gc, "p4", pc, "walgit", wc)


if __name__ == "__main__":
    main()
