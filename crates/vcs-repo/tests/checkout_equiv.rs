//! Randomized checkout equivalence: after any sequence of branch switches the working tree holds
//! exactly the files (and bytes, and exec bits) the target commit names, status is clean, and
//! unsafe switches (dirty tracked file, untracked file in the way) are refused without touching anything.
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use vcs_objects::Signature;
use vcs_repo::Repo;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}

type Files = BTreeMap<String, (Vec<u8>, bool)>;

fn snapshot(root: &Path) -> Files {
    fn go(root: &Path, dir: &Path, out: &mut Files) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if rel == ".vcs" {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                go(root, &p, out);
            } else {
                #[cfg(unix)]
                let exec = {
                    use std::os::unix::fs::PermissionsExt;
                    fs::metadata(&p).unwrap().permissions().mode() & 0o111 != 0
                };
                #[cfg(not(unix))]
                let exec = false;
                out.insert(rel, (fs::read(&p).unwrap(), exec));
            }
        }
    }
    let mut out = Files::new();
    go(root, root, &mut out);
    out
}

fn assert_clean(r: &Repo, ctx: &str) {
    let st = r.status().unwrap();
    for z in &st.zones {
        assert!(
            z.staged.is_empty() && z.unstaged.is_empty(),
            "{ctx}: not clean: {:?} {:?}",
            z.staged,
            z.unstaged
        );
    }
    assert!(st.untracked.is_empty(), "{ctx}: untracked {:?}", st.untracked);
}

fn random_path(rng: &mut Rng) -> String {
    let depth = 1 + rng.below(3);
    let mut parts: Vec<String> = (0..depth - 1).map(|_| format!("d{}", rng.below(4))).collect();
    parts.push(format!("f{}", rng.below(6)));
    parts.join("/")
}

