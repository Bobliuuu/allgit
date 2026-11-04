//! Prototype measurements for docs/research/10x-opportunities.md (not shipped behaviour).
//!
//!   snapbench clone  SRC DST          time clonefile(2) / FICLONE of a whole materialized tree
//!   snapbench restamp DIR [THREADS]   lstat every file (what re-stamping a cloned tree's index costs)
//!   snapbench create N DIR MODE T     create N 4 KiB files: MODE = tmp (tmp+rename, as RealFs::write) | direct (O_EXCL at final path)
//!   snapbench mmap FILE               mmap a fixed-width record file; time 100k random lookups vs full read+parse
use std::ffi::CString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn walk(d: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(d).unwrap().flatten() {
        let p = e.path();
        if e.file_type().unwrap().is_dir() {
            walk(&p, out)
        } else {
            out.push(p)
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a[1].as_str() {
        "clone" => {
            let (s, d) = (CString::new(a[2].clone()).unwrap(), CString::new(a[3].clone()).unwrap());
            let t = Instant::now();
            #[cfg(target_os = "macos")]
            let rc = unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), 0) };
            #[cfg(not(target_os = "macos"))]
            let rc = {
                let _ = (&s, &d);
                -1
            };
            println!("clonefile rc={rc} {:.3} s", t.elapsed().as_secs_f64());
        }
        "restamp" => {
            let th: usize = a.get(3).map(|x| x.parse().unwrap()).unwrap_or(1);
            let t = Instant::now();
            let mut v = vec![];
            walk(Path::new(&a[2]), &mut v);
            let tw = t.elapsed().as_secs_f64();
            let t = Instant::now();
            let n = std::sync::atomic::AtomicU64::new(0);
            std::thread::scope(|s| {
                for c in v.chunks(v.len().div_ceil(th)) {
                    let n = &n;
                    s.spawn(move || {
                        let mut x = 0u64;
                        for p in c {
                            let m = fs::symlink_metadata(p).unwrap();
                            use std::os::unix::fs::MetadataExt;
                            x = x.wrapping_add(m.ctime_nsec() as u64 + m.size());
                        }
                        n.fetch_add(x, std::sync::atomic::Ordering::Relaxed);
                    });
                }
            });
            println!(
                "files={} walk={:.3}s lstat x{th} threads={:.3}s",
                v.len(),
                tw,
                t.elapsed().as_secs_f64()
            );
        }
        "create" => {
            let n: usize = a[2].parse().unwrap();
            let root = PathBuf::from(&a[3]);
            let direct = a[4] == "direct";
            let th: usize = a[5].parse().unwrap();
            let data = vec![b'x'; 4096];
            let t = Instant::now();
            std::thread::scope(|s| {
                for k in 0..th {
                    let (root, data) = (&root, &data);
                    s.spawn(move || {
                        for i in (k..n).step_by(th) {
                            let dir = root.join(format!("d{:04}", i % 1000));
                            let fin = dir.join(format!("f{i}.txt"));
                            let mk = |p: &Path| match fs::OpenOptions::new().write(true).create_new(true).open(p) {
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                    fs::create_dir_all(p.parent().unwrap()).unwrap();
                                    fs::OpenOptions::new().write(true).create_new(true).open(p).unwrap()
                                }
                                r => r.unwrap(),
                            };
                            if direct {
                                let mut f = mk(&fin);
                                f.write_all(data).unwrap();
                                let _ = f.metadata().unwrap(); // fstat instead of path stat
                            } else {
                                let tmp = dir.join(format!(".tmp{i}"));
                                let mut f = mk(&tmp);
                                f.write_all(data).unwrap();
                                drop(f);
                                fs::rename(&tmp, &fin).unwrap();
                                let _ = fs::symlink_metadata(&fin).unwrap(); // path stat afterwards
                            }
                        }
                    });
                }
            });
            println!("create n={n} mode={} threads={th}: {:.3} s", a[4], t.elapsed().as_secs_f64());
        }
        "mmap" => {
            // 100k records of 64 bytes (32 B id + size + mtime + ctime + mode), sorted by path hash.
            let n = 100_000usize;
            let mut buf = Vec::with_capacity(n * 64);
            for i in 0..n {
                let mut r = [0u8; 64];
                r[..8].copy_from_slice(&(i as u64).to_be_bytes());
                r[32..40].copy_from_slice(&(600 + i as u64).to_le_bytes());
                buf.extend_from_slice(&r);
            }
            fs::write(&a[2], &buf).unwrap();
            let probes: Vec<usize> = (0..1000).map(|k| (k * 7919) % n).collect();
            let t = Instant::now();
            let b = fs::read(&a[2]).unwrap();
            let mut recs: Vec<([u8; 8], u64)> = Vec::with_capacity(n); // "decode all"
            for c in b.chunks_exact(64) {
                recs.push((c[..8].try_into().unwrap(), u64::from_le_bytes(c[32..40].try_into().unwrap())));
            }
            let mut s = 0;
            for &p in &probes {
                s += recs[p].1;
            }
            println!("read+decode-all+1000 lookups: {:.3} ms (sum {s})", t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            let f = fs::File::open(&a[2]).unwrap();
            use std::os::unix::io::AsRawFd;
            let p = unsafe { libc::mmap(std::ptr::null_mut(), n * 64, libc::PROT_READ, libc::MAP_PRIVATE, f.as_raw_fd(), 0) } as *const u8;
            let sl = unsafe { std::slice::from_raw_parts(p, n * 64) };
            let mut s = 0;
            for &p in &probes {
                s += u64::from_le_bytes(sl[p * 64 + 32..p * 64 + 40].try_into().unwrap());
            }
            println!(
                "mmap + 1000 lookups (zero-copy): {:.3} ms (sum {s})",
                t.elapsed().as_secs_f64() * 1e3
            );
        }
        _ => eprintln!("see header"),
    }
}
