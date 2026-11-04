//! Cache policies (byte-capacity), trace generation from the history model, and a replay simulator.
use crate::history::{build_zone, Params, Zone};
use crate::util::Rng;
use std::collections::HashMap;

// ------------------------------------------------------------------ intrusive lists over a slab
const NIL: u32 = u32::MAX;
#[derive(Clone, Copy)]
struct Slot {
    key: u64,
    size: u32,
    prev: u32,
    next: u32,
    list: u8,
    freq: u8,
}
#[derive(Clone, Copy)]
struct ListHead {
    head: u32, // most recent
    tail: u32, // least recent
    bytes: u64,
    len: u32,
}
impl Default for ListHead {
    fn default() -> Self {
        ListHead { head: NIL, tail: NIL, bytes: 0, len: 0 }
    }
}
/// Slab of entries that live in exactly one of several recency lists.
struct Lists {
    slots: Vec<Slot>,
    free: Vec<u32>,
    heads: Vec<ListHead>,
    map: HashMap<u64, u32>,
}
impl Lists {
    fn new(nlists: usize) -> Lists {
        Lists { slots: vec![], free: vec![], heads: vec![ListHead::default(); nlists], map: HashMap::new() }
    }
    fn alloc(&mut self, key: u64, size: u32, list: u8) -> u32 {
        let s = Slot { key, size, prev: NIL, next: NIL, list, freq: 0 };
        let i = if let Some(i) = self.free.pop() {
            self.slots[i as usize] = s;
            i
        } else {
            self.slots.push(s);
            (self.slots.len() - 1) as u32
        };
        self.map.insert(key, i);
        i
    }
    fn push_front(&mut self, i: u32, list: u8) {
        let h = &mut self.heads[list as usize];
        let old = h.head;
        self.slots[i as usize].list = list;
        self.slots[i as usize].prev = NIL;
        self.slots[i as usize].next = old;
        if old != NIL {
            self.slots[old as usize].prev = i;
        } else {
            h.tail = i;
        }
        h.head = i;
        h.bytes += self.slots[i as usize].size as u64;
        h.len += 1;
    }
    fn unlink(&mut self, i: u32) {
        let s = self.slots[i as usize];
        let h = &mut self.heads[s.list as usize];
        if s.prev != NIL {
            self.slots[s.prev as usize].next = s.next;
        } else {
            h.head = s.next;
        }
        if s.next != NIL {
            self.slots[s.next as usize].prev = s.prev;
        } else {
            h.tail = s.prev;
        }
        h.bytes -= s.size as u64;
        h.len -= 1;
    }
    fn drop_entry(&mut self, i: u32) {
        self.unlink(i);
        self.map.remove(&self.slots[i as usize].key);
        self.free.push(i);
    }
    fn tail(&self, list: u8) -> Option<u32> {
        let t = self.heads[list as usize].tail;
        (t != NIL).then_some(t)
    }
    fn bytes(&self, list: u8) -> u64 {
        self.heads[list as usize].bytes
    }
}

pub trait Policy {
    fn name(&self) -> String;
    /// Look up `key`; on a miss insert it (subject to admission). Returns hit.
    fn access(&mut self, key: u64, size: u32) -> bool;
    fn contains(&self, key: u64) -> bool;
    fn used(&self) -> u64;
}

// ------------------------------------------------------------------ LRU
pub struct Lru {
    l: Lists,
    cap: u64,
}
impl Lru {
    pub fn new(cap: u64) -> Lru {
        Lru { l: Lists::new(1), cap }
    }
}
impl Policy for Lru {
    fn name(&self) -> String {
        "lru".into()
    }
    fn access(&mut self, key: u64, size: u32) -> bool {
        if let Some(&i) = self.l.map.get(&key) {
            self.l.unlink(i);
            self.l.push_front(i, 0);
            return true;
        }
        if size as u64 > self.cap {
            return false;
        }
        while self.l.bytes(0) + size as u64 > self.cap {
            let t = self.l.tail(0).unwrap();
            self.l.drop_entry(t);
        }
        let i = self.l.alloc(key, size, 0);
        self.l.push_front(i, 0);
        false
    }
    fn contains(&self, key: u64) -> bool {
        self.l.map.contains_key(&key)
    }
    fn used(&self) -> u64 {
        self.l.bytes(0)
    }
}

