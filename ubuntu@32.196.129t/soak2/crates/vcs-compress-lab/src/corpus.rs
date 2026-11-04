//! Corpus loading: a corpus is an ordered list of revisions, each a list of (path, blob).
//! Blobs are deduplicated by content (blake3) so each unique content is held once.
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Class {
    SourceText,
    SmallBinary,
    LargeBinary,
    ModelCkpt,
}
impl Class {
    pub const ALL: [Class; 4] = [Class::SourceText, Class::SmallBinary, Class::LargeBinary, Class::ModelCkpt];
    pub fn name(&self) -> &'static str {
        match self {
            Class::SourceText => "source_text",
            Class::SmallBinary => "small_binary",
            Class::LargeBinary => "large_binary",
            Class::ModelCkpt => "model_ckpt",
        }
    }
}

pub struct Entry {
    pub path: String,
    pub blob: usize,
}
pub struct Blob {
    pub data: Vec<u8>,
    pub class: Class,
    pub path: String,
}
pub struct Corpus {
    pub name: String,
    pub revs: Vec<Vec<Entry>>,
    pub blobs: Vec<Blob>,
}

const TEXT_EXT: &[&str] = &[
    "rs", "c", "h", "cc", "cpp", "hpp", "py", "js", "ts", "tsx", "jsx", "go", "java", "kt", "rb", "sh", "md", "txt", "toml", "yaml", "yml",
    "json", "xml", "html", "css", "cfg", "ini", "lock", "S", "rst", "tex", "gd", "tscn", "tres", "godot", "cs", "glsl", "shader", "csv",
    "sql", "svg", "mk", "cmake", "in", "dts", "dtsi", "kconfig",
];
const MODEL_EXT: &[&str] = &["safetensors", "gguf", "ckpt", "pt", "pth", "onnx", "npz", "npy", "bin_model"];

pub fn classify(path: &str, data: &[u8]) -> Class {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if MODEL_EXT.contains(&ext.as_str()) {
        return Class::ModelCkpt;
    }
    let head = &data[..data.len().min(8192)];
    let nul = head.contains(&0);
    let utf8 = std::str::from_utf8(head).is_ok() || (head.len() == 8192 && std::str::from_utf8(&head[..head.len() - 3]).is_ok());
    if !nul && (TEXT_EXT.contains(&ext.as_str()) || utf8) {
        return Class::SourceText;
    }
    if data.len() >= 1 << 20 {
        Class::LargeBinary
    } else {
        Class::SmallBinary
    }
}

struct Builder {
    blobs: Vec<Blob>,
    by_hash: HashMap<[u8; 32], usize>,
}
impl Builder {
    fn add(&mut self, path: &str, data: Vec<u8>) -> usize {
        let h = *blake3::hash(&data).as_bytes();
        if let Some(&i) = self.by_hash.get(&h) {
            return i;
        }
        let class = classify(path, &data);
        self.blobs.push(Blob {
            data,
            class,
            path: path.to_string(),
        });
        let i = self.blobs.len() - 1;
        self.by_hash.insert(h, i);
        i
    }
}

fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, std::path::PathBuf)>) {
    let mut ents: Vec<_> = match std::fs::read_dir(dir) {
        Ok(r) => r.filter_map(|e| e.ok()).collect(),
        Err(_) => return,
    };
    ents.sort_by_key(|e| e.file_name());
    for e in ents {
        let p = e.path();
        let name = e.file_name();
        if name == ".git" {
            continue;
        }
        let ft = match e.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_dir() {
            walk(&p, root, out);
        } else if ft.is_file() {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().to_string();
            out.push((rel, p));
        }
    }
}

fn load_dir_rev(b: &mut Builder, dir: &Path) -> Vec<Entry> {
    let mut files = Vec::new();
    walk(dir, dir, &mut files);
    let mut v = Vec::new();
    for (rel, p) in files {
        if let Ok(data) = std::fs::read(&p) {
            let blob = b.add(&rel, data);
            v.push(Entry { path: rel, blob });
        }
    }
    v
}

fn git(repo: &str, args: &[&str]) -> Vec<u8> {
    let o = Command::new("git").arg("-C").arg(repo).args(args).output().expect("git");
    if !o.status.success() {
        panic!("git {:?} failed: {}", args, String::from_utf8_lossy(&o.stderr));
    }
    o.stdout
}