fn mutate(root: &Path, rng: &mut Rng) {
    let cur = snapshot(root);
    for _ in 0..1 + rng.below(8) {
        match rng.below(4) {
            0 | 1 => {
                // write (maybe replacing a directory or a file's parent): skip if a path conflict
                let p = random_path(rng);
                let abs = root.join(&p);
                if cur
                    .keys()
                    .any(|k| k.starts_with(&format!("{p}/")) || p.starts_with(&format!("{k}/")))
                {
                    continue;
                }
                fs::create_dir_all(abs.parent().unwrap()).unwrap();
                let body: Vec<u8> = (0..rng.below(40)).map(|_| rng.next() as u8).collect();
                fs::write(&abs, &body).unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = if rng.below(4) == 0 { 0o755 } else { 0o644 };
                    fs::set_permissions(&abs, fs::Permissions::from_mode(mode)).unwrap();
                }
            }
            _ => {
                if let Some(k) = cur.keys().nth(rng.below(cur.len().max(1))) {
                    let _ = fs::remove_file(root.join(k));
                    let mut d = root.join(k);
                    while d.pop() && d != root {
                        if fs::remove_dir(&d).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    }
}

fn run(seed: u64) {
    let mut rng = Rng(seed | 1);
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("w")).unwrap();
    let root = r.worktree().to_path_buf();
    for _ in 0..12 {
        let p = random_path(&mut rng);
        if root.join(&p).exists()
            || snapshot(&root)
                .keys()
                .any(|k| p.starts_with(&format!("{k}/")) || k.starts_with(&format!("{p}/")))
        {
            continue;
        }
        fs::create_dir_all(root.join(&p).parent().unwrap()).unwrap();
        fs::write(root.join(&p), format!("base {p}")).unwrap();
    }
    fs::write(root.join("keep.txt"), "keep").unwrap();
    r.add(&["".into()]).unwrap();
    r.commit("base", sig()).unwrap();
    let mut expect: BTreeMap<String, Files> = BTreeMap::new();
    expect.insert("main".into(), snapshot(&root));
    for b in 0..5 {
        let name = format!("b{b}");
        r.create_branch("root", &name).unwrap();
        r.checkout("root", &name).unwrap();
        mutate(&root, &mut rng);
        r.add(&["".into()]).unwrap();
        let _ = r.commit(&format!("c{b}"), sig()); // "nothing to commit" leaves the branch equal to its parent
        expect.insert(name, snapshot(&root));
    }
    let names: Vec<String> = expect.keys().cloned().collect();
    let mut cur = "b4".to_string(); // the branch left checked out by the loop above
    for step in 0..40 {
        let target = names[rng.below(names.len())].clone();
        if target == cur {
            continue;
        }
        // Sometimes make the switch unsafe and check it is refused with the tree untouched.
        match rng.below(6) {
            0 => {
                if let Some(k) = expect[&cur].keys().next().cloned() {
                    fs::write(root.join(&k), b"dirty edit").unwrap();
                    let before = snapshot(&root);
                    assert!(
                        r.checkout("root", &target).is_err(),
                        "seed {seed} step {step}: dirty switch allowed"
                    );
                    assert_eq!(snapshot(&root), before);
                    let (body, exec) = expect[&cur][&k].clone();
                    fs::write(root.join(&k), &body).unwrap();
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(root.join(&k), fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 })).unwrap();
                    }
                    let _ = exec;
                }
            }
            1 => {
                // an untracked file where the target wants a file the current branch lacks
                let want: Vec<String> = expect[&target].keys().filter(|k| !expect[&cur].contains_key(*k)).cloned().collect();
                if let Some(k) = want.first() {
                    let abs = root.join(k);
                    if !abs.exists() && abs.parent().is_some_and(|p| p.is_dir() || fs::create_dir_all(p).is_ok()) {
                        fs::write(&abs, b"in the way").unwrap();
                        let before = snapshot(&root);
                        assert!(r.checkout("root", &target).is_err(), "seed {seed} step {step}: overwrite allowed");
                        assert_eq!(snapshot(&root), before);
                        fs::remove_file(&abs).unwrap();
                        let mut p = abs.clone();
                        while p.pop() && p != root {
                            if fs::remove_dir(&p).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        r.checkout("root", &target)
            .unwrap_or_else(|e| panic!("seed {seed} step {step}: {cur} -> {target}: {e}"));
        assert_eq!(snapshot(&root), expect[&target], "seed {seed} step {step}: {cur} -> {target}");
        assert_clean(&r, &format!("seed {seed} step {step}"));
        if step % 10 == 0 {
            assert!(r.fsck().unwrap().problems.is_empty());
        }
        cur = target;
    }
}

#[test]
fn random_branch_switches_materialize_exactly_the_target_tree() {
    for seed in 1..=12u64 {
        run(seed * 0x9E37_79B9);
    }
}

#[test]
fn switching_many_files_uses_the_parallel_writer_correctly() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("w")).unwrap();
    let root = r.worktree().to_path_buf();
    for i in 0..400 {
        fs::create_dir_all(root.join(format!("d{}", i % 20))).unwrap();
        fs::write(root.join(format!("d{}/f{i}", i % 20)), format!("v1 {i}")).unwrap();
    }
    r.add(&["".into()]).unwrap();
    r.commit("one", sig()).unwrap();
    r.create_branch("root", "two").unwrap();
    r.checkout("root", "two").unwrap();
    for i in 0..400 {
        if i % 3 == 0 {
            fs::write(root.join(format!("d{}/f{i}", i % 20)), format!("v2 {i}")).unwrap();
        } else if i % 3 == 1 {
            fs::remove_file(root.join(format!("d{}/f{i}", i % 20))).unwrap();
        }
        fs::write(root.join(format!("d{}/new{i}", i % 20)), format!("new {i}")).unwrap();
    }
    r.add(&["".into()]).unwrap();
    r.commit("two", sig()).unwrap();
    let two = snapshot(&root);
    r.checkout("root", "main").unwrap();
    assert_eq!(snapshot(&root).len(), 400);
    assert!(snapshot(&root)
        .iter()
        .all(|(k, (b, _))| b.starts_with(b"v1 ") && !k.contains("new")));
    assert_clean(&r, "back on main");
    r.checkout("root", "two").unwrap();
    assert_eq!(snapshot(&root), two);
    assert_clean(&r, "back on two");
}