// ------------------------------------------------------------------ FIFO (baseline: no recency)
pub struct Fifo {
    l: Lists,
    cap: u64,
}
impl Fifo {
    pub fn new(cap: u64) -> Fifo {
        Fifo { l: Lists::new(1), cap }
    }
}
impl Policy for Fifo {
    fn name(&self) -> String {
        "fifo".into()
    }
    fn access(&mut self, key: u64, size: u32) -> bool {
        if self.l.map.contains_key(&key) {
            return true;
        }
        if size as u64 > self.cap {
            return false;
        }
        while self.l.bytes(0) + size as u64 > self.cap {
            let t = self.l.tail(0).unwrap();
            self.l.drop_entry(t);
        }
        let i = self.l.alloc(key, size, 0);
        self.l.push_front(i, 0);
        false
    }
    fn contains(&self, key: u64) -> bool {
        self.l.map.contains_key(&key)
    }
    fn used(&self) -> u64 {
        self.l.bytes(0)
    }
}

// ------------------------------------------------------------------ ARC, byte-weighted
// lists: 0=T1 (seen once), 1=T2 (seen >=2), 2=B1 (ghost of T1), 3=B2 (ghost of T2)
pub struct Arc {
    l: Lists,
    cap: u64,
    p: u64, // target bytes for T1
}
impl Arc {
    pub fn new(cap: u64) -> Arc {
        Arc { l: Lists::new(4), cap, p: 0 }
    }
    fn replace(&mut self, in_b2: bool) {
        let t1 = self.l.bytes(0);
        let evict_t1 = t1 > 0 && (t1 > self.p || (in_b2 && t1 == self.p));
        let (from, ghost) = if evict_t1 || self.l.tail(1).is_none() { (0u8, 2u8) } else { (1u8, 3u8) };
        if let Some(t) = self.l.tail(from) {
            self.l.unlink(t);
            self.l.push_front(t, ghost); // keep key as ghost (no data)
        }
    }
    fn make_room(&mut self, size: u64, in_b2: bool) {
        while self.l.bytes(0) + self.l.bytes(1) + size > self.cap && (self.l.tail(0).is_some() || self.l.tail(1).is_some()) {
            self.replace(in_b2);
        }
        // bound ghosts to `cap` bytes each
        while self.l.bytes(2) > self.cap {
            let t = self.l.tail(2).unwrap();
            self.l.drop_entry(t);
        }
        while self.l.bytes(3) > self.cap {
            let t = self.l.tail(3).unwrap();
            self.l.drop_entry(t);
        }
    }
}
impl Policy for Arc {
    fn name(&self) -> String {
        "arc".into()
    }
    fn access(&mut self, key: u64, size: u32) -> bool {
        if size as u64 > self.cap {
            return false;
        }
        if let Some(&i) = self.l.map.get(&key) {
            match self.l.slots[i as usize].list {
                0 | 1 => {
                    self.l.unlink(i);
                    self.l.push_front(i, 1);
                    return true;
                }
                2 => {
                    // ghost hit in B1: grow p
                    let b1 = self.l.bytes(2).max(1);
                    let b2 = self.l.bytes(3);
                    self.p = (self.p + (size as u64) * (b2 / b1).max(1)).min(self.cap);
                    self.l.unlink(i);
                    self.make_room(size as u64, false);
                    self.l.slots[i as usize].size = size;
                    self.l.push_front(i, 1);
                    return false;
                }
                _ => {
                    let b1 = self.l.bytes(2);
                    let b2 = self.l.bytes(3).max(1);
                    self.p = self.p.saturating_sub((size as u64) * (b1 / b2).max(1));
                    self.l.unlink(i);
                    self.make_room(size as u64, true);
                    self.l.slots[i as usize].size = size;
                    self.l.push_front(i, 1);
                    return false;
                }
            }
        }
        self.make_room(size as u64, false);
        let i = self.l.alloc(key, size, 0);
        self.l.push_front(i, 0);
        false
    }
    fn contains(&self, key: u64) -> bool {
        self.l.map.get(&key).is_some_and(|&i| self.l.slots[i as usize].list < 2)
    }
    fn used(&self) -> u64 {
        self.l.bytes(0) + self.l.bytes(1)
    }
}

