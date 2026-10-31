//! Synthetic multi-zone history model + commit-graph / changed-path / tree index candidates.
//!
//! Model (all parameters printed with the results): Z zones, each an independent DAG (a parent always lives in
//! its child's zone, as in docs/object-model.md). Commits are stored in topological order. Each commit changes a few
//! files chosen by Zipf-skewed directory popularity, with a small fraction of "refactor" commits touching hundreds.
use crate::util::{Id, Rng};

pub struct Commit {
    pub parents: Vec<u32>, // indexes within the zone, always smaller than own index
    pub time: u64,
    /// Changed nodes against the first parent (files plus every ancestor directory, deduplicated, sorted).
    pub changed: Vec<u32>,
}
pub struct Zone {
    pub commits: Vec<Commit>,
    /// Trie of paths: node -> parent node (u32::MAX for root). Nodes are files and directories.
    pub node_parent: Vec<u32>,
    pub node_name: Vec<String>,
    pub node_is_file: Vec<bool>,
    pub files: Vec<u32>,
}
impl Zone {
    pub fn path(&self, mut n: u32) -> String {
        let mut parts = vec![];
        while n != u32::MAX {
            parts.push(self.node_name[n as usize].as_str());
            n = self.node_parent[n as usize];
        }
        parts.reverse();
        parts.join("/")
    }
    pub fn depth(&self, mut n: u32) -> usize {
        let mut d = 0;
        while self.node_parent[n as usize] != u32::MAX {
            n = self.node_parent[n as usize];
            d += 1;
        }
        d
    }
}

pub struct Params {
    pub zones: usize,
    pub commits_per_zone: usize,
    pub files_per_zone: usize,
    pub branch_prob: f64,   // chance a mainline step starts a side branch
    pub mean_branch_len: f64,
    pub mean_files_per_commit: f64,
    pub refactor_prob: f64, // chance of a huge commit
    pub skew_prob: f64,     // chance a commit's timestamp is skewed (clock skew)
    pub seed: u64,
}
impl Params {
    pub fn describe(&self) -> String {
        format!(
            "zones={} commits/zone={} files/zone={} branch_prob={} mean_branch_len={} mean_files/commit={} refactor_prob={} clock_skew_prob={} seed={}",
            self.zones, self.commits_per_zone, self.files_per_zone, self.branch_prob, self.mean_branch_len, self.mean_files_per_commit, self.refactor_prob, self.skew_prob, self.seed
        )
    }
}

