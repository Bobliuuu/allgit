//! Randomized clone equivalence: a clone equals a fresh checkout of the same heads. For random
//! zoned histories published in several rounds (so several packs, with edits, deletions, exec bits,
//! empty and large chunked files), a full clone has exactly the working tree of the origin (names,
//! bytes, exec bits), a clean status and the origin's heads; a zone-filtered clone has exactly the
//! chosen zones' files; and the number of requests does not grow with the number of zones or files.
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use vcs_objects::Signature;
use vcs_remote::{Backend, CountingBackend, FsBackend, MemBackend};
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
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next() >> 20) as u8).collect()
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

fn write_file(root: &Path, p: &str, body: &[u8], exec: bool) {
    let abs = root.join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(&abs, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&abs, fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 })).unwrap();
    }
    let _ = exec;
}

fn run(seed: u64) {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let d = tempfile::tempdir().unwrap();
    let o = Repo::init(&d.path().join("origin")).unwrap();
    let root = o.worktree().to_path_buf();
    let nz = 2 + rng.below(7);
    let zones: Vec<String> = (0..nz).map(|i| format!("zone{i}")).collect();
    for z in &zones {
        o.create_zone(z, z).unwrap();
    }
    let backend: Box<dyn Backend> = if seed.is_multiple_of(2) {
        Box::new(MemBackend::new())
    } else {
        Box::new(FsBackend::open(d.path().join("remote")).unwrap())
    };
    let mut packs = 0usize;
    let mut live: Vec<String> = vec![];
    for round in 0..1 + rng.below(4) {
        for _ in 0..3 + rng.below(25) {
            match rng.below(8) {
                0 | 1 if !live.is_empty() => {
                    let k = live.swap_remove(rng.below(live.len()));
                    fs::remove_file(root.join(&k)).unwrap();
                }
                _ => {
                    let z = if rng.below(6) == 0 { None } else { Some(&zones[rng.below(nz)]) };
                    let p = format!(
                        "{}{}d{}/f{}.{}",
                        z.map_or(String::new(), |z| format!("{z}/")),
                        if rng.below(3) == 0 { "x/" } else { "" },
                        rng.below(3),
                        rng.below(10),
                        ["rs", "bin", "txt"][rng.below(3)]
                    );
                    if live
                        .iter()
                        .any(|k| k.starts_with(&format!("{p}/")) || p.starts_with(&format!("{k}/")))
                    {
                        continue;
                    }
                    let n = match rng.below(40) {
                        0 => 0,
                        1 if round == 0 => (1 << 20) + 1000 + rng.below(1 << 19), // chunked
                        2..=9 => rng.below(70_000),
                        _ => rng.below(600),
                    };
                    let body = rng.bytes(n);
                    write_file(&root, &p, &body, rng.below(5) == 0);
                    if !live.contains(&p) {
                        live.push(p);
                    }
                }
            }
        }
        o.add(&["".into()]).unwrap();
        let _ = o.commit(&format!("round {round}"), sig());
        if rng.below(3) != 0 {
            o.publish(&*backend, None, sig()).unwrap();
            packs += 1;
        }
    }
    o.publish(&*backend, None, sig()).unwrap();
    packs += 1;
    let want = snapshot(&root);

    // full clone == the origin's tree
    let counted = CountingBackend::new(&*backend);
    let c = Repo::clone_from(&counted, "mem", &d.path().join("full"), None).unwrap();
    let got = snapshot(c.worktree());
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        want.keys().collect::<Vec<_>>(),
        "seed {seed}: file set differs"
    );
    for (k, v) in &want {
        assert_eq!(&got[k], v, "seed {seed}: {k} differs");
    }
    assert_clean(&c, &format!("seed {seed} full"));
    let reqs = counted.stats().requests;
    assert!(
        reqs <= 20 + 6 * packs as u64,
        "seed {seed}: full clone made {reqs} requests for {nz} zones, {packs} packs"
    );
    for z in std::iter::once("root".to_string()).chain(zones.iter().cloned()) {
        assert_eq!(
            c.refs().unwrap().head(&z, "main"),
            o.refs().unwrap().head(&z, "main"),
            "seed {seed}: head of {z}"
        );
    }

    // zone-filtered clone == exactly the chosen zones' files
    let pick: Vec<String> = zones
        .iter()
        .filter(|z| o.refs().unwrap().head(z, "main").is_some() && rng.below(2) == 0)
        .cloned()
        .collect();
    if !pick.is_empty() {
        let p = Repo::clone_from(&*backend, "mem", &d.path().join("part"), Some(&pick)).unwrap();
        let got = snapshot(p.worktree());
        let in_pick = |k: &str| pick.iter().any(|z| k.starts_with(&format!("{z}/")));
        let exp: Files = want
            .iter()
            .filter(|(k, _)| in_pick(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let got_pick: Files = got
            .iter()
            .filter(|(k, _)| in_pick(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        assert_eq!(got_pick, exp, "seed {seed}: filtered clone differs in the chosen zones");
        assert!(
            got.keys().all(|k| in_pick(k) || !k.contains('/')),
            "seed {seed}: file of an unchosen zone present"
        );
        assert_clean(&p, &format!("seed {seed} part"));
    }
}

#[test]
fn clone_equals_fresh_checkout_of_the_same_heads() {
    for seed in 1..=24 {
        run(seed);
    }
}