// ------------------------------------------------------------------ W-TinyLFU
struct Sketch {
    t: Vec<u8>,
    mask: usize,
    adds: u64,
    reset_at: u64,
}
impl Sketch {
    fn new(entries: usize) -> Sketch {
        let w = (entries * 4).next_power_of_two().max(1024);
        Sketch { t: vec![0; w * 4], mask: w - 1, adds: 0, reset_at: (entries as u64 * 10).max(1000) }
    }
    fn idx(&self, key: u64, r: usize) -> usize {
        let h = key.wrapping_mul(0x9E37_79B9_7F4A_7C15 ^ ((r as u64 + 1) * 0xC2B2_AE3D_27D4_EB4F));
        r * (self.mask + 1) + ((h >> 20) as usize & self.mask)
    }
    fn add(&mut self, key: u64) {
        for r in 0..4 {
            let i = self.idx(key, r);
            if self.t[i] < 15 {
                self.t[i] += 1;
            }
        }
        self.adds += 1;
        if self.adds >= self.reset_at {
            for c in self.t.iter_mut() {
                *c >>= 1;
            }
            self.adds /= 2;
        }
    }
    fn est(&self, key: u64) -> u8 {
        (0..4).map(|r| self.t[self.idx(key, r)]).min().unwrap()
    }
}
/// W-TinyLFU (Einziger et al. 2017): 1% window LRU, main = SLRU(20% probation / 80% protected), count-min admission.
/// lists: 0=window 1=probation 2=protected
pub struct TinyLfu {
    l: Lists,
    cap: u64,
    sk: Sketch,
}
impl TinyLfu {
    pub fn new(cap: u64, avg_obj: u64) -> TinyLfu {
        TinyLfu { l: Lists::new(3), cap, sk: Sketch::new((cap / avg_obj.max(1)).max(64) as usize) }
    }
    fn win_cap(&self) -> u64 {
        (self.cap / 100).max(1)
    }
    fn main_cap(&self) -> u64 {
        self.cap - self.win_cap()
    }
    fn prot_cap(&self) -> u64 {
        self.main_cap() * 8 / 10
    }
    fn evict_to_fit(&mut self) {
        // window overflow -> candidate competes with probation victim
        while self.l.bytes(0) > self.win_cap() {
            let cand = self.l.tail(0).unwrap();
            self.l.unlink(cand);
            self.l.push_front(cand, 1);
            while self.l.bytes(1) + self.l.bytes(2) > self.main_cap() {
                let victim = match self.l.tail(1) {
                    Some(v) => v,
                    None => break,
                };
                let c = if self.l.heads[1].head != NIL { self.l.heads[1].head } else { cand };
                // compare newcomer (head of probation) with the probation victim (tail)
                if victim == c {
                    self.l.drop_entry(victim);
                    break;
                }
                let (fc, fv) = (self.sk.est(self.l.slots[c as usize].key), self.sk.est(self.l.slots[victim as usize].key));
                if fc > fv {
                    self.l.drop_entry(victim);
                } else {
                    self.l.drop_entry(c);
                }
            }
        }
    }
}
impl Policy for TinyLfu {
    fn name(&self) -> String {
        "w-tinylfu".into()
    }
    fn access(&mut self, key: u64, size: u32) -> bool {
        self.sk.add(key);
        if let Some(&i) = self.l.map.get(&key) {
            match self.l.slots[i as usize].list {
                0 => {
                    self.l.unlink(i);
                    self.l.push_front(i, 0);
                }
                1 => {
                    self.l.unlink(i);
                    self.l.push_front(i, 2);
                    while self.l.bytes(2) > self.prot_cap() {
                        let t = self.l.tail(2).unwrap();
                        self.l.unlink(t);
                        self.l.push_front(t, 1);
                    }
                }
                _ => {
                    self.l.unlink(i);
                    self.l.push_front(i, 2);
                }
            }
            return true;
        }
        if size as u64 > self.win_cap().max(self.cap / 2) {
            return false;
        }
        let i = self.l.alloc(key, size, 0);
        self.l.push_front(i, 0);
        self.evict_to_fit();
        false
    }
    fn contains(&self, key: u64) -> bool {
        self.l.map.contains_key(&key)
    }
    fn used(&self) -> u64 {
        self.l.bytes(0) + self.l.bytes(1) + self.l.bytes(2)
    }
}

