//! The lazy commit (tree walk over the mapped index image + cached trees) must produce exactly the
//! root tree a from-scratch build of the tracked files gives, on random trees and edit sequences,
//! and must write only the trees along the changed paths.
use std::collections::BTreeMap;
use std::fs;
use vcs_objects::{EntryMode, Object, ObjectId, Signature};
use vcs_repo::{treeops, Repo};

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn head_root(r: &Repo, rep: &vcs_repo::CommitReport) -> ObjectId {
    match r.store().get(&rep.commits[0].2).unwrap() {
        Object::Commit(c) => c.root,
        _ => panic!("not a commit"),
    }
}

fn expected(r: &Repo, model: &BTreeMap<String, String>) -> ObjectId {
    let ents: Vec<(String, ObjectId)> = model
        .iter()
        .map(|(p, c)| (p.clone(), r.store().put(&Object::Blob(c.clone().into_bytes())).unwrap()))
        .collect();
    treeops::build(r.store(), ents.iter().map(|(p, id)| (p.as_str(), EntryMode::File, *id))).unwrap()
}

fn path(rng: &mut Rng) -> String {
    let depth = 1 + rng.below(3);
    let mut p = String::new();
    for _ in 0..depth {
        p.push_str(&format!("d{}/", rng.below(3)));
    }
    p.push_str(&format!("f{}", rng.below(24)));
    p
}

#[test]
fn lazy_commit_root_equals_full_build_on_random_trees() {
    for seed in 1..=6u64 {
        let d = tempfile::tempdir().unwrap();
        let r = Repo::init(&d.path().join("r")).unwrap();
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut model: BTreeMap<String, String> = BTreeMap::new();
        for round in 0..14 {
            let edits = if round == 0 { 120 } else { 1 + rng.below(4) };
            let mut changed = false;
            for _ in 0..edits {
                let p = path(&mut rng);
                // a path that is a file must not also be a directory
                if model
                    .keys()
                    .any(|k| k.starts_with(&format!("{p}/")) || p.starts_with(&format!("{k}/")))
                {
                    continue;
                }
                let w = r.worktree().join(&p);
                if rng.below(4) == 0 && model.contains_key(&p) {
                    fs::remove_file(&w).unwrap();
                    model.remove(&p);
                } else {
                    let c = format!("{seed}-{round}-{}", rng.next());
                    fs::create_dir_all(w.parent().unwrap()).unwrap();
                    fs::write(&w, &c).unwrap();
                    model.insert(p, c);
                }
                changed = true;
            }
            if !changed {
                continue;
            }
            r.add(&["".into()]).unwrap();
            let before = r.counters().tree_puts;
            let rep = match r.commit("m", sig()) {
                Ok(x) => x,
                Err(vcs_repo::Error::NothingToCommit) => continue,
                Err(e) => panic!("{e}"),
            };
            let puts = r.counters().tree_puts - before;
            assert_eq!(head_root(&r, &rep), expected(&r, &model), "seed {seed} round {round}");
            if round > 0 {
                // at most 4 edits, each touching <= 4 directories on its path
                assert!(puts <= 4 * 4, "seed {seed} round {round}: {puts} trees written");
            }
        }
        assert!(r.fsck().unwrap().problems.is_empty());
    }
}

fn show(s: &vcs_repo::Status) -> String {
    let mut o = String::new();
    for z in &s.zones {
        o += &format!("{} {:?}\nS {:?}\nU {:?}\n", z.zone, z.head, z.staged, z.unstaged);
    }
    o += &format!(
        "untracked {:?}\nunsupported {:?}\nunsub {:?}\n",
        s.untracked, s.unsupported, s.unsubscribed
    );
    o
}

#[test]
fn lazy_status_equals_eager_status_on_random_trees() {
    for seed in 1..=6u64 {
        let d = tempfile::tempdir().unwrap();
        let r = Repo::init(&d.path().join("r")).unwrap();
        let mut rng = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03));
        for round in 0..10 {
            for _ in 0..(if round == 0 { 150 } else { 1 + rng.below(6) }) {
                let p = path(&mut rng);
                let w = r.worktree().join(&p);
                if w.is_dir() || w.ancestors().skip(1).take(4).any(|a| a.is_file()) {
                    continue;
                }
                match rng.below(5) {
                    0 if w.exists() => fs::remove_file(&w).unwrap(),
                    _ => {
                        fs::create_dir_all(w.parent().unwrap()).unwrap();
                        fs::write(&w, format!("{seed}-{round}-{}", rng.next())).unwrap();
                    }
                }
            }
            // sometimes stage, sometimes leave unstaged/untracked
            if rng.below(3) != 0 {
                let _ = r.add(&["".into()]);
            }
            if rng.below(3) == 0 {
                let _ = r.commit("m", sig());
            }
            let opts = || vcs_repo::StatusOpts {
                dir_cache: false,
                ..Default::default()
            };
            let (a, b) = (r.status_with(opts()).unwrap(), r.status_eager(opts()).unwrap());
            assert_eq!(show(&a), show(&b), "seed {seed} round {round}");
        }
    }
}
