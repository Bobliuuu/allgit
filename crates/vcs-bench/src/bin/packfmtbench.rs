//! Pack format v1 vs v2 (#245, #246).
//!   packfmtbench size <name> <dir>      import <dir> into a repo, report bytes loose / v1 / v2
//!   packfmtbench binary                 same on 100 x 256 KiB random files, 4 versions (packbench dataset)
//!   packfmtbench lookup <objects> <packs>  miss/hit lookup and open time across <packs> synthetic packs
use std::path::Path;
use std::time::Instant;
use vcs_objects::{ObjectId, Signature};
use vcs_repo::Repo;
use vcs_store::pack::{write_pack_fmt, Pack, PackFormat};

fn sizes(dir: &Path) -> (u64, u64) {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    let (mut apparent, mut alloc) = (0, 0);
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let m = e.metadata().unwrap();
            if m.is_dir() {
                stack.push(e.path());
            } else {
                apparent += m.len();
                #[cfg(unix)]
                {
                    alloc += m.blocks() * 512;
                }
                #[cfg(not(unix))]
                {
                    alloc += m.len();
                }
            }
        }
    }
    (apparent, alloc)
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let name = e.file_name();
        if name == "target" || name == ".git" || name == ".vcs" {
            continue;
        }
        let p = e.path();
        if e.file_type().unwrap().is_dir() {
            copy_tree(&p, &to.join(&name));
        } else {
            std::fs::copy(&p, to.join(&name)).unwrap();
        }
    }
}

fn report(repo: &Repo, objs: &Path) {
    let sig = |ts| Signature {
        name: "B".into(),
        email: "b@example.invalid".into(),
        timestamp: ts,
    };
    let _ = sig;
    let (ap, al) = sizes(objs);
    println!("loose: apparent {ap} B, allocated {al} B");
    let n = repo.repack_as(PackFormat::V1).unwrap();
    let (ap1, al1) = sizes(&objs.join("pack"));
    let t = Instant::now();
    let r = repo.fsck().unwrap();
    println!(
        "v1 pack+idx: apparent {ap1} B, allocated {al1} B ({} objects; fsck {:?}, {} problems)",
        r.objects,
        t.elapsed(),
        r.problems.len()
    );
    let t = Instant::now();
    let c = repo.repack_as(PackFormat::V2).unwrap();
    println!("convert v1->v2: {:?} ({} packs)", t.elapsed(), c.1);
    let (ap2, al2) = sizes(&objs.join("pack"));
    let t = Instant::now();
    let r = repo.fsck().unwrap();
    println!(
        "v2 pack+idx: apparent {ap2} B, allocated {al2} B (fsck {:?}, {} problems)",
        t.elapsed(),
        r.problems.len()
    );
    println!(
        "v2/v1 apparent {:.3}, v2/loose apparent {:.3}  [first repack packed {} loose]",
        ap2 as f64 / ap1 as f64,
        ap2 as f64 / ap as f64,
        n.0
    );
}

fn size_mode(src: &Path) {
    let t = tempfile::tempdir().unwrap();
    copy_tree(src, t.path());
    let repo = Repo::init(t.path()).unwrap();
    repo.add(&[String::new()]).unwrap();
    repo.commit(
        "import",
        Signature {
            name: "B".into(),
            email: "b@example.invalid".into(),
            timestamp: 1,
        },
    )
    .unwrap();
    report(&repo, &t.path().join(".vcs/objects"));
}

fn lookup_mode(total: usize, packs: usize) {
    let per = total / packs;
    let t = tempfile::tempdir().unwrap();
    let mut ids_all: Vec<ObjectId> = Vec::with_capacity(total);
    for fmt in [PackFormat::V1, PackFormat::V2] {
        let dir = t.path().join(format!("{fmt:?}"));
        let mut idx_paths = Vec::new();
        ids_all.clear();
        for p in 0..packs {
            let objs: Vec<_> = (0..per)
                .map(|i| {
                    let id = ObjectId::from_bytes(*blake3::hash(&((p * per + i) as u64).to_le_bytes()).as_bytes());
                    ids_all.push(id);
                    (id, ((p * per + i) as u64).to_le_bytes().to_vec())
                })
                .collect();
            let pk = write_pack_fmt(&dir, t.path(), objs, fmt).unwrap();
            idx_paths.push(pk.pack_path.with_extension("idx"));
        }
        let t0 = Instant::now();
        let opened: Vec<Pack> = idx_paths.iter().map(|p| Pack::open(p).unwrap()).collect();
        let open = t0.elapsed();
        let misses: Vec<ObjectId> = (0..20_000u64)
            .map(|i| ObjectId::from_bytes(*blake3::hash(&(i + (1 << 40)).to_le_bytes()).as_bytes()))
            .collect();
        let t0 = Instant::now();
        let mut found = 0usize;
        for m in &misses {
            found += opened.iter().filter(|p| p.contains(m)).count();
        }
        let miss = t0.elapsed();
        assert_eq!(found, 0);
        let hits: Vec<ObjectId> = ids_all.iter().step_by((total / 20_000).max(1)).copied().collect();
        let t0 = Instant::now();
        let mut found = 0usize;
        for h in &hits {
            found += opened.iter().any(|p| p.contains(h)) as usize;
        }
        let hit = t0.elapsed();
        assert_eq!(found, hits.len());
        println!(
            "{fmt:?}: {total} objects / {packs} packs: open all {open:?}; miss across all packs {:.2} us/lookup; hit {:.2} us/lookup",
            miss.as_secs_f64() * 1e6 / misses.len() as f64,
            hit.as_secs_f64() * 1e6 / hits.len() as f64
        );
        drop(opened);
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["size", name, dir] => {
            println!("== {name} ({dir})");
            size_mode(Path::new(dir));
        }
        ["binary"] => {
            println!("== binary generator (100 x 256 KiB, 4 versions)");
            let t = tempfile::tempdir().unwrap();
            vcs_bench::generate_binary(t.path(), 100, 262144, 42).unwrap();
            let repo = Repo::init(t.path()).unwrap();
            for v in 0..4u64 {
                if v > 0 {
                    vcs_bench::mutate_binary(t.path(), 100, 4, 4096, 1000 + v).unwrap();
                }
                repo.add(&[String::new()]).unwrap();
                repo.commit(
                    &format!("v{v}"),
                    Signature {
                        name: "B".into(),
                        email: "b@example.invalid".into(),
                        timestamp: v,
                    },
                )
                .unwrap();
            }
            report(&repo, &t.path().join(".vcs/objects"));
        }
        ["lookup", n, p] => lookup_mode(n.parse().unwrap(), p.parse().unwrap()),
        _ => {
            eprintln!("usage: packfmtbench size <name> <dir> | binary | lookup <objects> <packs>");
            std::process::exit(2);
        }
    }
}