// ------------------------------------------------------------------ Belady-style offline bound
/// Evict the resident object whose next use is farthest. Exact optimum only for uniform sizes; for variable sizes it is
/// a strong reference, not a proof of optimality (finding the optimum is NP-hard).
pub fn belady(trace: &[Access], cap: u64) -> (u64, u64) {
    let n = trace.len();
    let mut next = vec![usize::MAX; n];
    let mut last: HashMap<u64, usize> = HashMap::new();
    for i in (0..n).rev() {
        next[i] = *last.get(&trace[i].key).unwrap_or(&usize::MAX);
        last.insert(trace[i].key, i);
    }
    let mut res: HashMap<u64, (usize, u32)> = HashMap::new(); // key -> (next use, size)
    let mut heap: std::collections::BinaryHeap<(usize, u64)> = std::collections::BinaryHeap::new();
    let (mut used, mut hits, mut hit_bytes) = (0u64, 0u64, 0u64);
    for (i, a) in trace.iter().enumerate() {
        if let Some(e) = res.get_mut(&a.key) {
            hits += 1;
            hit_bytes += a.size as u64;
            e.0 = next[i];
            heap.push((next[i], a.key));
            continue;
        }
        if a.size as u64 > cap || next[i] == usize::MAX {
            continue; // never reused: bypass (the offline optimum would not admit it)
        }
        while used + a.size as u64 > cap {
            let (nu, k) = heap.pop().unwrap();
            match res.get(&k) {
                Some(&(cur, sz)) if cur == nu => {
                    res.remove(&k);
                    used -= sz as u64;
                }
                _ => continue, // stale heap entry
            }
        }
        res.insert(a.key, (next[i], a.size));
        heap.push((next[i], a.key));
        used += a.size as u64;
    }
    (hits, hit_bytes)
}

// ------------------------------------------------------------------ traces
#[derive(Clone, Copy)]
pub struct Access {
    pub key: u64,
    pub size: u32,
    /// batch level within an operation (objects of one level can be fetched in parallel); level 0 = commit/root
    pub level: u8,
    /// operation index (for per-op cost and warm-start analysis)
    pub op: u32,
    pub kind: u8, // 0 blob 1 tree 2 commit
}

