//! Load a real git history (via the git binary) into memory as an ordered object list.
//! Objects are git's objects (sha1 ids, git encodings), not allgit's: content and history shape are real,
//! identities and exact byte encodings differ. This is a stated caveat of every measurement.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

pub const COMMIT: u8 = 1;
pub const TREE: u8 = 2;
pub const BLOB: u8 = 3;

pub struct Obj {
    pub sha: [u8; 20],
    pub kind: u8,
    pub commit: u32,
    pub path: u32,
    pub zone: u16,
    pub data: Vec<u8>,
}

pub struct Dataset {
    pub name: String,
    pub objs: Vec<Obj>,
    pub paths: Vec<String>,
    pub zones: Vec<String>,
    pub head: Vec<u32>,
    pub ncommits: u32,
}

fn unhex(s: &str) -> [u8; 20] {
    let mut o = [0u8; 20];
    for i in 0..20 {
        o[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
    }
    o
}

fn comps(p: &str) -> Vec<&str> {
    p.split('/').filter(|c| !c.is_empty()).collect()
}

impl Dataset {
    pub fn total_bytes(&self) -> u64 {
        self.objs.iter().map(|o| o.data.len() as u64).sum()
    }

    pub fn load(name: &str, repo: &str, max_commits: Option<usize>) -> Dataset {
        let mut args: Vec<String> = ["-C", repo, "log", "--reverse", "--topo-order", "--all", "--root", "-t", "-r", "--no-abbrev", "--no-renames", "--raw", "--format=C%H %T"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        if let Some(m) = max_commits {
            args.insert(4, format!("--max-count={m}"));
        }
        let out = Command::new("git").args(&args).output().expect("git log");
        assert!(out.status.success(), "git log failed");
        let text = String::from_utf8_lossy(&out.stdout);
        let mut order: Vec<([u8; 20], u32, String)> = Vec::new();
        let mut seen: HashMap<[u8; 20], ()> = HashMap::new();
        let mut cidx: i64 = -1;
        let zero = [0u8; 20];
        for line in text.lines() {
            if let Some(r) = line.strip_prefix('C') {
                cidx += 1;
                let mut it = r.split(' ');
                let (c, t) = (unhex(it.next().unwrap()), unhex(it.next().unwrap()));
                if seen.insert(c, ()).is_none() {
                    order.push((c, cidx as u32, String::new()));
                }
                if seen.insert(t, ()).is_none() {
                    order.push((t, cidx as u32, String::new()));
                }
            } else if let Some(r) = line.strip_prefix(':') {
                let Some((meta, path)) = r.split_once('\t') else { continue };
                let f: Vec<&str> = meta.split(' ').collect();
                if f.len() < 5 || f[1] == "160000" {
                    continue;
                }
                let n = unhex(f[3]);
                if n == zero {
                    continue;
                }
                if seen.insert(n, ()).is_none() {
                    order.push((n, cidx as u32, path.to_string()));
                }
            }
        }
        drop(text);
        // contents
        let mut child = Command::new("git").args(["-C", repo, "cat-file", "--batch"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let shas: Vec<String> = order.iter().map(|o| o.0.iter().map(|b| format!("{b:02x}")).collect()).collect();
        let w = std::thread::spawn(move || {
            for s in shas {
                stdin.write_all(s.as_bytes()).unwrap();
                stdin.write_all(b"\n").unwrap();
            }
        });
        let mut rd = BufReader::with_capacity(1 << 20, child.stdout.take().unwrap());
        let mut path_ids: HashMap<String, u32> = HashMap::new();
        let mut paths: Vec<String> = vec![];
        let mut objs = Vec::with_capacity(order.len());
        let mut raw_paths: Vec<String> = vec![];
        for (sha, c, p) in &order {
            let mut hdr = String::new();
            rd.read_line(&mut hdr).unwrap();
            let f: Vec<&str> = hdr.trim_end().split(' ').collect();
            assert!(f.len() == 3, "bad header {hdr}");
            let size: usize = f[2].parse().unwrap();
            let mut data = vec![0u8; size + 1];
            rd.read_exact(&mut data).unwrap();
            data.pop();
            let kind = match f[1] {
                "commit" => COMMIT,
                "tree" => TREE,
                "blob" => BLOB,
                _ => 0,
            };
            if kind == 0 {
                continue;
            }
            let pid = *path_ids.entry(p.clone()).or_insert_with(|| {
                paths.push(p.clone());
                paths.len() as u32 - 1
            });
            raw_paths.push(p.clone());
            objs.push(Obj { sha: *sha, kind, commit: *c, path: pid, zone: 0, data });
        }
        w.join().unwrap();
        let _ = child.wait();
        // zones: first path component; split a dominant top-level dir by its second component if that stays sane.
        let mut top: HashMap<String, usize> = HashMap::new();
        for p in &paths {
            let c = comps(p);
            if let Some(f) = c.first() {
                *top.entry(f.to_string()).or_default() += 1;
            }
        }
        let total: usize = top.values().sum();
        let mut split: HashMap<String, bool> = HashMap::new();
        for (k, v) in &top {
            if *v * 100 > total * 40 {
                let mut subs: HashMap<&str, ()> = HashMap::new();
                for p in &paths {
                    let c = comps(p);
                    if c.len() >= 2 && c[0] == k {
                        subs.insert(c[1], ());
                    }
                }
                if subs.len() <= 200 && subs.len() >= 4 {
                    split.insert(k.clone(), true);
                }
            }
        }
        let mut zone_ids: HashMap<String, u16> = HashMap::new();
        let mut zones: Vec<String> = vec![];
        let mut zid = |z: String| -> u16 {
            *zone_ids.entry(z.clone()).or_insert_with(|| {
                zones.push(z);
                zones.len() as u16 - 1
            })
        };
        zid("<root>".into());
        let pz: Vec<u16> = paths
            .iter()
            .map(|p| {
                let c = comps(p);
                match c.len() {
                    0 => 0,
                    1 => zid(c[0].to_string()),
                    _ => {
                        if split.contains_key(c[0]) && c.len() >= 3 {
                            zid(format!("{}/{}", c[0], c[1]))
                        } else {
                            zid(c[0].to_string())
                        }
                    }
                }
            })
            .collect();
        // a depth-1 *file* belongs to root; the log path alone can't tell file from dir, use the object kind.
        for o in objs.iter_mut() {
            let c = comps(&paths[o.path as usize]);
            o.zone = if o.kind == COMMIT || (c.len() == 1 && o.kind == BLOB) { 0 } else { pz[o.path as usize] };
        }
        let ids: HashMap<[u8; 20], u32> = objs.iter().enumerate().map(|(i, o)| (o.sha, i as u32)).collect();
        let lt = Command::new("git").args(["-C", repo, "ls-tree", "-r", "-t", "HEAD"]).output().unwrap();
        let mut head = vec![];
        for l in String::from_utf8_lossy(&lt.stdout).lines() {
            if let Some((m, _)) = l.split_once('\t') {
                let f: Vec<&str> = m.split(' ').collect();
                if f.len() == 3 && f[1] != "commit" {
                    if let Some(&i) = ids.get(&unhex(f[2])) {
                        head.push(i);
                    }
                }
            }
        }
        let ncommits = (cidx + 1) as u32;
        Dataset { name: name.into(), objs, paths, zones, head, ncommits }
    }
}

#[allow(dead_code)]
pub fn read_all<R: Read>(mut r: R) -> Vec<u8> {
    let mut v = vec![];
    r.read_to_end(&mut v).unwrap();
    v
}
