//! usage: packbench <files> <size-bytes> <versions> <edits-per-file> <edit-len>
//! Builds a binary-heavy history in a temp repo, then measures loose vs packed size and fsck time.
use std::path::Path;
use std::time::Instant;
use vcs_bench::{generate_binary, mutate_binary};
use vcs_objects::Signature;
use vcs_repo::Repo;

fn disk(dir: &Path) -> (u64, u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let (mut apparent, mut alloc, mut files) = (0, 0, 0);
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let m = e.metadata().unwrap();
            if m.is_dir() {
                stack.push(e.path());
            } else {
                apparent += m.len();
                alloc += m.blocks() * 512;
                files += 1;
            }
        }
    }
    (apparent, alloc, files)
}

fn main() {
    let a: Vec<usize> = std::env::args().skip(1).map(|s| s.parse().expect("number")).collect();
    if a.len() != 5 {
        eprintln!("usage: packbench <files> <size-bytes> <versions> <edits-per-file> <edit-len>");
        std::process::exit(2);
    }
    let (n, size, versions, edits, elen) = (a[0], a[1], a[2], a[3], a[4]);
    let t = tempfile::tempdir().unwrap();
    generate_binary(t.path(), n, size, 42).unwrap();
    let repo = Repo::init(t.path()).unwrap();
    let sig = |ts| Signature {
        name: "B".into(),
        email: "b@example.invalid".into(),
        timestamp: ts,
    };
    let start = Instant::now();
    for v in 0..versions {
        if v > 0 {
            mutate_binary(t.path(), n, edits, elen, 1000 + v as u64).unwrap();
        }
        repo.add(&[String::new()]).unwrap();
        repo.commit(&format!("v{v}"), sig(v as u64)).unwrap();
    }
    println!("files={n} size={size} versions={versions} edits/file={edits} edit_len={elen}");
    println!("add+commit all versions: {:?}", start.elapsed());
    let objs = t.path().join(".vcs/objects");
    let (ap, al, f) = disk(&objs);
    println!("loose:  {f} files, apparent {ap} B, allocated {al} B");
    let s = Instant::now();
    let r = repo.fsck().unwrap();
    println!(
        "loose fsck: {:?} ({} objects, {} problems)",
        s.elapsed(),
        r.objects,
        r.problems.len()
    );
    let s = Instant::now();
    let packed = repo.repack().unwrap();
    println!("repack: {:?} ({packed} objects)", s.elapsed());
    let (ap, al, f) = disk(&objs);
    println!("packed: {f} files, apparent {ap} B, allocated {al} B");
    let s = Instant::now();
    let r = repo.fsck().unwrap();
    println!(
        "packed fsck: {:?} ({} objects, {} problems)",
        s.elapsed(),
        r.objects,
        r.problems.len()
    );
}