fn load_git(b: &mut Builder, repo: &str, nrev: usize, prefix: Option<&str>, max_blob: usize) -> (Vec<Vec<Entry>>, Vec<String>) {
    let list = String::from_utf8(git(repo, &["rev-list", "--first-parent", "HEAD"])).unwrap();
    let all: Vec<&str> = list.lines().collect(); // newest first
    let n = all.len();
    let mut picks: Vec<usize> = (0..nrev.min(n))
        .map(|i| if nrev <= 1 { 0 } else { (i * (n - 1)) / (nrev - 1) })
        .collect();
    picks.dedup();
    picks.reverse(); // oldest first
    let revs_sel: Vec<String> = picks.iter().map(|&i| all[i].to_string()).collect();
    // gather (oid,size,path) per rev
    let mut per_rev: Vec<Vec<(String, String)>> = Vec::new();
    let mut wanted: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for r in &revs_sel {
        let mut args = vec!["ls-tree", "-r", "-z", "-l", r.as_str()];
        if let Some(p) = prefix {
            args.push("--");
            args.push(p);
        }
        let out = git(repo, &args);
        let mut ents = Vec::new();
        for rec in out.split(|&c| c == 0) {
            if rec.is_empty() {
                continue;
            }
            let s = String::from_utf8_lossy(rec).to_string();
            let (meta, path) = match s.split_once('\t') {
                Some(x) => x,
                None => continue,
            };
            let f: Vec<&str> = meta.split_whitespace().collect();
            if f.len() < 4 || f[1] != "blob" || f[0] == "160000" {
                continue;
            }
            let size: usize = f[3].parse().unwrap_or(0);
            if size > max_blob {
                continue;
            }
            if seen.insert(f[2].to_string()) {
                wanted.push(f[2].to_string());
            }
            ents.push((f[2].to_string(), path.to_string()));
        }
        per_rev.push(ents);
    }
    // fetch blobs
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("git cat-file");
    let mut stdin = child.stdin.take().unwrap();
    let w2 = wanted.clone();
    let writer = std::thread::spawn(move || {
        for o in w2 {
            let _ = writeln!(stdin, "{o}");
        }
    });
    let mut rd = BufReader::with_capacity(1 << 20, child.stdout.take().unwrap());
    let mut oid_blob: HashMap<String, usize> = HashMap::new();
    let mut first_path: HashMap<&str, &str> = HashMap::new();
    for ents in &per_rev {
        for (o, p) in ents {
            first_path.entry(o.as_str()).or_insert(p.as_str());
        }
    }
    for o in &wanted {
        let mut line = String::new();
        rd.read_line(&mut line).unwrap();
        let f: Vec<&str> = line.split_whitespace().collect();
        let size: usize = f[2].parse().unwrap();
        let mut data = vec![0u8; size];
        rd.read_exact(&mut data).unwrap();
        let mut nl = [0u8; 1];
        rd.read_exact(&mut nl).unwrap();
        let idx = b.add(first_path[o.as_str()], data);
        oid_blob.insert(o.clone(), idx);
    }
    writer.join().unwrap();
    let _ = child.wait();
    let revs = per_rev
        .into_iter()
        .map(|ents| {
            ents.into_iter()
                .map(|(o, p)| Entry {
                    path: p,
                    blob: oid_blob[&o],
                })
                .collect()
        })
        .collect();
    (revs, revs_sel)
}

/// SPEC: dir:PATH | dirrevs:PATH | git:PATH[:NREV[:SUBPATH]] | list:FILE
pub fn load(name: &str, spec: &str, max_blob: usize) -> Corpus {
    let mut b = Builder {
        blobs: Vec::new(),
        by_hash: HashMap::new(),
    };
    let (kind, rest) = spec.split_once(':').expect("spec kind:arg");
    let revs: Vec<Vec<Entry>> = match kind {
        "dir" => vec![load_dir_rev(&mut b, Path::new(rest))],
        "dirrevs" => {
            let mut subs: Vec<_> = std::fs::read_dir(rest)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.path())
                .collect();
            subs.sort();
            subs.iter().map(|d| load_dir_rev(&mut b, d)).collect()
        }
        "list" => {
            let txt = std::fs::read_to_string(rest).unwrap();
            let mut v = Vec::new();
            for l in txt.lines().filter(|l| !l.trim().is_empty()) {
                if let Ok(data) = std::fs::read(l) {
                    let blob = b.add(l, data);
                    v.push(Entry { path: l.to_string(), blob });
                }
            }
            vec![v]
        }
        "git" => {
            let mut it = rest.splitn(3, ':');
            let repo = it.next().unwrap();
            let n: usize = it.next().and_then(|x| x.parse().ok()).unwrap_or(10);
            let prefix = it.next();
            let (revs, sel) = load_git(&mut b, repo, n, prefix, max_blob);
            eprintln!("git revisions: {}", sel.join(" "));
            revs
        }
        _ => panic!("unknown source kind {kind}"),
    };
    Corpus {
        name: name.to_string(),
        revs,
        blobs: b.blobs,
    }
}

impl Corpus {
    /// Zone = first `depth` directory components of the path ("." for files that sit above that depth).
    pub fn zone_of(path: &str, depth: usize) -> String {
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() <= depth {
            return ".".to_string();
        }
        parts[..depth].join("/")
    }
    pub fn summary(&self) -> serde_json::Value {
        let mut by: HashMap<&str, (usize, u64)> = HashMap::new();
        for bl in &self.blobs {
            let e = by.entry(bl.class.name()).or_default();
            e.0 += 1;
            e.1 += bl.data.len() as u64;
        }
        let logical: u64 = self
            .revs
            .iter()
            .map(|r| r.iter().map(|e| self.blobs[e.blob].data.len() as u64).sum::<u64>())
            .sum();
        let last: u64 = self
            .revs
            .last()
            .map(|r| r.iter().map(|e| self.blobs[e.blob].data.len() as u64).sum())
            .unwrap_or(0);
        serde_json::json!({
            "name": self.name, "revisions": self.revs.len(),
            "files_per_rev": self.revs.iter().map(|r| r.len()).collect::<Vec<_>>(),
            "unique_blobs": self.blobs.len(),
            "unique_bytes": self.blobs.iter().map(|b| b.data.len() as u64).sum::<u64>(),
            "logical_bytes_all_revisions": logical, "logical_bytes_last_revision": last,
            "by_class": by.iter().map(|(k, v)| (k.to_string(), serde_json::json!({"blobs": v.0, "bytes": v.1}))).collect::<serde_json::Map<_,_>>(),
        })
    }
}
