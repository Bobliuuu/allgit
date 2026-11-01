//! Sequential round trips of the remote hot paths against an object store with injected latency
//! (EMULATED: an in-memory backend that sleeps per call). Asserts request counts and sequential
//! DEPTH (longest chain of calls where each starts after the previous ended), never wall time.
//! Run with `--nocapture` for the per-request timeline.
use std::fs;
use std::sync::Arc;
use std::time::Duration;
use vcs_objects::Signature;
use vcs_remote::timeline::TimelineBackend;
use vcs_remote::{MemBackend, MemoBackend};
use vcs_repo::Repo;

const LAT: Duration = Duration::from_millis(15);

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}
fn write(r: &Repo, p: &str, c: &str) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
}
fn commit_all(r: &Repo, m: &str) {
    r.add(&["".into()]).unwrap();
    r.commit(m, sig()).unwrap();
}
fn dump(what: &str, t: &TimelineBackend) {
    println!("{what}: requests={} depth={}", t.requests(), t.depth());
    for e in t.events() {
        println!("    {:>7}..{:>7} us  {} {}", e.start_us, e.end_us, e.op, e.key);
    }
}

struct Fx {
    mem: Arc<MemBackend>,
    tl: Arc<TimelineBackend>,
    a: Repo,
    _d: tempfile::TempDir,
}

fn fixture() -> Fx {
    let d = tempfile::tempdir().unwrap();
    let mem = Arc::new(MemBackend::new());
    let a = Repo::init(&d.path().join("a")).unwrap();
    for z in ["engine", "assets", "docs", "tools"] {
        a.create_zone(z, z).unwrap();
    }
    for i in 0..12 {
        for z in ["engine", "assets", "docs", "tools"] {
            write(&a, &format!("{z}/f{i}.txt"), &format!("{z} {i}"));
        }
    }
    commit_all(&a, "init");
    for i in 0..6 {
        write(&a, "engine/f0.txt", &format!("v{i}"));
        commit_all(&a, "edit");
    }
    a.publish(mem.as_ref(), None, sig()).unwrap();
    let tl = Arc::new(TimelineBackend::new(mem.clone(), LAT));
    Fx { mem, tl, a, _d: d }
}

/// What the CLI does for `vcs clone`: memo with snapshot, primed, then clone_from.
fn cli_clone(tl: &Arc<TimelineBackend>, dest: &std::path::Path, zones: Option<&[String]>) -> Repo {
    let b = MemoBackend::new(Box::new(tl.clone()), true);
    b.prime();
    Repo::clone_from(&b, "mem", dest, zones).unwrap()
}

#[test]
fn clone_round_trips() {
    let f = fixture();
    let dest = f._d.path().join("c");
    let c = cli_clone(&f.tl, &dest, None);
    dump("clone", &f.tl);
    let (n, d) = (f.tl.requests(), f.tl.depth());
    assert!(n <= 8, "clone requests {n}");
    assert!(d <= 6, "clone depth {d}");
    // equivalence: same heads and same files as a clone straight from the backend
    let plain = Repo::clone_from(f.mem.as_ref(), "mem", &f._d.path().join("p"), None).unwrap();
    assert_eq!(c.refs().unwrap().encode(), plain.refs().unwrap().encode());
    assert_eq!(c.remote_refs().unwrap().encode(), plain.remote_refs().unwrap().encode());
    for i in 0..12 {
        for z in ["engine", "assets", "docs", "tools"] {
            let p = format!("{z}/f{i}.txt");
            assert_eq!(
                fs::read(c.worktree().join(&p)).unwrap(),
                fs::read(plain.worktree().join(&p)).unwrap()
            );
        }
    }
}

#[test]
fn zone_clone_round_trips() {
    let f = fixture();
    let want = ["engine".to_string()];
    let c = cli_clone(&f.tl, &f._d.path().join("c"), Some(&want));
    dump("zone clone", &f.tl);
    assert!(f.tl.requests() <= 8, "zone clone requests {}", f.tl.requests());
    assert!(f.tl.depth() <= 7, "zone clone depth {}", f.tl.depth());
    assert_eq!(fs::read_to_string(c.worktree().join("engine/f0.txt")).unwrap(), "v5");
}

#[test]
fn noop_fetch_is_one_request() {
    let f = fixture();
    let c = cli_clone(&f.tl, &f._d.path().join("c"), None);
    c.fetch(f.mem.as_ref(), None).unwrap(); // warms the manifest cache like a first fetch
    f.tl.clear();
    c.fetch(&f.tl, None).unwrap();
    dump("no-op fetch", &f.tl);
    assert_eq!((f.tl.requests(), f.tl.depth()), (1, 1));
}

#[test]
fn fetch_one_commit_round_trips() {
    let f = fixture();
    let c = cli_clone(&f.tl, &f._d.path().join("c"), None);
    c.fetch(f.mem.as_ref(), None).unwrap();
    write(&f.a, "engine/f0.txt", "new");
    commit_all(&f.a, "n");
    f.a.publish(f.mem.as_ref(), None, sig()).unwrap();
    f.tl.clear();
    c.fetch(&f.tl, None).unwrap();
    dump("fetch one commit", &f.tl);
    assert!(f.tl.requests() <= 5, "fetch requests {}", f.tl.requests());
    assert!(f.tl.depth() <= 5, "fetch depth {}", f.tl.depth());
    assert_eq!(
        c.remote_refs().unwrap().head("engine", "main"),
        f.a.refs().unwrap().head("engine", "main")
    );
}

#[test]
fn publish_round_trips() {
    let f = fixture();
    write(&f.a, "engine/f1.txt", "changed");
    commit_all(&f.a, "edit");
    f.a.publish(&f.tl, None, sig()).unwrap();
    dump("publish", &f.tl);
    assert!(f.tl.requests() <= 12, "publish requests {}", f.tl.requests());
    assert!(f.tl.depth() <= 12, "publish depth {}", f.tl.depth());
}
