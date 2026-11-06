//! Simulated work sessions: how often is a zone the developer touches next already local?
//! Deterministic (seeded). The access model below is MY assumption, not measured behaviour of real
//! developers; the numbers say how the predictor behaves under that model (docs/research/zone-profile-bench.md).
//! `ZP_SIM_DEVS=200 cargo test -p vcs-repo --test zoneprofile_sim -- --nocapture` prints the table.
use std::collections::{BTreeMap, BTreeSet};
use vcs_repo::zoneprofile::{rank, Event, Graph, Hints, Kind, Params, Profile};

const DAY: u64 = 86_400_000;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

fn graph(z: usize, rng: &mut Rng) -> Graph {
    let mut g = Graph::default();
    for i in 1..z {
        for _ in 0..rng.below(4) {
            g.add(&name(i), &name(rng.below(i)));
        }
    }
    g
}
fn name(i: usize) -> String {
    format!("z{i:02}")
}
fn closure(g: &Graph, seeds: &[usize], depth: usize) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = seeds.iter().map(|s| name(*s)).collect();
    let mut frontier: Vec<String> = seen.iter().cloned().collect();
    for _ in 0..depth {
        let mut next = vec![];
        for f in &frontier {
            for t in g.deps.get(f).into_iter().flatten() {
                if seen.insert(t.clone()) {
                    next.push(t.clone());
                }
            }
        }
        frontier = next;
    }
    seen
}

/// One developer: sessions of events over `days` days; a home set that drifts.
/// `hidden_deps`: accesses to dependency zones (the compiler reading them) are not recorded, only
/// the developer's own commands are. Returns (zone, kind, recorded).
fn sessions(z: usize, days: usize, g: &Graph, hidden_deps: bool, rng: &mut Rng) -> Vec<Vec<(String, Kind, bool)>> {
    let mut home: Vec<usize> = (0..3).map(|_| rng.below(z)).collect();
    let mut out = vec![];
    for d in 0..days {
        if d > 0 && d % 20 == 0 {
            let k = rng.below(3);
            home[k] = rng.below(z); // task drift: one home zone is replaced
        }
        let deps: Vec<String> = closure(g, &home, 2).into_iter().collect();
        let mut s = vec![];
        for _ in 0..(4 + rng.below(5)) {
            let (zone, kind, rec) = if rng.chance(65) {
                (
                    name(home[rng.below(home.len())]),
                    if rng.chance(40) { Kind::Edit } else { Kind::Read },
                    true,
                )
            } else if rng.chance(55) {
                (deps[rng.below(deps.len())].clone(), Kind::Build, !hidden_deps)
            } else {
                (name(rng.below(z)), Kind::Read, true) // wandering: any zone
            };
            s.push((zone, kind, rec));
        }
        out.push(s);
    }
    out
}

#[derive(Clone, Copy)]
enum Pred {
    Random,
    Frequency,
    Recency,
    OursNoDeps,
    Ours,
    OursDep(f64),
    #[allow(dead_code)]
    Oracle,
}

#[allow(clippy::too_many_arguments)]
fn hot_set(
    p: Pred,
    k: usize,
    z: usize,
    hist: &[Vec<(String, Kind, bool)>],
    g: &Graph,
    now_day: u64,
    rng: &mut Rng,
    truth: &BTreeSet<String>,
) -> BTreeSet<String> {
    let names: Vec<String> = (0..z).map(name).collect();
    let events: Vec<Event> = hist
        .iter()
        .enumerate()
        .flat_map(|(d, s)| {
            s.iter().enumerate().filter(|(_, e)| e.2).map(move |(i, (zn, kd, _))| Event {
                ts_ms: d as u64 * DAY + i as u64 * 60_000,
                kind: *kd,
                zone: zn.clone(),
                dir: String::new(),
            })
        })
        .collect();
    match p {
        Pred::Random => {
            let mut s = BTreeSet::new();
            while s.len() < k {
                s.insert(name(rng.below(z)));
            }
            s
        }
        Pred::Frequency => {
            let mut c: BTreeMap<&str, usize> = BTreeMap::new();
            for e in &events {
                *c.entry(e.zone.as_str()).or_default() += 1;
            }
            let mut v: Vec<_> = c.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            v.into_iter().take(k).map(|(n, _)| n.to_owned()).collect()
        }
        Pred::Recency => {
            let mut s = BTreeSet::new();
            for e in events.iter().rev() {
                s.insert(e.zone.clone());
                if s.len() == k {
                    break;
                }
            }
            s
        }
        Pred::OursNoDeps | Pred::Ours | Pred::OursDep(_) => {
            let prof = Profile { disabled: false, events };
            let mut params = Params {
                now_ms: now_day * DAY,
                ..Params::default()
            };
            if let Pred::OursDep(f) = p {
                params.dep_factor = f;
            }
            let gr = if matches!(p, Pred::OursNoDeps) {
                Graph::default()
            } else {
                g.clone()
            };
            rank(&prof, &Hints::default(), &gr, &names, &params)
                .into_iter()
                .filter(|r| r.score >= params.min_score)
                .take(k)
                .map(|r| r.zone)
                .collect()
        }
        Pred::Oracle => truth.iter().take(k).cloned().collect(),
    }
}

