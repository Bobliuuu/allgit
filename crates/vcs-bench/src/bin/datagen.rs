//! usage: datagen <flat|deep|bigdir> <n> <out-dir> [seed]
use std::path::Path;
use vcs_bench::{generate, Layout};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: datagen <flat|deep|bigdir> <n> <out-dir> [seed]");
        std::process::exit(2);
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
