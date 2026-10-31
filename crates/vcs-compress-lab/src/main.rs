//! vcs-compress-lab: research harness. Never linked into production binaries.
//! usage:
//!   compress-lab synth <ml|sparse|assets> <out-dir> <MiB> [seed]
//!   compress-lab <codecs|chunking|blocks|info> --name N --source SPEC --out FILE.json [--reps 3] [--cap-mb 64] [--window-mb 16]
//!                [--threads 8] [--stream-cap-mb 256] [--tmp DIR] [--order path|first-seen] [--max-blob-mb 512]
//!   SPEC: dir:PATH | dirrevs:PATH | git:PATH[:NREV[:SUBPATH]] | list:FILE
mod codec;
mod corpus;
mod exp_blocks;
mod exp_chunk;
mod exp_codecs;
mod rabin;
mod synth;
mod sys;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 2 {
        eprintln!("see source header for usage");
        std::process::exit(2);
    }
    if a[1] == "synth" {
        let seed = a.get(5).map(|s| s.parse().unwrap()).unwrap_or(7);
        std::fs::create_dir_all(&a[3]).unwrap();
        synth::generate(&a[2], std::path::Path::new(&a[3]), a[4].parse().unwrap(), seed);
        return;
    }
    let flag = |k: &str| a.iter().position(|x| x == k).map(|i| a[i + 1].clone());
    let num = |k: &str, d: usize| flag(k).map(|v| v.parse().unwrap()).unwrap_or(d);
    let name = flag("--name").unwrap_or_else(|| "dataset".into());
    let spec = flag("--source").expect("--source");
    let out = flag("--out");
    let t = std::time::Instant::now();
    let c = corpus::load(&name, &spec, num("--max-blob-mb", 512) << 20);
    eprintln!("loaded in {:.1}s: {}", t.elapsed().as_secs_f64(), c.summary());
    let body = match a[1].as_str() {
        "info" => c.summary(),
        "codecs" => exp_codecs::run(&c, &exp_codecs::Opts { reps: num("--reps", 3), cap_bytes: num("--cap-mb", 64) << 20, max_file_window: num("--window-mb", 16) << 20 }),
        "chunking" => exp_chunk::run(&c, num("--reps", 3), num("--threads", 8)),
        "blocks" => {
            let tmp = flag("--tmp").unwrap_or_else(|| std::env::temp_dir().to_string_lossy().to_string());
            exp_blocks::run(&c, &exp_blocks::Opts { reps: num("--reps", 3), stream_cap: num("--stream-cap-mb", 256) << 20, tmp, order_by_path: flag("--order").as_deref() != Some("first-seen") })
        }
        x => panic!("unknown subcommand {x}"),
    };
    let mut body = body;
    body["machine"] = sys::machine_meta();
    body["source_spec"] = serde_json::json!(spec);
    let s = serde_json::to_string_pretty(&body).unwrap();
    match out {
        Some(p) => std::fs::write(p, s).unwrap(),
        None => println!("{s}"),
    }
}