pub struct World {
    pub zones: Vec<Zone>,
    /// per zone, per node: sorted commit indexes that touched it
    touched: Vec<Vec<Vec<u32>>>,
    children: Vec<Vec<u32>>, // per zone: child count per dir node
}
impl World {
    pub fn new(p: &Params) -> World {
        let zones: Vec<Zone> = (0..p.zones).map(|zi| build_zone(p, zi)).collect();
        let mut touched = vec![];
        let mut children = vec![];
        for z in &zones {
            let mut t = vec![vec![]; z.node_parent.len()];
            for (c, cm) in z.commits.iter().enumerate() {
                for &n in &cm.changed {
                    t[n as usize].push(c as u32);
                }
            }
            let mut ch = vec![0u32; z.node_parent.len()];
            for n in 0..z.node_parent.len() {
                if z.node_parent[n] != u32::MAX {
                    ch[z.node_parent[n] as usize] += 1;
                }
            }
            touched.push(t);
            children.push(ch);
        }
        World { zones, touched, children }
    }
    fn ver(&self, zi: usize, node: u32, c: u32) -> u32 {
        let t = &self.touched[zi][node as usize];
        let p = t.partition_point(|&x| x <= c);
        if p == 0 {
            0
        } else {
            t[p - 1] + 1
        }
    }
    fn obj(&self, zi: usize, node: u32, c: u32) -> (u64, u32, u8) {
        let z = &self.zones[zi];
        let v = self.ver(zi, node, c);
        let key = ((zi as u64) << 56) | ((node as u64) << 24) | v as u64 | 1 << 62;
        let h = key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 11;
        if z.node_is_file[node as usize] {
            // lognormal-ish blob sizes: median ~3 KB, heavy tail to ~1 MB; stable per node, jitter per version
            let u1 = ((h & 0xfffff) as f64 + 1.0) / 1048577.0;
            let u2 = (((h >> 20) & 0xfffff) as f64) / 1048576.0;
            let g = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
            let s = (3000.0 * (1.3 * g).exp()).clamp(60.0, 1_500_000.0);
            (key, s as u32, 0)
        } else {
            (key, 40 + 40 * self.children[zi][node as usize], 1)
        }
    }
    /// Objects needed to move the checkout of zone `zi` from commit `a` (None = nothing checked out) to `b`:
    /// level 0 commit, level 1 root tree, then changed dirs, then changed blobs.
    fn transition(&self, zi: usize, a: Option<u32>, b: u32, op: u32, out: &mut Vec<Access>) {
        let z = &self.zones[zi];
        let ckey = ((zi as u64) << 56) | (b as u64) | 3 << 62;
        out.push(Access { key: ckey, size: 220, level: 0, op, kind: 2 });
        let mut nodes: Vec<u32> = match a {
            Some(a) if a != b => {
                let (lo, hi) = (a.min(b), a.max(b));
                let mut v: Vec<u32> = z.commits[lo as usize + 1..=hi as usize].iter().flat_map(|c| c.changed.iter().copied()).collect();
                v.sort_unstable();
                v.dedup();
                v
            }
            Some(_) => vec![],
            None => (0..z.node_parent.len() as u32).filter(|&n| self.ver(zi, n, b) > 0 || n == 0).collect(),
        };
        if a.is_none() {
            // materialise only nodes that exist at commit b: files touched by some commit <= b, plus all dirs on their paths
            let mut keep = vec![false; z.node_parent.len()];
            keep[0] = true;
            for c in &z.commits[..=b as usize] {
                for &n in &c.changed {
                    keep[n as usize] = true;
                }
            }
            nodes.retain(|&n| keep[n as usize]);
        }
        // sort by depth so tree levels come first
        let mut keyed: Vec<(usize, u32)> = nodes.iter().map(|&n| (z.depth(n), n)).collect();
        keyed.sort_unstable();
        for (d, n) in keyed {
            let (key, size, kind) = self.obj(zi, n, b);
            out.push(Access { key, size, level: (1 + d).min(250) as u8 + if kind == 0 { 0 } else { 0 }, op, kind });
        }
    }
}

pub struct TraceCfg {
    pub kind: &'static str, // "checkout_sync" | "ci_fresh" | "checkout_sync_scan"
    pub ops: usize,
    pub subscribed: usize,
    pub seed: u64,
    /// after a sync, speculatively fetch the objects of the next `prefetch_ahead` commits of that zone (0 = off)
    pub prefetch_ahead: u32,
}

