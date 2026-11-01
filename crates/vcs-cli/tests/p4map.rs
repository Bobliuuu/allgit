//! `vcs p4 ...` end to end with the real binary, one table row per mapped Perforce command
//! (docs/p4-to-vcs.md). Every case runs in its own temp world: alice's repo with a directory
//! remote, and bob's clone of it.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(dir: &Path, who: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", who)
        .env("VCS_AUTHOR_EMAIL", format!("{who}@example.com"))
        .env("VCS_PRINCIPAL", who)
        .env("VCS_NO_HOOKS", "1")
        .env_remove("VCS_P4_LOCK_LEASE_SECS")
        .output()
        .unwrap()
}
fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

struct W {
    _t: tempfile::TempDir,
    root: PathBuf,
    a: PathBuf,
    b: PathBuf,
    url: String,
}

impl W {
    fn new() -> W {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_path_buf();
        let (a, b) = (root.join("a"), root.join("b"));
        let remote = root.join("remote");
        let url = format!("file://{}", remote.display());
        let w = W {
            _t: t,
            root: root.clone(),
            a: a.clone(),
            b: b.clone(),
            url: url.clone(),
        };
        w.vcs(&root, "alice", &["init", "a"]);
        w.write(&a, ".vcsattributes", "*.psd lockable\n");
        w.write(&a, "a.txt", "one\n");
        w.write(&a, "b.txt", "bee\n");
        w.write(&a, "src/c.txt", "see\n");
        w.write(&a, "art/hero.psd", "v1\n");
        w.vcs(&a, "alice", &["add", "."]);
        w.vcs(&a, "alice", &["commit", "-m", "init"]);
        w.vcs(&a, "alice", &["remote", "set", &url]);
        w.vcs(&a, "alice", &["publish"]);
        w.vcs(&root, "bob", &["clone", &url, "b"]);
        w
    }
    fn write(&self, d: &Path, f: &str, c: &str) {
        let p = d.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        if p.exists() {
            let mut perm = std::fs::metadata(&p).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            std::fs::set_permissions(&p, perm).unwrap();
        }
        std::fs::write(p, c).unwrap();
    }
    fn read(&self, d: &Path, f: &str) -> String {
        std::fs::read_to_string(d.join(f)).unwrap()
    }
    /// a plain vcs command that must succeed
    fn vcs(&self, d: &Path, who: &str, args: &[&str]) -> String {
        let o = run(d, who, args);
        assert!(o.status.success(), "vcs {args:?} failed: {}", text(&o));
        text(&o)
    }
    /// `vcs p4 <args>` as alice in her repo, must succeed
    fn p4(&self, args: &[&str]) -> String {
        self.p4_in(&self.a, "alice", args)
    }
    fn p4_in(&self, d: &Path, who: &str, args: &[&str]) -> String {
        let mut v = vec!["p4"];
        v.extend(args);
        self.vcs(d, who, &v)
    }
    /// `vcs p4 <args>` must fail; returns its output
    fn p4_fail(&self, args: &[&str]) -> String {
        self.p4_fail_in(&self.a, "alice", args)
    }
    fn p4_fail_in(&self, d: &Path, who: &str, args: &[&str]) -> String {
        let mut v = vec!["p4"];
        v.extend(args);
        let o = run(d, who, &v);
        assert!(!o.status.success(), "vcs {v:?} unexpectedly succeeded: {}", text(&o));
        text(&o)
    }
    fn status(&self) -> String {
        self.vcs(&self.a, "alice", &["status"])
    }
    fn commit_edit(&self, f: &str, c: &str, m: &str) {
        self.write(&self.a, f, c);
        self.vcs(&self.a, "alice", &["add", f]);
        self.vcs(&self.a, "alice", &["commit", "-m", m]);
    }
}