fn geom(r: &mut Rng, mean: f64) -> usize {
    let p = 1.0 / mean.max(1.0);
    let u = (r.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
    1 + (((1.0 - u).ln() / (1.0 - p).ln()) as usize)
}

pub fn build_zone(p: &Params, zi: usize) -> Zone {
    let mut r = Rng::new(p.seed ^ (zi as u64 + 1).wrapping_mul(0x9E37_79B9));
    // path trie: fanout 6 dirs per level, 3..6 levels, ~16 files per leaf dir => realistic source tree shape
    let mut z = Zone { commits: vec![], node_parent: vec![], node_name: vec![], node_is_file: vec![], files: vec![] };
    let root = 0u32;
    z.node_parent.push(u32::MAX);
    z.node_name.push(format!("z{zi}"));
    z.node_is_file.push(false);
    let mut dirs = vec![(root, 0usize)];
    let mut leaf_dirs = vec![];
    let mut frontier = vec![root];
    for depth in 1..=6usize {
        let mut next = vec![];
        for &d in &frontier {
            let fan = if depth <= 2 { 6 } else { 3 };
            for k in 0..fan {
                if (z.node_name.len() as f64) * 0.06 > (p.files_per_zone as f64) / 16.0 + 4.0 {
                    break;
                }
                let id = z.node_parent.len() as u32;
                z.node_parent.push(d);
                z.node_name.push(format!("d{k}"));
                z.node_is_file.push(false);
                dirs.push((id, depth));
                next.push(id);
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    leaf_dirs.extend(frontier.iter().copied());
    if leaf_dirs.is_empty() {
        leaf_dirs.push(root);
    }
    for f in 0..p.files_per_zone {
        let d = leaf_dirs[(f * 7 + (r.next_u64() as usize % 3)) % leaf_dirs.len()];
        let id = z.node_parent.len() as u32;
        z.node_parent.push(d);
        z.node_name.push(format!("f{f}.rs"));
        z.node_is_file.push(true);
        z.files.push(id);
    }
    // Zipf popularity over files (s ~ 1): file rank f has weight 1/(f+1); sampled via inverse CDF on a prefix table
    let nf = z.files.len();
    let mut cdf = Vec::with_capacity(nf);
    let mut acc = 0.0;
    for i in 0..nf {
        acc += 1.0 / (i as f64 + 1.0);
        cdf.push(acc);
    }
    let mut perm: Vec<u32> = z.files.clone();
    for i in (1..nf).rev() {
        perm.swap(i, (r.next_u64() as usize) % (i + 1)); // popularity rank -> random file
    }
    let pick = |r: &mut Rng| -> u32 {
        let u = (r.next_u64() >> 11) as f64 / (1u64 << 53) as f64 * acc;
        perm[cdf.partition_point(|&c| c < u).min(nf - 1)]
    };
    let ancestors = |z: &Zone, f: u32, out: &mut Vec<u32>| {
        let mut n = f;
        while n != u32::MAX {
            out.push(n);
            n = z.node_parent[n as usize];
        }
    };
    // DAG: mainline index `tip`; side branches of random length that merge back
    let mut time = 1_600_000_000u64;
    let mut tip: Option<u32> = None;
    let target = p.commits_per_zone;
    let mut side: Option<(u32, usize)> = None; // (side tip, remaining)
    let mut side_changes: Vec<u32> = vec![];
    while z.commits.len() < target {
        time += 1 + r.next_u64() % 3600;
        let make_changes = |r: &mut Rng, z: &Zone| -> Vec<u32> {
            let n = if (r.next_u64() as f64 / u64::MAX as f64) < p.refactor_prob { 100 + (r.next_u64() % 900) as usize } else { geom(r, p.mean_files_per_commit) };
            let mut c = vec![];
            for _ in 0..n {
                ancestors(z, pick(r), &mut c);
            }
            c.sort_unstable();
            c.dedup();
            c
        };
        let skew = |r: &mut Rng, t: u64| -> u64 {
            if (r.next_u64() as f64 / u64::MAX as f64) < p.skew_prob {
                let d = 3600 * (1 + r.next_u64() % 48);
                if r.next_u64() & 1 == 0 { t.saturating_sub(d) } else { t + d }
            } else {
                t
            }
        };
        let idx = z.commits.len() as u32;
        let coin = r.next_u64() & 1 == 0;
        match (side, tip) {
            (Some((_, rem)), Some(m)) if rem > 0 && !coin => {
                // mainline keeps moving while the side branch is open
                let ch = make_changes(&mut r, &z);
                let t = skew(&mut r, time);
                z.commits.push(Commit { parents: vec![m], time: t, changed: ch });
                tip = Some(idx);
            }
            (Some((st, rem)), Some(_)) if rem > 0 => {
                let ch = make_changes(&mut r, &z);
                side_changes.extend_from_slice(&ch);
                let t = skew(&mut r, time);
                z.commits.push(Commit { parents: vec![st], time: t, changed: ch });
                side = Some((idx, rem - 1));
            }
            (Some((st, _)), Some(m)) => {
                // merge side back: changes against first parent (mainline) = union of the side's changes
                let mut ch = std::mem::take(&mut side_changes);
                ch.sort_unstable();
                ch.dedup();
                let t = skew(&mut r, time);
                z.commits.push(Commit { parents: vec![m, st], time: t, changed: ch });
                tip = Some(idx);
                side = None;
            }
            (None, _) => {
                let ch = make_changes(&mut r, &z);
                let t = skew(&mut r, time);
                let parents = tip.map(|t| vec![t]).unwrap_or_default();
                let start_side = tip.is_some() && (r.next_u64() as f64 / u64::MAX as f64) < p.branch_prob;
                z.commits.push(Commit { parents: parents.clone(), time: t, changed: ch });
                if start_side {
                    // the side branch forks from the commit just made (mainline tip), so it will have its own history
                    side = Some((idx, geom(&mut r, p.mean_branch_len)));
                    tip = Some(idx);
                    side_changes.clear();
                } else {
                    tip = Some(idx);
                }
            }
            _ => unreachable!(),
        }
    }
    // a still-open side branch just stays unmerged (a live feature branch) -- leave as is
    let _ = dirs;
    z
}

pub fn commit_id(zone: usize, i: usize) -> Id {
    *blake3::hash(format!("c{zone}/{i}").as_bytes()).as_bytes()
}

// ---------------- reachability ----------------

/// Generation numbers, topological level (git v1): roots = 1, else 1 + max(parents).
pub fn gen_v1(z: &Zone) -> Vec<u32> {
    let mut g = vec![0u32; z.commits.len()];
    for (i, c) in z.commits.iter().enumerate() {
        g[i] = 1 + c.parents.iter().map(|&p| g[p as usize]).max().unwrap_or(0);
    }
    g
}
/// Corrected commit date (git v2): max(own time, 1 + max corrected parent time).
pub fn gen_v2(z: &Zone) -> Vec<u64> {
    let mut g = vec![0u64; z.commits.len()];
    for (i, c) in z.commits.iter().enumerate() {
        g[i] = c.time.max(1 + c.parents.iter().map(|&p| g[p as usize]).max().unwrap_or(0));
    }
    g
}

pub struct Grail {
    k: usize,
    low: Vec<u32>, // k per commit
    post: Vec<u32>,
}
impl Grail {
    pub fn bytes(&self) -> usize {
        (self.low.len() + self.post.len()) * 4
    }
    /// k randomised DFS labelings over parent edges. post = rank in post-order, low = min post in the reachable set.
    pub fn build(z: &Zone, k: usize, seed: u64) -> Grail {
        let n = z.commits.len();
        let mut low = vec![0u32; n * k];
        let mut post = vec![0u32; n * k];
        let mut r = Rng::new(seed);
        for t in 0..k {
            let mut visited = vec![false; n];
            let mut rank = 0u32;
            let mut order: Vec<u32> = (0..n as u32).collect();
            for i in (1..n).rev() {
                order.swap(i, (r.next_u64() as usize) % (i + 1));
            }
            for &root in &order {
                if visited[root as usize] {
                    continue;
                }
                let mut st: Vec<(u32, usize)> = vec![(root, 0)];
                visited[root as usize] = true;
                while let Some(&mut (u, ref mut ci)) = st.last_mut() {
                    let ps = &z.commits[u as usize].parents;
                    if *ci < ps.len() {
                        let v = ps[if t % 2 == 0 { *ci } else { ps.len() - 1 - *ci }];
                        *ci += 1;
                        if !visited[v as usize] {
                            visited[v as usize] = true;
                            st.push((v, 0));
                        }
                    } else {
                        rank += 1;
                        post[u as usize * k + t] = rank;
                        let mut lo = rank;
                        for &v in ps {
                            lo = lo.min(low[v as usize * k + t]);
                        }
                        low[u as usize * k + t] = lo;
                        st.pop();
                    }
                }
            }
        }
        Grail { k, low, post }
    }
    /// false => definitely cannot reach
    pub fn may_reach(&self, from: u32, to: u32) -> bool {
        (0..self.k).all(|t| {
            let (f, o) = (from as usize * self.k + t, to as usize * self.k + t);
            self.low[f] <= self.low[o] && self.post[o] <= self.post[f]
        })
    }
}

#[derive(Default, Clone, Copy)]
pub struct Walk {
    pub visited: u64,
    pub answer: bool,
}

/// Is `anc` an ancestor of (or equal to) `desc`? Strategy decides pruning. Returns commits popped (decoded in real life).
pub enum Prune<'a> {
    None,
    Time,
    GenV1(&'a [u32]),
    GenV2(&'a [u64]),
    Grail(&'a Grail, &'a [u32]),
}
pub fn is_ancestor(z: &Zone, anc: u32, desc: u32, prune: &Prune, seen: &mut Vec<u32>, stamp: u32) -> Walk {
    let mut w = Walk::default();
    let mut st = vec![desc];
    seen[desc as usize] = stamp;
    while let Some(u) = st.pop() {
        w.visited += 1;
        if u == anc {
            w.answer = true;
            return w;
        }
        for &p in &z.commits[u as usize].parents {
            if seen[p as usize] == stamp {
                continue;
            }
            let cut = match prune {
                Prune::None => false,
                Prune::Time => z.commits[p as usize].time < z.commits[anc as usize].time,
                Prune::GenV1(g) => g[p as usize] < g[anc as usize],
                Prune::GenV2(g) => g[p as usize] < g[anc as usize],
                Prune::Grail(gr, g) => g[p as usize] < g[anc as usize] || !gr.may_reach(p, anc),
            };
            seen[p as usize] = stamp;
            if !cut {
                st.push(p);
            }
        }
    }
    w
}

// ---------------- changed-path filters ----------------

pub fn node_hash(zone: usize, z: &Zone, n: u32) -> u64 {
    let h = blake3::hash(format!("{zone}:{}", z.path(n)).as_bytes());
    u64::from_le_bytes(h.as_bytes()[..8].try_into().unwrap())
}

/// Per-commit Bloom filter, Git style: double hashing, `bpe` bits per entry, k hashes, commits with > max_changes entries get
/// no filter ("too large": always maybe).
pub struct CommitBloom {
    pub bpe: f64,
    pub k: u32,
    pub max: usize,
    /// per commit: None = too large
    pub f: Vec<Option<Vec<u8>>>,
}
fn bl_pos(h: u64, i: u64, nbits: u64) -> u64 {
    let h1 = h as u32 as u64;
    let h2 = (h >> 32) | 1;
    (h1.wrapping_add(i.wrapping_mul(h2)) & 0xffff_ffff) % nbits
}
impl CommitBloom {
    pub fn build(z: &Zone, zi: usize, hashes: &[u64], bpe: f64, k: u32, max: usize) -> CommitBloom {
        let _ = (z, zi);
        let f = z
            .commits
            .iter()
            .map(|c| {
                if c.changed.len() > max {
                    return None;
                }
                let nbits = ((c.changed.len() as f64 * bpe).ceil() as u64).max(8);
                let mut b = vec![0u8; nbits.div_ceil(8) as usize];
                for &n in &c.changed {
                    let h = hashes[n as usize];
                    for i in 0..k as u64 {
                        let p = bl_pos(h, i, nbits);
                        b[(p >> 3) as usize] |= 1 << (p & 7);
                    }
                }
                Some(b)
            })
            .collect();
        CommitBloom { bpe, k, max, f }
    }
    pub fn maybe(&self, commit: usize, h: u64) -> bool {
        match &self.f[commit] {
            None => true,
            Some(b) => {
                let nbits = b.len() as u64 * 8;
                // the filter length rounds up to bytes; git stores nbits implicitly the same way (len*8)
                (0..self.k as u64).all(|i| {
                    let p = bl_pos(h, i, nbits);
                    b[(p >> 3) as usize] >> (p & 7) & 1 == 1
                })
            }
        }
    }
    pub fn bytes(&self) -> usize {
        self.f.iter().map(|x| x.as_ref().map(|v| v.len() + 4).unwrap_or(4)).sum() // + 4-byte end offset per commit (git BIDX)
    }
}

/// Exact inverted index: node -> sorted commit list.
pub struct Inverted {
    pub postings: Vec<Vec<u32>>,
}
impl Inverted {
    pub fn build(z: &Zone) -> Inverted {
        let mut postings = vec![vec![]; z.node_parent.len()];
        for (i, c) in z.commits.iter().enumerate() {
            for &n in &c.changed {
                postings[n as usize].push(i as u32);
            }
        }
        Inverted { postings }
    }
    /// varint-delta encoded size, the realistic on-disk form
    pub fn bytes(&self) -> usize {
        let mut t = 0;
        for p in &self.postings {
            let mut prev = 0u32;
            for &c in p {
                let mut d = c - prev;
                prev = c;
                t += 1;
                while d >= 128 {
                    d >>= 7;
                    t += 1;
                }
            }
            t += 2; // posting length + dictionary slot amortised (approx, 2 B; not measured)
        }
        t
    }
}