/// Generate an access trace for one client. Returns accesses in order.
pub fn gen_trace(w: &World, cfg: &TraceCfg) -> Vec<Access> {
    let mut r = Rng::new(cfg.seed);
    let nz = w.zones.len();
    // Zipf over zones for subscription
    let mut subs: Vec<usize> = vec![];
    while subs.len() < cfg.subscribed.min(nz) {
        let u = (r.next_u64() % 1000) as f64 / 1000.0;
        let z = ((u * u) * nz as f64) as usize % nz;
        if !subs.contains(&z) {
            subs.push(z);
        }
    }
    let mut out = vec![];
    // per-zone checked-out commit and remote head known
    let mut cur: Vec<Option<u32>> = vec![None; nz];
    let mut head: Vec<u32> = subs.iter().map(|&z| (w.zones[z].commits.len() as u32 / 2).max(1)).collect();
    let nsub = subs.len();
    for op in 0..cfg.ops as u32 {
        let si = (r.next_u64() as usize) % nsub;
        let zi = subs[si];
        let nc = w.zones[zi].commits.len() as u32;
        match cfg.kind {
            "ci_fresh" => {
                // each CI job: fresh checkout of the zone head, then the zone advances by a few commits
                let h = head[si];
                w.transition(zi, None, h, op, &mut out);
                head[si] = (h + 1 + (r.next_u64() % 4) as u32).min(nc - 1);
            }
            _ => {
                let x = r.next_u64() % 100;
                if cur[zi].is_none() {
                    w.transition(zi, None, head[si], op, &mut out); // zone add
                    cur[zi] = Some(head[si]);
                } else if x < 55 {
                    // sync: remote advances 1..5 commits, then checkout head
                    head[si] = (head[si] + 1 + (r.next_u64() % 5) as u32).min(nc - 1);
                    w.transition(zi, cur[zi], head[si], op, &mut out);
                    cur[zi] = Some(head[si]);
                    if cfg.prefetch_ahead > 0 {
                        let start = out.len();
                        w.transition(zi, Some(head[si]), (head[si] + cfg.prefetch_ahead).min(nc - 1), op, &mut out);
                        for a in &mut out[start..] {
                            a.kind = 9;
                        }
                    }
                } else if x < 85 {
                    // switch to a recent commit (branch switch / undo), temporal locality
                    let back = 1 + (r.next_u64() % 30) as u32;
                    let t = head[si].saturating_sub(back);
                    w.transition(zi, cur[zi], t, op, &mut out);
                    cur[zi] = Some(t);
                } else if x < 93 {
                    // bisect: ~10 jumps over all history
                    let mut c = cur[zi];
                    for _ in 0..10 {
                        let t = (r.next_u64() % head[si] as u64) as u32;
                        w.transition(zi, c, t, op, &mut out);
                        c = Some(t);
                    }
                    cur[zi] = c;
                } else {
                    // return to head
                    w.transition(zi, cur[zi], head[si], op, &mut out);
                    cur[zi] = Some(head[si]);
                }
                if cfg.kind == "checkout_sync_scan" && op % 400 == 399 {
                    // periodic one-shot full scan of one zone's whole history (fsck / log -p / export): never reused soon
                    let z = (r.next_u64() as usize) % nz;
                    for c in 0..w.zones[z].commits.len().min(1500) as u32 {
                        let ck = ((z as u64) << 56) | c as u64 | 3 << 62;
                        out.push(Access { key: ck, size: 220, level: 0, op, kind: 2 });
                        for &n in w.zones[z].commits[c as usize].changed.iter().take(6) {
                            let (key, size, kind) = w.obj(z, n, c);
                            out.push(Access { key, size, level: 1, op, kind });
                        }
                    }
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------ replay
#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub accesses: u64,
    pub hits: u64,
    pub bytes: u64,
    pub hit_bytes: u64,
    pub fetches: u64,
    pub fetch_bytes: u64,
    /// modelled network time in seconds: on-demand (1 object per RTT) and batched (per level, `par` streams)
    pub t_on_demand: f64,
    pub t_batched: f64,
    pub prefetch_fetches: u64,
    pub prefetch_bytes: u64,
    pub prefetch_useful: u64,
}
pub struct Net {
    pub rtt_s: f64,
    pub bw_bps: f64,
    pub par: u64,
}
pub fn replay(trace: &[Access], pol: &mut dyn Policy, net: &Net, from: usize, to: usize) -> Stats {
    let mut s = Stats::default();
    let mut pf: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut i = from;
    while i < to {
        // group = same (op, level)
        let (op, lv) = (trace[i].op, trace[i].level);
        let mut miss_n = 0u64;
        let mut miss_b = 0u64;
        while i < to && trace[i].op == op && trace[i].level == lv {
            let a = trace[i];
            if a.kind == 9 {
                // speculative fetch: background, off the critical path; counts as transferred bytes if it was not already cached
                if !pol.contains(a.key) {
                    pol.access(a.key, a.size);
                    s.prefetch_fetches += 1;
                    s.prefetch_bytes += a.size as u64;
                    pf.insert(a.key);
                }
                i += 1;
                continue;
            }
            s.accesses += 1;
            s.bytes += a.size as u64;
            if pol.access(a.key, a.size) {
                s.hits += 1;
                s.hit_bytes += a.size as u64;
                if pf.remove(&a.key) {
                    s.prefetch_useful += 1;
                }
            } else {
                pf.remove(&a.key);
                miss_n += 1;
                miss_b += a.size as u64;
                s.fetches += 1;
                s.fetch_bytes += a.size as u64;
            }
            i += 1;
        }
        s.t_on_demand += miss_n as f64 * net.rtt_s + miss_b as f64 / net.bw_bps;
        s.t_batched += miss_n.div_ceil(net.par) as f64 * net.rtt_s + miss_b as f64 / net.bw_bps;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tr(keys: &[u64]) -> Vec<Access> {
        keys.iter().enumerate().map(|(i, &k)| Access { key: k, size: 10, level: 0, op: i as u32, kind: 0 }).collect()
    }
    fn run(p: &mut dyn Policy, t: &[Access]) -> u64 {
        t.iter().filter(|a| p.access(a.key, a.size)).count() as u64
    }
    #[test]
    fn capacity_never_exceeded() {
        let mut r = Rng::new(1);
        let t: Vec<Access> = (0..20000).map(|i| Access { key: r.next_u64() % 500, size: 1 + (r.next_u64() % 40) as u32, level: 0, op: i, kind: 0 }).collect();
        for mut p in [Box::new(Lru::new(2000)) as Box<dyn Policy>, Box::new(Fifo::new(2000)), Box::new(Arc::new(2000)), Box::new(TinyLfu::new(2000, 20))] {
            for a in &t {
                p.access(a.key, a.size);
                assert!(p.used() <= 2000, "{} used {}", p.name(), p.used());
            }
        }
    }
    #[test]
    fn lru_basic() {
        let t = tr(&[1, 2, 3, 1, 2, 3, 4, 1]);
        assert_eq!(run(&mut Lru::new(30), &t), 3); // 1,2,3 hit second time; cap 3 objects; 4 evicts 1
        let mut l = Lru::new(30);
        assert_eq!(run(&mut l, &tr(&[1, 2, 3, 4, 1])), 0);
    }
    #[test]
    fn belady_beats_lru() {
        let mut r = Rng::new(2);
        let t: Vec<Access> = (0..5000).map(|i| Access { key: r.next_u64() % 100, size: 10, level: 0, op: i, kind: 0 }).collect();
        let (b, _) = belady(&t, 300);
        let l = run(&mut Lru::new(300), &t);
        assert!(b >= l);
    }
    #[test]
    fn scan_resistance() {
        // hot set of 20 keys + a scan of 1000 distinct keys: TinyLFU/ARC should keep the hot set, LRU should not
        let mut keys = vec![];
        for round in 0..30 {
            for _ in 0..2 {
                for h in 0..20 {
                    keys.push(h);
                }
            }
            for s in 0..40 {
                keys.push(1000 + round * 100 + s);
            }
        }
        let t = tr(&keys);
        let lru = run(&mut Lru::new(300), &t);
        let tiny = run(&mut TinyLfu::new(300, 10), &t);
        let arc = run(&mut Arc::new(300), &t);
        assert!(tiny > lru && arc > lru, "lru {lru} arc {arc} tiny {tiny}");
    }
}