fn client_view(w: &W) {
    let o = w.p4(&["client", "-o"]);
    assert!(o.contains("root") && o.contains("vcs zone list"), "{o}");
    assert!(w.p4_fail(&["client"]).contains("no vcs equivalent"));
}
fn clients(w: &W) {
    assert!(w.p4(&["clients"]).contains("(main)"));
}
fn depots(w: &W) {
    assert!(w.p4(&["depots"]).contains("root"));
}
fn sync(w: &W) {
    w.p4(&["edit", "a.txt"]);
    w.commit_edit("a.txt", "one\ntwo\n", "second");
    w.vcs(&w.a, "alice", &["publish"]);
    w.p4_in(&w.b, "bob", &["sync"]);
    assert_eq!(w.read(&w.b, "a.txt"), "one\ntwo\n");
}
fn sync_f(w: &W) {
    w.write(&w.b, "b.txt", "scribble\n");
    w.p4_in(&w.b, "bob", &["sync", "-f"]);
    assert_eq!(w.read(&w.b, "b.txt"), "bee\n");
}
fn sync_n(w: &W) {
    w.commit_edit("a.txt", "new\n", "second");
    w.vcs(&w.a, "alice", &["publish"]);
    let before = w.vcs(&w.b, "bob", &["rev-parse", "HEAD"]);
    let o = w.p4_in(&w.b, "bob", &["sync", "-n"]);
    assert!(o.contains("no dry run"), "{o}");
    assert_eq!(before, w.vcs(&w.b, "bob", &["rev-parse", "HEAD"]));
    assert_eq!(w.read(&w.b, "a.txt"), "one\n");
}
fn sync_rev_missing(w: &W) {
    assert!(w.p4_fail(&["sync", "//depot/...@5"]).contains("changelist number"));
    assert!(w.p4_fail(&["sync", "//depot/...@v1"]).contains("no vcs equivalent"));
}
fn edit_plain(w: &W) {
    assert!(w.p4(&["edit", "a.txt"]).contains("open for edit"));
    assert!(w.p4_fail(&["edit", "nope.txt"]).contains("not found"));
    assert!(w.p4_fail(&["edit", "-c", "12", "a.txt"]).contains("pending changelist 12"));
}
fn edit_lockable(w: &W) {
    let o = w.p4(&["edit", "art/hero.psd"]);
    assert!(o.contains("locked art/hero.psd"), "{o}");
    assert!(w.vcs(&w.a, "alice", &["locks"]).contains("art/hero.psd"));
    let f = w.p4_fail_in(&w.b, "bob", &["edit", "art/hero.psd"]);
    assert!(f.to_lowercase().contains("lock"), "{f}");
}
fn add(w: &W) {
    w.write(&w.a, "n.txt", "n\n");
    assert!(w.p4(&["add", "-n", "n.txt"]).contains("would add"));
    w.p4(&["add", "n.txt"]);
    assert!(w.status().contains("new file:  n.txt"));
    assert!(w.p4_fail(&["add"]).contains("no file given"));
}
fn delete(w: &W) {
    w.p4(&["delete", "b.txt"]);
    assert!(!w.a.join("b.txt").exists());
    assert!(w.status().contains("deleted:   b.txt"));
}
fn move_(w: &W) {
    w.p4(&["move", "b.txt", "d.txt"]);
    assert!(w.a.join("d.txt").exists() && !w.a.join("b.txt").exists());
    w.p4(&["rename", "d.txt", "e.txt"]);
    assert!(w.a.join("e.txt").exists());
}
fn revert(w: &W) {
    w.write(&w.a, "a.txt", "changed\n");
    w.vcs(&w.a, "alice", &["add", "a.txt"]);
    w.write(&w.a, "n.txt", "n\n");
    w.vcs(&w.a, "alice", &["add", "n.txt"]);
    w.p4(&["revert", "a.txt", "n.txt"]);
    assert_eq!(w.read(&w.a, "a.txt"), "one\n");
    assert!(w.a.join("n.txt").exists(), "a reverted add stays on disk, like p4");
    let st = w.status();
    assert!(!st.contains("staged:") && st.contains("untracked:"), "{st}");
    w.write(&w.a, "b.txt", "changed\n");
    assert!(w.p4(&["revert", "-n", "b.txt"]).contains("modified:"));
}
fn revert_a(w: &W) {
    assert!(w.p4(&["revert", "-a"]).contains("nothing to do"));
}
fn diff(w: &W) {
    w.write(&w.a, "a.txt", "one\nx\n");
    w.write(&w.a, "b.txt", "bee\ny\n");
    let o = w.p4(&["diff", "a.txt"]);
    assert!(o.contains("+x") && !o.contains("+y"), "{o}");
    assert!(w.p4(&["diff", "-du"]).contains("+y"));
    let names = w.p4(&["diff", "-se"]);
    assert!(names.contains("//depot/a.txt") && names.contains("//depot/b.txt"), "{names}");
    w.vcs(&w.a, "alice", &["add", "a.txt"]);
    assert!(w.p4(&["diff", "-sa"]).contains("//depot/a.txt"));
}
fn diff2(w: &W) {
    w.vcs(&w.a, "alice", &["tag", "v1"]);
    w.commit_edit("a.txt", "one\nmore\n", "second");
    let o = w.p4(&["diff2", "a.txt@v1", "a.txt"]);
    assert!(o.contains("+more"), "{o}");
    assert!(w.p4(&["diff2", "a.txt", "a.txt"]).contains("identical"));
}
fn opened(w: &W) {
    assert!(w.p4(&["opened"]).contains("not opened"));
    w.write(&w.a, "a.txt", "e\n");
    w.write(&w.a, "n.txt", "n\n");
    w.vcs(&w.a, "alice", &["add", "n.txt"]);
    let o = w.p4(&["opened"]);
    assert!(o.contains("//depot/a.txt - edit default change (not staged"), "{o}");
    assert!(o.contains("//depot/n.txt - add default change"), "{o}");
    assert!(!w.p4(&["opened", "n.txt"]).contains("a.txt"));
}
fn opened_a(w: &W) {
    w.p4(&["lock", "art/hero.psd"]);
    assert!(w.p4_in(&w.b, "bob", &["opened", "-a"]).contains("art/hero.psd"));
}
fn reconcile(w: &W) {
    w.write(&w.a, "n.txt", "n\n");
    w.write(&w.a, "a.txt", "e\n");
    std::fs::remove_file(w.a.join("b.txt")).unwrap();
    let o = w.p4(&["reconcile", "-n"]);
    assert!(
        o.contains("//depot/n.txt - would add") && o.contains("a.txt - would edit") && o.contains("b.txt - would delete"),
        "{o}"
    );
    assert!(!w.status().contains("\n  staged:"), "preview must not stage");
    w.p4(&["reconcile", "-a"]);
    let st = w.status();
    assert!(st.contains("new file:  n.txt") && !st.contains("modified:  a.txt\n  not"), "{st}");
    w.p4(&["reconcile", "-e", "-d"]);
    let st = w.status();
    assert!(st.contains("deleted:   b.txt") && !st.contains("not staged"), "{st}");
}
fn p4_status(w: &W) {
    w.write(&w.a, "n.txt", "n\n");
    assert!(w.p4(&["status"]).contains("n.txt - would add"));
}
fn submit(w: &W) {
    w.write(&w.a, "a.txt", "one\nsubmitted\n");
    w.write(&w.a, "n.txt", "n\n");
    w.p4(&["add", "n.txt"]);
    let o = w.p4(&["submit", "-d", "my change"]);
    assert!(o.contains("my change") || o.contains("published") || o.contains("publish"), "{o}");
    let log = w.vcs(&w.a, "alice", &["log", "-n", "1", "--oneline"]);
    assert!(log.contains("my change"), "{log}");
    w.p4_in(&w.b, "bob", &["sync"]);
    assert_eq!(w.read(&w.b, "a.txt"), "one\nsubmitted\n");
    assert_eq!(w.read(&w.b, "n.txt"), "n\n");
    // no remote: commits locally and says so
    let l = w.root.join("local");
    w.vcs(&w.root, "carol", &["init", "local"]);
    w.write(&l, "f.txt", "1\n");
    w.vcs(&l, "carol", &["add", "f.txt"]);
    w.vcs(&l, "carol", &["commit", "-m", "base"]);
    w.write(&l, "f.txt", "2\n");
    let o = w.p4_in(&l, "carol", &["submit", "-d", "local change"]);
    assert!(o.contains("local only"), "{o}");
    assert!(w.vcs(&l, "carol", &["log", "-n", "1", "--oneline"]).contains("local change"));
    assert!(w.p4_fail_in(&l, "carol", &["submit", "-c", "9"]).contains("pending changelist 9"));
}
fn changes(w: &W) {
    w.commit_edit("a.txt", "two\n", "second one");
    let o = w.p4(&["changes", "-m", "1"]);
    assert!(o.contains("second one") && !o.contains("init"), "{o}");
    assert!(w.p4(&["changes"]).contains("init"));
    assert!(w.p4(&["changes", "-u", "alice", "-l"]).contains("init"));
    assert!(w.p4(&["changes", "-m1", "a.txt"]).contains("second one"));
    assert!(w.p4(&["changes", "-s", "pending"]).contains("zone root"));
    assert!(w.p4_fail(&["changes", "-s", "bogus"]).contains("no vcs equivalent"));
}
fn describe(w: &W) {
    let o = w.p4(&["describe", "-s", "HEAD"]);
    assert!(o.contains("init") && o.contains("a.txt"), "{o}");
    assert!(w.p4_fail(&["describe", "12"]).contains("changelist number"));
}
fn filelog(w: &W) {
    w.commit_edit("a.txt", "two\n", "second one");
    let o = w.p4(&["filelog", "a.txt"]);
    assert!(o.contains("second one") && o.contains("init"), "{o}");
}
fn annotate(w: &W) {
    let o = w.p4(&["annotate", "a.txt"]);
    assert!(o.contains("one"), "{o}");
}
fn files(w: &W) {
    let o = w.p4(&["files", "//depot/..."]);
    assert!(o.contains("a.txt") && o.contains("src/c.txt"), "{o}");
    assert!(!w.p4(&["files", "src/..."]).contains("a.txt"));
}
fn fstat(w: &W) {
    let o = w.p4(&["fstat", "a.txt"]);
    assert!(
        o.contains("... depotFile") && o.contains("... headChange") && o.contains("... headAuthor alice"),
        "{o}"
    );
}
fn print(w: &W) {
    w.vcs(&w.a, "alice", &["tag", "v1"]);
    w.commit_edit("a.txt", "two\n", "second");
    assert!(w.p4(&["-q", "print", "-q", "a.txt"]).ends_with("two\n"));
    assert!(w.p4(&["-q", "print", "-q", "a.txt@v1"]).ends_with("one\n"));
    assert!(w.p4_fail(&["print", "-q", "a.txt#2"]).contains("revision numbers"));
}
fn have(w: &W) {
    let o = w.p4(&["have"]);
    assert!(o.contains("a.txt") && o.contains("art/hero.psd"), "{o}");
}
fn setup_conflict(w: &W) {
    w.p4(&["branch", "dev"]);
    w.p4(&["switch", "dev"]);
    w.commit_edit("a.txt", "dev\n", "dev edit");
    w.p4(&["switch", "main"]);
}
fn integrate(w: &W) {
    w.p4(&["branch", "dev"]);
    assert!(w.p4(&["branches"]).contains("dev"));
    w.p4(&["switch", "//depot/dev"]);
    w.commit_edit("b.txt", "from dev\n", "dev edit");
    w.p4(&["switch", "main"]);
    w.p4(&["integrate", "//depot/dev/..."]);
    assert_eq!(w.read(&w.a, "b.txt"), "from dev\n");
    assert!(w.p4_fail(&["integrate", "dev", "main"]).contains("target is always"));
    assert!(w.p4_fail(&["integrate", "-n", "dev"]).contains("no vcs equivalent"));
}
fn copy_missing(w: &W) {
    assert!(w
        .p4_fail(&["copy", "//depot/dev/...", "//depot/main/..."])
        .contains("no vcs equivalent"));
}
fn resolve(w: &W) {
    setup_conflict(w);
    w.commit_edit("a.txt", "main\n", "main edit");
    let e = w.p4_fail(&["integrate", "dev"]);
    assert!(e.contains("conflict"), "{e}");
    assert!(w.p4(&["resolve", "-n"]).contains("a.txt"));
    assert!(w.p4_fail(&["resolve", "a.txt"]).contains("interactive"));
    w.p4(&["resolve", "-at"]);
    assert_eq!(w.read(&w.a, "a.txt"), "dev\n");
    w.vcs(&w.a, "alice", &["commit", "-m", "merged"]);
    // -ay keeps the checked-out side
    w.p4(&["switch", "dev"]);
    w.commit_edit("a.txt", "dev2\n", "dev again");
    w.p4(&["switch", "main"]);
    w.commit_edit("a.txt", "main2\n", "main again");
    assert!(w.p4_fail(&["integrate", "dev"]).contains("conflict"));
    w.p4(&["resolve", "-ay", "a.txt"]);
    assert_eq!(w.read(&w.a, "a.txt"), "main2\n");
}
fn shelve(w: &W) {
    w.write(&w.a, "a.txt", "shelved\n");
    w.vcs(&w.a, "alice", &["add", "a.txt"]);
    w.p4(&["shelve", "-c", "s1"]);
    assert!(w.p4(&["changes", "-s", "shelved"]).contains("s1"));
    w.p4(&["revert", "a.txt"]);
    assert_eq!(w.read(&w.a, "a.txt"), "one\n");
    w.p4(&["unshelve", "-s", "s1"]);
    assert_eq!(w.read(&w.a, "a.txt"), "shelved\n");
    w.p4(&["shelve", "-d", "-c", "s1"]);
    assert!(w.p4_fail(&["shelve", "-d"]).contains("-c <name>"));
}
fn label(w: &W) {
    w.p4(&["tag", "-l", "rel1"]);
    assert!(w.p4(&["labels"]).contains("rel1"));
    w.p4(&["labelsync", "-l", "rel2"]);
    w.p4(&["label", "rel3"]);
    let o = w.p4(&["labels"]);
    assert!(o.contains("rel2") && o.contains("rel3"), "{o}");
    w.p4(&["tag", "-d", "-l", "rel1"]);
    assert!(!w.p4(&["labels"]).contains("rel1"));
    w.p4(&["tag", "-l", "rel4", "//depot/..."]);
    assert!(w.p4_fail(&["tag", "-l", "x", "a.txt"]).contains("whole commit"));
}
fn labels(w: &W) {
    w.p4(&["tag", "-l", "r1"]);
    assert!(w.p4(&["labels"]).contains("r1"));
}
fn branch(w: &W) {
    w.p4(&["branch", "feat"]);
    assert!(w.vcs(&w.a, "alice", &["branch"]).contains("feat"));
    w.p4(&["branch", "-d", "feat"]);
    assert!(!w.vcs(&w.a, "alice", &["branch"]).contains("feat"));
    w.p4(&["stream", "//depot/s1"]);
    assert!(w.p4(&["streams"]).contains("s1"));
}
fn branches(w: &W) {
    assert!(w.p4(&["branches"]).contains("main"));
}
fn switch(w: &W) {
    w.p4(&["branch", "x"]);
    assert!(w.p4(&["switch", "x"]).contains("switched to x"));
}
fn lock(w: &W) {
    let o = w.p4(&["lock", "art/hero.psd"]);
    assert!(o.contains("locked art/hero.psd") && o.contains("28800"), "{o}");
    assert!(w.p4_in(&w.b, "bob", &["locks"]).contains("art/hero.psd"));
    assert!(!w.p4_fail_in(&w.b, "bob", &["unlock", "art/hero.psd"]).is_empty());
    w.p4(&["unlock", "art/hero.psd"]);
    assert!(!w.p4(&["locks"]).contains("art/hero.psd"));
    let e = w.p4_in(&w.a, "alice", &["--explain", "unlock", "-f", "x"]);
    assert!(e.contains("vcs unlock --break"), "{e}");
}
fn where_(w: &W) {
    let o = w.p4(&["where", "src/c.txt"]);
    assert!(o.contains("//depot/src/c.txt") && o.contains("/a/src/c.txt"), "{o}");
}
fn info(w: &W) {
    let o = w.p4(&["info"]);
    assert!(
        o.contains("Client name: a") && o.contains("Server address: ") && o.contains("remote"),
        "{o}"
    );
}
fn users(w: &W) {
    assert!(w.p4(&["users"]).contains("alice"));
}
fn login(w: &W) {
    let e = w.p4(&["--explain", "login"]);
    assert!(e.contains("vcs login "), "{e}");
    let e = w.p4(&["--explain", "login", "-s"]);
    assert!(e.contains("vcs auth status"), "{e}");
    assert!(w.p4(&["--explain", "logout"]).contains("vcs logout"));
}
fn push(w: &W) {
    w.commit_edit("a.txt", "two\n", "pushed");
    w.p4(&["push"]);
    w.p4_in(&w.b, "bob", &["fetch"]);
    w.p4_in(&w.b, "bob", &["sync"]);
    assert_eq!(w.read(&w.b, "a.txt"), "two\n");
    let d = w.root.join("cl");
    w.p4_in(&w.root, "bob", &["clone", "-p", &w.url, "-d", d.to_str().unwrap()]);
    assert_eq!(w.read(&d, "a.txt"), "two\n");
    let i = w.root.join("fresh");
    std::fs::create_dir(&i).unwrap();
    w.p4_in(&i, "bob", &["init"]);
    assert!(i.join(".vcs").exists());
}
fn grep(w: &W) {
    let o = w.p4(&["grep", "-n", "-e", "see"]);
    assert!(o.contains("src/c.txt"), "{o}");
    assert!(w.p4_fail(&["grep", "-v", "-e", "x"]).contains("no vcs equivalent"));
}
fn clean(w: &W) {
    w.write(&w.a, "a.txt", "junk\n");
    w.write(&w.a, "u.txt", "u\n");
    w.p4(&["clean"]);
    assert_eq!(w.read(&w.a, "a.txt"), "one\n");
    assert!(w.a.join("u.txt").exists(), "untracked files are kept and the note says so");
}
fn change(w: &W) {
    let o = w.p4(&["change", "-o"]);
    assert!(o.contains("no numbered pending changelists"), "{o}");
}
fn verify(w: &W) {
    w.p4(&["verify"]);
}
fn missing_cmds(w: &W) {
    for c in ["reopen", "obliterate", "jobs", "fix"] {
        assert!(w.p4_fail(&[c]).contains("no vcs equivalent"), "{c}");
    }
    let e = w.p4_fail(&["frobnicate"]);
    assert!(e.contains("not an everyday command"), "{e}");
    let g = w.p4_fail(&["-p", "host:1666", "sync"]);
    assert!(g.contains("no vcs equivalent"), "{g}");
}
fn protect_missing(w: &W) {
    for c in ["protect", "groups", "passwd"] {
        let e = w.p4_fail(&[c]);
        assert!(e.contains("no vcs equivalent") && e.contains("docs/server.md"), "{e}");
    }
}

