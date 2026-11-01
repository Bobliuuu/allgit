//! Adding and checking out a 1 GiB file stays within a fixed memory bound (Linux: VmHWM of this process).
#![cfg(target_os = "linux")]
use std::io::Write;
use vcs_objects::Signature;
use vcs_repo::Repo;

fn hwm_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    let l = s.lines().find(|l| l.starts_with("VmHWM:")).unwrap();
    l.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[test]
fn one_gib_file_add_status_and_checkout_stay_under_a_memory_bound() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("a")).unwrap();
    let path = r.worktree().join("big.bin");
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut buf = vec![0u8; 8 << 20];
        for _ in 0..128 {
            for b in buf.chunks_exact_mut(8) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                b.copy_from_slice(&x.to_le_bytes());
            }
            f.write_all(&buf).unwrap();
        }
    }
    let start = hwm_kib();
    r.add(&["".into()]).unwrap();
    let sig = Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1,
    };
    r.commit("m", sig).unwrap();
    let c1 = r.head_commit("root").unwrap().unwrap();
    assert!(r
        .status()
        .unwrap()
        .zones
        .iter()
        .all(|z| z.staged.is_empty() && z.unstaged.is_empty()));
    // change one chunk's worth, then go back to the first commit (streamed from chunks)
    {
        use std::os::unix::fs::FileExt;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .write_all_at(&[1, 2, 3, 4], 512 << 20)
            .unwrap();
    }
    assert!(r.status().unwrap().zones.iter().any(|z| !z.unstaged.is_empty()));
    r.add(&["".into()]).unwrap();
    r.commit(
        "edit",
        Signature {
            name: "T".into(),
            email: "t@x".into(),
            timestamp: 2,
        },
    )
    .unwrap();
    r.checkout_commit("root", "main", &c1).unwrap();
    let grown = (hwm_kib() - start.min(hwm_kib())) / 1024;
    // The file is 1024 MiB. Constant-memory paths hold a 64 MiB read window, the store's 256 MiB
    // pending budget and a few in-flight chunks; whole-file reassembly would exceed 1 GiB.
    assert!(grown < 600, "peak resident memory grew by {grown} MiB for a 1 GiB file");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 1 << 30);
}