fn run(devs: usize, z: usize, k: usize, days: usize, warmup: usize, hidden: bool) -> Vec<(&'static str, Vec<f64>)> {
    let preds = [
        ("random", Pred::Random),
        ("recency only", Pred::Recency),
        ("frequency only", Pred::Frequency),
        ("ours without dependencies", Pred::OursNoDeps),
        ("ours (default, dep 0.3)", Pred::Ours),
        ("ours (dep 0.45)", Pred::OursDep(0.45)),
        ("ours (dep 0.15)", Pred::OursDep(0.15)),
    ];
    let mut res: Vec<(&'static str, Vec<f64>)> = preds.iter().map(|(n, _)| (*n, vec![])).collect();
    for dev in 0..devs {
        let mut rng = Rng(0x9E3779B97F4A7C15 ^ (dev as u64 + 1).wrapping_mul(0x2545F4914F6CDD1D));
        let g = graph(z, &mut rng);
        let ss = sessions(z, days, &g, hidden, &mut rng);
        let mut hits = vec![0.0f64; preds.len()];
        let mut total = 0.0;
        for t in warmup..days {
            let accessed: BTreeSet<&String> = ss[t].iter().map(|(z, _, _)| z).collect();
            total += accessed.len() as f64;
            for (i, (_, p)) in preds.iter().enumerate() {
                let hot = hot_set(*p, k, z, &ss[..t], &g, t as u64, &mut rng, &BTreeSet::new());
                hits[i] += accessed.iter().filter(|a| hot.contains(**a)).count() as f64;
            }
        }
        for (i, h) in hits.iter().enumerate() {
            res[i].1.push(h / total);
        }
    }
    res
}

fn stats(v: &[f64]) -> (f64, f64, f64) {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = s.iter().sum::<f64>() / s.len() as f64;
    (mean, s[s.len() / 10], s[s.len() * 9 / 10])
}

#[test]
fn prediction_hit_rate_on_simulated_sessions() {
    let devs: usize = std::env::var("ZP_SIM_DEVS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let (z, days) = (50, 60);
    for (k, warmup, hidden) in [
        (4usize, 10usize, false),
        (6, 10, false),
        (8, 10, false),
        (12, 10, false),
        (6, 4, false),
        (4, 10, true),
        (6, 10, true),
        (8, 10, true),
        (12, 10, true),
    ] {
        let r = run(devs, z, k, days, warmup, hidden);
        println!("zones={z} budget k={k} hot zones, {devs} simulated developers, {} evaluated sessions each after {warmup} training session(s), dependency accesses {}", days - warmup, if hidden { "NOT recorded" } else { "recorded" });
        println!("| predictor | mean hit rate | p10 | p90 |");
        println!("|---|---|---|---|");
        for (n, v) in &r {
            let (m, lo, hi) = stats(v);
            println!("| {n} | {:.1}% | {:.1}% | {:.1}% |", m * 100.0, lo * 100.0, hi * 100.0);
        }
        let get = |n: &str| stats(&r.iter().find(|(x, _)| *x == n).unwrap().1).0;
        assert!(
            get("ours (default, dep 0.3)") > get("random") * 2.0,
            "the predictor must clearly beat random under this model"
        );
    }
}

#[test]
fn simulation_is_deterministic() {
    let a = run(5, 30, 6, 30, 5, true);
    let b = run(5, 30, 6, 30, 5, true);
    assert_eq!(
        a.iter().map(|x| x.1.clone()).collect::<Vec<_>>(),
        b.iter().map(|x| x.1.clone()).collect::<Vec<_>>()
    );
}