type Case = (&'static str, fn(&W));
const CASES: &[Case] = &[
    ("client_view", client_view),
    ("clients", clients),
    ("depots", depots),
    ("sync", sync),
    ("sync_f", sync_f),
    ("sync_n", sync_n),
    ("sync_rev_missing", sync_rev_missing),
    ("edit_plain", edit_plain),
    ("edit_lockable", edit_lockable),
    ("add", add),
    ("delete", delete),
    ("move", move_),
    ("revert", revert),
    ("revert_a", revert_a),
    ("diff", diff),
    ("diff2", diff2),
    ("opened", opened),
    ("opened_a", opened_a),
    ("reconcile", reconcile),
    ("p4_status", p4_status),
    ("submit", submit),
    ("changes", changes),
    ("describe", describe),
    ("filelog", filelog),
    ("annotate", annotate),
    ("files", files),
    ("fstat", fstat),
    ("print", print),
    ("have", have),
    ("integrate", integrate),
    ("copy_missing", copy_missing),
    ("resolve", resolve),
    ("shelve", shelve),
    ("label", label),
    ("labels", labels),
    ("branch", branch),
    ("branches", branches),
    ("switch", switch),
    ("lock", lock),
    ("where", where_),
    ("info", info),
    ("users", users),
    ("login", login),
    ("push", push),
    ("grep", grep),
    ("clean", clean),
    ("change", change),
    ("verify", verify),
    ("missing_cmds", missing_cmds),
    ("protect_missing", protect_missing),
];

#[test]
fn every_mapped_pair_end_to_end() {
    let mut failed = vec![];
    for (name, f) in CASES {
        let w = W::new();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&w)));
        if r.is_err() {
            failed.push(*name);
        }
    }
    assert!(failed.is_empty(), "failing p4 map cases: {failed:?}");
}

