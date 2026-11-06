use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use vcs_objects::Signature;
use vcs_repo::Repo;

fn sig() -> Signature { Signature { name: "T".into(), email: "t@x".into(), timestamp: 1000 } }
fn write(r: &Repo, p: &str, c: &str) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
}
fn commit_all(r: &Repo, m: &str) { r.add(&["".into()]).unwrap(); r.commit(m, sig()).unwrap(); }
fn source(dir: &Path) -> Repo {
    let a = Repo::init(&dir.join("a")).unwrap();
    a.create_zone("engine", "engine").unwrap();
    write(&a, "README", "readme");
    for i in 0..30 { write(&a, &format!("engine/f{i}.rs"), &format!("content {i}")); }
    write(&a, "big.bin", &"B".repeat(80_000));
    commit_all(&a, "init");
    a
}
fn rehash(b: &mut Vec<u8>) { let n = b.len() - 32; let h = blake3::hash(&b[..n]); b[n..].copy_from_slice(h.as_bytes()); }
fn snap(dir: &Path) -> BTreeMap<PathBuf, u64> {
    fn go(d: &Path, root: &Path, m: &mut BTreeMap<PathBuf, u64>) {
        for e in fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            let md = e.metadata().unwrap();
            if md.is_dir() { go(&p, root, m) } else { m.insert(p.strip_prefix(root).unwrap().into(), md.len()); }
        }
    }
    let mut m = BTreeMap::new(); go(dir, dir, &mut m); m
}
fn hdr(good: &[u8]) -> (usize, usize) { let hl = u32::from_le_bytes(good[12..16].try_into().unwrap()) as usize; (16, hl) }

/// Every variant must be refused by clone and fetch, leaving no destination / no change.
fn variants(good: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut v = vec![];
    let (hs, hl) = hdr(good);
    let ps = hs + hl;
    // flipped byte, trailer not fixed
    for off in [ps + 20, ps + good.len() / 3 - ps / 3, good.len() - 40, 20] { let mut m = good.to_vec(); m[off] ^= 1; v.push((format!("flip{off}"), m)); }
    // flipped byte in pack, trailer fixed (pack checksum must catch)
    for off in [ps + 30, ps + (good.len() - ps) / 2, good.len() - 40] { let mut m = good.to_vec(); m[off] ^= 1; rehash(&mut m); v.push((format!("flip+rehash{off}"), m)); }
    // truncated
    for n in [good.len() - 1, good.len() - 33, good.len() / 2, ps + 5] { v.push((format!("trunc{n}"), good[..n].to_vec())); }
    // wrong declared head id (header says a different object), trailer fixed
    {
        let text = String::from_utf8(good[hs..hs + hl].to_vec()).unwrap();
        let line = text.lines().find(|l| l.starts_with("head\t")).unwrap();
        let hex = line.rsplit('\t').next().unwrap();
        let mut other = hex.to_string(); other.replace_range(0..1, if &hex[..1] == "0" { "1" } else { "0" });
        let nt = text.replace(hex, &other);
        let mut m = good.to_vec(); m[hs..hs + hl].copy_from_slice(nt.as_bytes()); rehash(&mut m);
        v.push(("wrong-head-id".into(), m));
    }
    // duplicate first object
    {
        let text = String::from_utf8(good[hs..hs + hl].to_vec()).unwrap();
        let pack = &good[ps..good.len() - 32];
        let n0 = u32::from_le_bytes(pack[12..16].try_into().unwrap()) as usize;
        let entry = &pack[12..12 + 4 + n0];
        let mut np = pack.to_vec();
        np.extend_from_slice(entry);
        let cnt = u32::from_le_bytes(np[8..12].try_into().unwrap()) + 1;
        np[8..12].copy_from_slice(&cnt.to_le_bytes());
        let ck = blake3::hash(&np); np.extend_from_slice(ck.as_bytes());
        let mut nt = String::new();
        for l in text.split_terminator('\n') {
            if let Some(x) = l.strip_prefix("pack-bytes\t") { let _ = x; nt += &format!("pack-bytes\t{}\n", np.len()); }
            else if let Some(x) = l.strip_prefix("objects\t") { nt += &format!("objects\t{}\n", x.parse::<usize>().unwrap() + 1); }
            else { nt += l; nt += "\n"; }
        }
        let mut m = good[..12].to_vec();
        m.extend_from_slice(&(nt.len() as u32).to_le_bytes());
        m.extend_from_slice(nt.as_bytes()); m.extend_from_slice(&np);
        m.extend_from_slice(&[0u8; 32]); rehash(&mut m);
        v.push(("duplicate-object".into(), m));
    }
    v
}

#[test]
fn hostile_bundles_rejected_by_clone_and_fetch_without_partial_import() {
    let d = tempfile::tempdir().unwrap();
    let a = source(d.path());
    let f = d.path().join("g.bundle");
    a.bundle_create(&f, &[], None).unwrap();
    let good = fs::read(&f).unwrap();
    // sanity: good clones
    Repo::clone_from_bundle(&f, &d.path().join("ok"), sig()).unwrap();
    let target = Repo::init(&d.path().join("t")).unwrap();
    let before = snap(&d.path().join("t"));
    for (name, m) in variants(&good) {
        let p = d.path().join("m.bundle");
        fs::write(&p, &m).unwrap();
        let dest = d.path().join(format!("c-{name}"));
        assert!(Repo::clone_from_bundle(&p, &dest, sig()).is_err(), "{name}: clone accepted");
        assert!(!dest.exists(), "{name}: partial clone dir left");
        // pre-existing empty dest stays empty
        let dest2 = d.path().join(format!("e-{name}")); fs::create_dir(&dest2).unwrap();
        assert!(Repo::clone_from_bundle(&p, &dest2, sig()).is_err(), "{name}: clone2 accepted");
        let left = snap(&dest2);
        assert!(left.is_empty(), "{name}: leftovers in pre-existing empty dest: {:?}", left.keys().collect::<Vec<_>>());
        assert!(target.bundle_fetch(&p, sig()).is_err(), "{name}: fetch accepted");
        assert_eq!(snap(&d.path().join("t")), before, "{name}: fetch changed repo");
    }
    assert!(target.fsck().unwrap().problems.is_empty());
}

#[test]
fn missing_prerequisite_no_partial_import() {
    let d = tempfile::tempdir().unwrap();
    let a = source(d.path());
    write(&a, "engine/f1.rs", "changed"); write(&a, "new.txt", "n");
    commit_all(&a, "second");
    let inc = d.path().join("inc.bundle");
    assert!(!a.bundle_create(&inc, &[], Some("HEAD~1")).unwrap().prerequisites.is_empty());
    assert!(Repo::clone_from_bundle(&inc, &d.path().join("c"), sig()).is_err());
    assert!(!d.path().join("c").exists());
    let t = Repo::init(&d.path().join("t")).unwrap();
    let before = snap(&d.path().join("t"));
    assert!(t.bundle_fetch(&inc, sig()).is_err());
    assert_eq!(snap(&d.path().join("t")), before);
}
