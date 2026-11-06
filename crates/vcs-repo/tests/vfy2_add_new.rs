use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use vcs_objects::Signature;
use vcs_repo::Repo;

fn sig() -> Signature { Signature { name: "T".into(), email: "t@x".into(), timestamp: 1000 } }
fn tracked(r: &Repo) -> BTreeSet<String> { r.tracked_files().unwrap().into_iter().map(|t| t.0).collect() }

fn expect_err(t: &BTreeSet<String>, p: &str) -> bool {
    // file/dir conflicts
    for q in t {
        if p.starts_with(&format!("{q}/")) || q.starts_with(&format!("{p}/")) { return true; }
    }
    // ascii fold collisions among all prefixes (files and dirs)
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let mut check = |x: &str| -> bool {
        match seen.get(&x.to_lowercase()) { Some(o) if o != x => true, _ => { seen.insert(x.to_lowercase(), x.to_string()); false } }
    };
    for q in t.iter().map(String::as_str).chain([p]) {
        let mut acc = String::new();
        for c in q.split('/') {
            if !acc.is_empty() { acc.push('/'); }
            acc.push_str(c);
            if check(&acc) { return true; }
        }
    }
    false
}

#[test]
fn add_single_new_file_matches_bruteforce_model() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let mut mk = |p: &str| { let a = r.worktree().join(p); fs::create_dir_all(a.parent().unwrap()).unwrap(); fs::write(a, "x\n").unwrap(); };
    for k in 0..3000u32 { mk(&format!("Dir{:02}/Sub{}/f{k:05}.txt", k % 20, k % 3)); }
    for p in ["lib", "Top.txt", "Mixed/Case/File.rs", "zeta/y", "alpha/Beta/gamma"] { mk(p); }
    r.add(&[String::new()]).unwrap();
    r.commit("base", sig()).unwrap();
    let mut seed = 99u64;
    let mut next = move |n: usize| { seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (seed >> 33) as usize % n };
    let comps = ["Dir03", "dir03", "DIR03", "Sub0", "sub0", "Sub9", "f00003.txt", "F00003.TXT", "new", "NEW", "lib", "LIB", "Lib", "Top.txt", "top.txt", "mixed", "Mixed", "case", "Case", "File.rs", "file.rs", "zeta", "Zeta", "y", "Y", "alpha", "ALPHA", "beta", "Beta", "gamma", "Gamma", "q"];
    let (mut oks, mut errs) = (0, 0);
    for _ in 0..400 {
        let depth = 1 + next(4);
        let p: Vec<&str> = (0..depth).map(|_| comps[next(comps.len())]).collect();
        let p = p.join("/");
        let t = tracked(&r);
        if t.contains(&p) { continue; }
        // skip candidates the filesystem itself can't hold (file where dir exists on disk)
        let abs = r.worktree().join(&p);
        if abs.exists() || abs.ancestors().skip(1).take_while(|a| *a != r.worktree()).any(|a| a.is_file()) { continue; }
        let Ok(()) = fs::create_dir_all(abs.parent().unwrap()).and_then(|_| fs::write(&abs, "n\n")) else { continue };
        let want_err = expect_err(&t, &p);
        let got = r.add(&[p.clone()]);
        match (&got, want_err) {
            (Ok(_), false) => { oks += 1; assert!(tracked(&r).contains(&p), "{p} not tracked"); r.rm(&[p.clone()], true, false).unwrap(); }
            (Err(_), true) => { errs += 1; assert!(!tracked(&r).contains(&p)); }
            (Ok(_), true) => panic!("accepted {p} but model says collision/conflict"),
            (Err(e), false) => panic!("rejected {p} but model says fine: {e}"),
        }
        let _ = fs::remove_file(&abs);
        // remove emptied dirs we created
        let mut a = abs.parent().map(|x| x.to_path_buf());
        while let Some(x) = a { if x == r.worktree() || fs::remove_dir(&x).is_err() { break; } a = x.parent().map(|y| y.to_path_buf()); }
        assert_eq!(tracked(&r), t, "tracked set changed after {p}");
    }
    eprintln!("oks={oks} errs={errs}");
    assert!(oks > 20 && errs > 20);
    assert!(r.fsck().unwrap().problems.is_empty());
}
