//! usage: datagen <flat|deep|bigdir|binary> <n> <out-dir> [seed]
use std::path::Path;
use vcs_bench::{generate, generate_binary_flat, Layout};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: datagen <flat|deep|bigdir|binary> <n> <out-dir> [seed]  (binary: 100 per dir, 4 KiB random each)");
        std::process::exit(2);
    }
    if a[1] == "binary" {
        let n: usize = a[2].parse().expect("n");
        let seed: u64 = a.get(4).map(|s| s.parse().expect("seed")).unwrap_or(42);
        std::fs::create_dir_all(&a[3]).expect("mkdir");
        generate_binary_flat(Path::new(&a[3]), n, 4096, seed).expect("generate");
        println!("generated {n} files (binary, 4096 B) in {}", a[3]);
        return;
    }
    let layout = Layout::parse(&a[1]).unwrap_or_else(|| {
        eprintln!("unknown layout {}", a[1]);
        std::process::exit(2)
    });
    let n: usize = a[2].parse().expect("n");
    let seed: u64 = a.get(4).map(|s| s.parse().expect("seed")).unwrap_or(42);
    generate(Path::new(&a[3]), layout, n, seed).expect("generate");
    println!("generated {n} files ({}) in {}", a[1], a[3]);
}