#[test]
fn table_rows_have_tests_and_docs() {
    let w = W::new();
    let list = w.vcs(&w.a, "alice", &["p4", "--list"]);
    let names: BTreeSet<&str> = CASES.iter().map(|c| c.0).collect();
    let doc = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/p4-to-vcs.md")).unwrap();
    let mut rows = 0;
    for line in list.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        assert_eq!(f.len(), 4, "{line}");
        assert!(names.contains(f[3]), "row {:?} names unknown test {:?}", f[0], f[3]);
        assert!(doc.contains(f[0]), "docs/p4-to-vcs.md lacks the row for `{}`", f[0]);
        assert!(["exact", "renamed", "different-semantics", "missing"].contains(&f[2]));
        rows += 1;
    }
    assert!(rows >= 45, "{rows}");
}

#[test]
fn explain_prints_the_translation_and_runs_nothing() {
    let w = W::new();
    let o = w.p4(&["--explain", "submit", "-d", "msg"]);
    assert!(o.contains("vcs commit -m msg") && o.contains("vcs publish"), "{o}");
    let o = w.p4(&["--explain", "changes", "-m", "3", "-u", "bob"]);
    assert!(o.contains("vcs log -n 3 --author bob --oneline"), "{o}");
    let o = w.p4(&["--explain", "delete", "b.txt"]);
    assert!(o.contains("vcs rm"), "{o}");
    assert!(w.a.join("b.txt").exists());
    let o = w.p4(&["-d", w.b.to_str().unwrap(), "--explain", "where", "a.txt"]);
    assert!(o.contains("print"), "{o}");
}
