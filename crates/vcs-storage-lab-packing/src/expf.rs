//! Experiment F: LSM-style write buffer + tiered vs leveled compaction over a content-addressed key stream.
//! Keys are the real object ids of the dataset in arrival order, sizes are real compressed sizes. Compaction is
//! simulated in memory (no files): every byte counted is a byte a real implementation would read or write.

use crate::dataset::*;
use crate::json::*;
use crate::layout::*;

type Key = [u8; 20];
#[derive(Clone)]
struct Run {
    e: Vec<(Key, u32)>,
    bytes: u64,
}
impl Run {
    fn from(mut e: Vec<(Key, u32, usize)>) -> Run {
        e.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.cmp(&b.2)));
        e.dedup_by(|b, a| a.0 == b.0);
        let e: Vec<(Key, u32)> = e.into_iter().map(|x| (x.0, x.1)).collect();
        let bytes = e.iter().map(|x| x.1 as u64).sum();
        Run { e, bytes }
    }
    fn min(&self) -> Key {
        self.e[0].0
    }
    fn max(&self) -> Key {
        self.e[self.e.len() - 1].0
    }
    fn has(&self, k: &Key) -> bool {
        self.e.binary_search_by(|x| x.0.cmp(k)).is_ok()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Pol {
    NoCompaction,
    Tiered(usize),
    Leveled(u64),
}

#[derive(Default)]
struct Stats {
    ingest_bytes: u64,
    flush_bytes: u64,
    compact_write: u64,
    compact_read: u64,
    puts: u64,
    gets: u64,
    peak_space: u64,
}

struct Lsm {
    pol: Pol,
    m: u64,
    levels: Vec<Vec<Run>>, // tiered: runs oldest..newest; leveled: L0 oldest..newest, L>=1 key-sorted disjoint files
    cursor: Vec<Key>,
    st: Stats,
}

fn merge(inputs: Vec<&Run>) -> Vec<(Key, u32)> {
    // inputs ordered newest first
    let mut all: Vec<(Key, u32, usize)> = vec![];
    for (r, run) in inputs.iter().enumerate() {
        all.extend(run.e.iter().map(|x| (x.0, x.1, r)));
    }
    Run::from(all).e
}

fn split(e: Vec<(Key, u32)>, fbytes: u64) -> Vec<Run> {
    let mut out = vec![];
    let (mut cur, mut b) = (vec![], 0u64);
    for x in e {
        b += x.1 as u64;
        cur.push(x);
        if b >= fbytes {
            out.push(Run { e: std::mem::take(&mut cur), bytes: b });
            b = 0;
        }
    }
    if !cur.is_empty() {
        out.push(Run { e: cur, bytes: b });
    }
    out
}

impl Lsm {
    fn new(pol: Pol, m: u64) -> Lsm {
        Lsm { pol, m, levels: vec![vec![]], cursor: vec![], st: Stats::default() }
    }
    fn total(&self) -> u64 {
        self.levels.iter().flatten().map(|r| r.bytes).sum()
    }
    fn lv(&mut self, i: usize) {
        while self.levels.len() <= i {
            self.levels.push(vec![]);
            self.cursor.push([0; 20]);
        }
        while self.cursor.len() < self.levels.len() {
            self.cursor.push([0; 20]);
        }
    }
    fn note_peak(&mut self, extra: u64) {
        let t = self.total() + extra;
        self.st.peak_space = self.st.peak_space.max(t);
    }
    fn flush(&mut self, run: Run) {
        self.st.flush_bytes += run.bytes;
        self.st.puts += 1;
        self.levels[0].push(run);
        self.note_peak(0);
        match self.pol {
            Pol::NoCompaction => {}
            Pol::Tiered(t) => {
                let mut i = 0;
                while self.levels[i].len() >= t {
                    let runs: Vec<Run> = std::mem::take(&mut self.levels[i]);
                    let rb: u64 = runs.iter().map(|r| r.bytes).sum();
                    let merged = merge(runs.iter().rev().collect());
                    let out = Run::from(merged.into_iter().map(|x| (x.0, x.1, 0)).collect());
                    self.st.compact_read += rb;
                    self.st.gets += runs.len() as u64;
                    self.st.compact_write += out.bytes;
                    self.st.puts += 1;
                    self.lv(i + 1);
                    // inputs still exist until the output is durable
                    let extra = rb.max(out.bytes);
                    self.levels[i] = runs;
                    self.note_peak(out.bytes);
                    self.levels[i].clear();
                    let _ = extra;
                    self.levels[i + 1].push(out);
                    i += 1;
                }
            }
            Pol::Leveled(t) => self.leveled(t),
        }
    }
    fn overlapping(files: &[Run], lo: Key, hi: Key) -> Vec<usize> {
        (0..files.len()).filter(|&i| files[i].max() >= lo && files[i].min() <= hi).collect()
    }
    fn compact_into(&mut self, from: Vec<Run>, from_level_is_l0: bool, to: usize) {
        let lo = from.iter().map(|r| r.min()).min().unwrap();
        let hi = from.iter().map(|r| r.max()).max().unwrap();
        self.lv(to);
        let ov = Self::overlapping(&self.levels[to], lo, hi);
        let mut ins: Vec<Run> = vec![];
        let mut newest_first: Vec<&Run> = from.iter().rev().collect();
        let _ = from_level_is_l0;
        let lower: Vec<Run> = ov.iter().map(|&i| self.levels[to][i].clone()).collect();
        newest_first.extend(lower.iter());
        let rb: u64 = newest_first.iter().map(|r| r.bytes).sum();
        let ng = newest_first.len() as u64;
        let merged = merge(newest_first);
        ins.extend(split(merged, self.m));
        let wb: u64 = ins.iter().map(|r| r.bytes).sum();
        self.st.compact_read += rb;
        self.st.gets += ng;
        self.st.compact_write += wb;
        self.st.puts += ins.len() as u64;
        self.note_peak(wb);
        for &i in ov.iter().rev() {
            self.levels[to].remove(i);
        }
        self.levels[to].extend(ins);
        self.levels[to].sort_by(|a, b| a.min().cmp(&b.min()));
    }
    fn leveled(&mut self, t: u64) {
        if self.levels[0].len() >= 4 {
            let l0 = std::mem::take(&mut self.levels[0]);
            self.compact_into(l0, true, 1);
        }
        let mut i = 1;
        while i < self.levels.len() {
            let target = self.m * t.pow(i as u32);
            while self.levels[i].iter().map(|r| r.bytes).sum::<u64>() > target {
                self.lv(i + 1);
                let cur = self.cursor[i];
                let idx = self.levels[i].iter().position(|r| r.min() > cur).unwrap_or(0);
                let f = self.levels[i].remove(idx);
                self.cursor[i] = f.max();
                self.compact_into(vec![f], false, i + 1);
            }
            i += 1;
        }
    }
    /// sorted runs a lookup must consider, newest first. Leveled levels count as one run each.
    fn lookup_order(&self) -> Vec<Vec<&Run>> {
        let mut v: Vec<Vec<&Run>> = vec![];
        match self.pol {
            Pol::Leveled(_) => {
                for r in self.levels[0].iter().rev() {
                    v.push(vec![r]);
                }
                for l in self.levels.iter().skip(1) {
                    if !l.is_empty() {
                        v.push(l.iter().collect());
                    }
                }
            }
            _ => {
                for l in &self.levels {
                    for r in l.iter().rev() {
                        v.push(vec![r]);
                    }
                }
            }
        }
        v
    }
}

fn has_in(group: &[&Run], k: &Key) -> bool {
    group.iter().any(|r| r.min() <= *k && *k <= r.max() && r.has(k))
}

pub fn run_f(ds: &Dataset, out: &Out) {
    let sz = sizes(ds);
    let objs: Vec<(Key, u32)> = ds.objs.iter().enumerate().map(|(i, o)| (o.sha, sz.z[i] + FRAME as u32)).collect();
    let unique: u64 = objs.iter().map(|x| x.1 as u64).sum();
    let bloom_fpr = 0.01; // MODEL: 10 bits/key bloom per sorted run
    for dup in [0.0f64, 0.25] {
        for m in [256u64 << 10, 1 << 20, 4 << 20, 16 << 20] {
            for pol in [Pol::NoCompaction, Pol::Tiered(4), Pol::Tiered(10), Pol::Leveled(4), Pol::Leveled(10)] {
                let mut rng = Rng(99);
                let mut l = Lsm::new(pol, m);
                let mut buf: Vec<(Key, u32, usize)> = vec![];
                let mut bufb = 0u64;
                let mut snapshots: Vec<(f64, f64, f64)> = vec![];
                let snap_every = (objs.len() / 8).max(1);
                let mut seq = 0usize;
                for (n, o) in objs.iter().enumerate() {
                    let mut batch = vec![*o];
                    if rng.f() < dup && n > 0 {
                        batch.push(objs[rng.below(n)]);
                    }
                    for (k, s) in batch {
                        l.st.ingest_bytes += s as u64;
                        seq += 1;
                        buf.push((k, s, usize::MAX - seq)); // newer = smaller rank
                        bufb += s as u64;
                    }
                    if bufb >= m {
                        let run = Run::from(std::mem::take(&mut buf));
                        bufb = 0;
                        l.lv(0);
                        l.flush(run);
                    }
                    if n % snap_every == snap_every - 1 {
                        snapshots.push(probe(&l, &objs[..=n], bloom_fpr, &mut Rng(5)));
                    }
                }
                if !buf.is_empty() {
                    let run = Run::from(std::mem::take(&mut buf));
                    l.lv(0);
                    l.flush(run);
                }
                let st = &l.st;
                let total = l.total();
                let (pnb, nb, runs) = probe(&l, &objs, bloom_fpr, &mut Rng(6));
                let mean = |f: &dyn Fn(&(f64, f64, f64)) -> f64| snapshots.iter().map(f).sum::<f64>() / snapshots.len().max(1) as f64;
                out.emit(
                    "F",
                    vec![
                        ("policy", J::s(match pol {
                            Pol::NoCompaction => "flush only (no compaction)".to_string(),
                            Pol::Tiered(t) => format!("tiered T={t}"),
                            Pol::Leveled(t) => format!("leveled T={t} (L0 trigger 4, file size = buffer)"),
                        })),
                        ("buffer_bytes", J::U(m)),
                        ("duplicate_put_rate", J::F(dup)),
                        ("unique_bytes", J::U(unique)),
                        ("ingested_bytes", J::U(st.ingest_bytes)),
                        ("final_stored_bytes", J::U(total)),
                        ("write_amp_total", J::F((st.flush_bytes + st.compact_write) as f64 / st.ingest_bytes as f64)),
                        ("write_amp_compaction_only", J::F(st.compact_write as f64 / st.ingest_bytes as f64)),
                        ("space_amp_final", J::F(total as f64 / unique as f64)),
                        ("peak_space_amp", J::F(st.peak_space as f64 / unique as f64)),
                        ("sorted_runs_final", J::F(runs)),
                        ("sorted_runs_mean_over_time", J::F(mean(&|x| x.2))),
                        ("read_amp_hit_probes_nobloom_final", J::F(pnb)),
                        ("read_amp_miss_probes_nobloom_final", J::F(nb)),
                        ("read_amp_hit_probes_bloom1pct_final", J::F(pnb_bloom(&l, &objs, bloom_fpr))),
                        ("read_amp_miss_probes_bloom1pct_final", J::F(runs * bloom_fpr)),
                        ("hit_probes_nobloom_mean_over_time", J::F(mean(&|x| x.0))),
                        ("miss_probes_nobloom_mean_over_time", J::F(mean(&|x| x.1))),
                        ("compaction_read_bytes", J::U(st.compact_read)),
                        ("objects_put", J::U(st.puts)),
                        ("compaction_gets", J::U(st.gets)),
                    ],
                );
            }
        }
    }
}

/// (mean probes for a present key without bloom, probes for an absent key without bloom, number of sorted runs)
fn probe(l: &Lsm, present: &[(Key, u32)], _fpr: f64, rng: &mut Rng) -> (f64, f64, f64) {
    let order = l.lookup_order();
    let runs = order.len() as f64;
    if present.is_empty() {
        return (0.0, runs, runs);
    }
    let mut tot = 0.0;
    let n = 600usize;
    for _ in 0..n {
        let k = present[rng.below(present.len())].0;
        let mut probes = 0.0;
        for g in &order {
            probes += 1.0;
            if has_in(g, &k) {
                break;
            }
        }
        tot += probes;
    }
    (tot / n as f64, runs, runs)
}

/// expected probes for a present key with a per-run bloom filter: the true run (1) plus fpr for each earlier run
fn pnb_bloom(l: &Lsm, present: &[(Key, u32)], fpr: f64) -> f64 {
    let order = l.lookup_order();
    let mut rng = Rng(11);
    let n = 600usize;
    let mut tot = 0.0;
    for _ in 0..n {
        let k = present[rng.below(present.len())].0;
        let mut before = 0.0;
        for g in &order {
            if has_in(g, &k) {
                break;
            }
            before += 1.0;
        }
        tot += 1.0 + before * fpr;
    }
    tot / n as f64
}
