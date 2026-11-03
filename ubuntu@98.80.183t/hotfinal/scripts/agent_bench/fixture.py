"""Deterministic fixture: one scripted history applied to a git repo (git CLI) and a vcs repo (vcs CLI).

Ground truth for every task is computed from SPEC below, never from either tool's output.
"""
import os
import subprocess

T0 = 1700000000
STEP = 3600

# zone name -> path prefix
ZONES = {
    "core": "crates/core", "util": "crates/util", "storage": "crates/storage", "net": "crates/net",
    "api": "crates/api", "cli": "crates/cli", "tools": "tools/gen", "docs": "docs",
}
DESC = {
    "core": "Core ledger primitives", "util": "Shared string and time helpers", "storage": "Durable page store",
    "net": "HTTP client with retries", "api": "Public request handlers", "cli": "Command line front end",
    "tools": "Code generators for fixtures",
}
DEPS = {"core": [], "util": [], "storage": ["core"], "net": ["core", "util"], "api": ["storage", "net"],
        "cli": ["api"], "tools": ["util"]}
CODEOWNERS = [("/crates/core/", ["@alice", "@bob"]), ("/crates/storage/", ["@bob"]), ("/crates/net/", ["@carol"]),
              ("/crates/api/", ["@alice", "@dave"]), ("/docs/", ["@erin"]), ("/tools/", ["@carol"])]
AUTHORS = {"alice": "Alice Ng", "bob": "Bob Roy", "carol": "Carol Diaz", "dave": "Dave Kim", "erin": "Erin Lee"}

# (author, subject, {path: content-or-None(delete)}).  Later entries may rewrite a path.
COMMITS = [
    ("alice", "init workspace manifests", {}),  # files filled in by initial_files()
    ("bob", "storage: add page cache", {"crates/storage/src/cache.rs": "pub struct PageCache;\n",
                                          "crates/storage/src/legacy.rs": "pub fn legacy_index() {}\n"}),
    ("carol", "net: add http client", {"crates/net/src/client.rs": "pub const MAX_RETRIES: u32 = 3;\n"}),
    ("carol", "net: add retry_with_backoff", {"crates/net/src/retry.rs": "pub fn retry_with_backoff() {}\n"}),
    ("alice", "api: add request router", {"crates/api/src/router.rs": "pub fn route() {}\n"}),
    ("dave", "api+core: shared error type", {"crates/core/src/error.rs": "pub enum Error {}\n",
                                             "crates/api/src/error.rs": "pub use core::Error;\n"}),
    ("erin", "docs: write intro guide", {"docs/guide/intro.md": "# Intro\nWelcome.\n"}),
    ("bob", "storage: fsync on commit", {"crates/storage/src/lib.rs": "pub fn open() { sync_all(); }\n"}),
    ("carol", "net: raise MAX_RETRIES to 5", {"crates/net/src/client.rs": "pub const MAX_RETRIES: u32 = 5;\n"}),
    ("alice", "cli: add serve command", {"crates/cli/src/serve.rs": "pub fn serve() {}\n"}),
    ("dave", "util+tools: date helpers and generator", {"crates/util/src/date.rs": "pub fn today() {}\n",
                                                        "tools/gen/src/main.rs": "fn main() {}\n"}),
    ("carol", "net+api: timeouts", {"crates/net/src/timeout.rs": "pub const TIMEOUT_MS: u64 = 3000;\n",
                                    "crates/api/src/router.rs": "pub fn route() { timeout(); }\n"}),
    ("bob", "core: ledger rounding fix", {"crates/core/src/lib.rs": "pub fn round() {}\n"}),
    ("alice", "storage: drop legacy index", {"crates/storage/src/legacy.rs": None}),
    ("erin", "docs: update api reference", {"docs/guide/api.md": "# API\nRoutes.\n", "docs/guide/intro.md": "# Intro\nWelcome back.\n"}),
    ("dave", "cli+api: version flag", {"crates/cli/src/main.rs": "fn main() { version(); }\n",
                                       "crates/api/src/router.rs": "pub fn route() { timeout(); version(); }\n"}),
]


def zone_of(path):
    for z, p in ZONES.items():
        if path == p or path.startswith(p + "/"):
            return z
    return "root"


def manifest(z):
    p = ZONES[z]
    t = f'[package]\nname = "{z}"\nversion = "0.1.0"\ndescription = "{DESC[z]}"\n\n[dependencies]\n'
    for d in DEPS[z]:
        rel = os.path.relpath(ZONES[d], p)
        t += f'{d} = {{ path = "{rel}" }}\n'
    return t


def initial_files():
    f = {}
    for z in DEPS:
        f[f"{ZONES[z]}/Cargo.toml"] = manifest(z)
        f[f"{ZONES[z]}/src/lib.rs"] = f"// {z}\n"
    f["docs/README.md"] = "# Docs\n\nOperator handbook and guides.\n"
    f["README.md"] = "# Demo monorepo\n\nA small multi-component repository.\n"
    f["Makefile"] = "all:\n\t@echo build\n"
    f["CODEOWNERS"] = "".join(f"{p} {' '.join(o)}\n" for p, o in CODEOWNERS)
    return f


def commit_files(i):
    return initial_files() if i == 0 else COMMITS[i][2]


def ts(i):
    return T0 + i * STEP


def changes(i):
    """{path: kind} of commit i, computed by replaying the spec."""
    state = {}
    out = {}
    for j in range(i + 1):
        for p, c in commit_files(j).items():
            if j == i:
                out[p] = "deleted" if c is None else ("modified" if p in state else "added")
            if c is None:
                state.pop(p, None)
            else:
                state[p] = c
    return out


def subject(i):
    return COMMITS[i][1]


def _run(argv, cwd, env=None):
    e = dict(os.environ)
    e.update(env or {})
    r = subprocess.run(argv, cwd=cwd, env=e, capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"{argv} failed: {r.stderr}")
    return r.stdout


def _apply(root, files):
    for p, c in files.items():
        full = os.path.join(root, p)
        if c is None:
            os.remove(full)
        else:
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "w") as fh:
                fh.write(c)


def build(dest, vcs_bin):
    """Create dest/git and dest/vcs from the same scripted history."""
    g, v = os.path.join(dest, "git"), os.path.join(dest, "vcs")
    os.makedirs(g)
    os.makedirs(v)
    _run(["git", "init", "-q", "-b", "main"], g)
    _run([vcs_bin, "init", v], dest)
    for z, p in ZONES.items():
        _run([vcs_bin, "zone", "create", z, "--path", p], v)
    for i, (a, subj, _) in enumerate(COMMITS):
        files = commit_files(i)
        name, email = AUTHORS[a], f"{a}@example.com"
        _apply(g, files)
        _apply(v, files)
        date = f"{ts(i)} +0000"
        _run(["git", "add", "-A"], g)
        _run(["git", "commit", "-q", "-m", subj], g, {
            "GIT_AUTHOR_NAME": name, "GIT_AUTHOR_EMAIL": email, "GIT_AUTHOR_DATE": date,
            "GIT_COMMITTER_NAME": name, "GIT_COMMITTER_EMAIL": email, "GIT_COMMITTER_DATE": date})
        _run([vcs_bin, "add", "."], v)
        _run([vcs_bin, "commit", "-m", subj], v, {
            "VCS_AUTHOR_NAME": name, "VCS_AUTHOR_EMAIL": email, "VCS_TIMESTAMP": str(ts(i))})
    return g, v
