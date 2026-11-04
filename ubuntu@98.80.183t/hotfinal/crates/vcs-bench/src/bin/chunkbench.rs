//! usage: chunkbench <files> <size-bytes> <versions> <edits-per-file> <edit-len> <overwrite|insert>
//! Measures how many bytes content-defined chunks would store vs whole blobs across all versions.
use std::collections::HashSet;
use std::time::Instant;
use vcs_bench::{generate_binary, mutate_binary, mutate_binary_insert};
use vcs_store::chunk::{chunk_ends, ChunkParams};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 6 {
        eprintln!("usage: chunkbench <files> <size-bytes> <versions> <edits-per-file> <edit-len> <overwrite|insert>");
        std::process::exit(2);
    }
    let n: Vec<usize> = args[..5].iter().map(|s| s.parse().expect("number")).collect();
    let (files, size, versions, edits, elen) = (n[0], n[1], n[2], n[3], n[4]);
    let insert = args[5] == "insert";
    let t = tempfile::tempdir().unwrap();
    generate_binary(t.path(), files, size, 42).unwrap();
    println!(
        "files={files} size={size} versions={versions} edits/file={edits} edit_len={elen} mode={}",
        args[5]
    );
    for (min, avg, max) in [(2048, 8192, 32768), (8192, 32768, 131072)] {
        let p = ChunkParams::new(min, avg, max);
        let _ = std::fs::remove_dir_all(t.path());
        generate_binary(t.path(), files, size, 42).unwrap();
        let (mut whole, mut seen_whole) = (HashSet::new(), 0u64);
        let (mut seen, mut chunk_bytes, mut nchunks, mut total) = (HashSet::new(), 0u64, 0u64, 0u64);
        let mut chunk_time = std::time::Duration::ZERO;
        for v in 0..versions {
            if v > 0 {
                let seed = 1000 + v as u64;
                if insert {
                    mutate_binary_insert(t.path(), files, edits, elen, seed).unwrap();
                } else {
                    mutate_binary(t.path(), files, edits, elen, seed).unwrap();
                }
            }
            for i in 0..files {
                let b = std::fs::read(t.path().join(format!("bin/f{i:06}.bin"))).unwrap();
                total += b.len() as u64;
                if whole.insert(*blake3::hash(&b).as_bytes()) {
                    seen_whole += b.len() as u64;
                }
                let s = Instant::now();
                let ends = chunk_ends(&b, p);
                chunk_time += s.elapsed();
                let mut st = 0;
                for e in ends {
                    nchunks += 1;
                    if seen.insert(*blake3::hash(&b[st..e]).as_bytes()) {
                        chunk_bytes += (e - st) as u64;
                    }
                    st = e;
                }
            }
        }
        println!(
            "chunks min/avg/max={min}/{avg}/{max}: logical {total} B, whole-blob stored {seen_whole} B, chunk stored {chunk_bytes} B ({:.1}% of whole-blob), {nchunks} chunks, mean {} B, chunking time {:?}",
            100.0 * chunk_bytes as f64 / seen_whole as f64,
            total / nchunks.max(1),
            chunk_time
        );
    }
}
