//! Hit rate of the zone predictor on real histories (issue #465).
//!
//! `cargo run -p vcs-repo --release --example zone_hitrate -- <git-dir> [--label NAME] [--k 6] [--train 10]
//!    [--min-days 5] [--bucket-hours 24] [--cochange-refresh-hours 168] [--depth auto|1|2] [--max-commits N] [--tsv]`
//!
//! The question: if each person's *own past commits* were their usage profile, how many of the zones they touch
//! on a later day would `sync --auto` have chosen (the `k` best-ranked zones)? Nothing is simulated: the commits,
//! authors, times and touched paths are what the repository's history says (`git log --name-only`, git is needed
//! only by this example, never by the library).
//!
//! * A "zone" is a directory: the first path component, or the first two under a workspace-style directory
//!   (`crates/x`, `packages/x`, `src/x`, ...). Top-level files are the root zone and are not scored (root is always kept).
//! * A person's profile is one **edit** event per zone per commit, at the commit time. Real profiles also hold reads and
//!   builds, which a history cannot show: this measures the edit part only.
//! * Evaluation points (`--bucket-hours`, default one day; a history made in a few days needs smaller buckets): every bucket in which a person with at least `--train` earlier commits commits again.
//!   Truth = the zones they touched in that bucket. Hit rate = share of the truth that was in the hot set, averaged over days.
//! * Predictors: random, recency (most recently edited), frequency (most edits), ours (`zoneprofile::rank`, decayed
//!   frequency, no dependency edges, since manifests are not in a history log) and ours + co-change mined from everybody's
//!   earlier commits (zones committed with one author and message within 120 s; refreshed every `--cochange-refresh-hours`, default a week; never future data).
//! * The last row drops the `min_score` floor (0.05 of the busiest zone, there so that `--auto` does not download zones with no
//!   evidence) and fills the `k` slots whenever it has anything to rank: what the floor costs in hit rate.
//! * Cold start (a person's first `--train` commits) is not scored; it is what hints and the checked-in role are for.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::process::Command;
use vcs_repo::zoneprofile::{mine_cochange, rank, ChangeRec, Cochange, Event, Graph, Hints, Kind, Params, Profile};

const WORKSPACE_DIRS: [&str; 14] = [
    "crates", "packages", "apps", "libs", "services", "modules", "pkg", "plugins", "cmd", "internal", "src", "lib", "extensions", "examples",
];

struct Commit {
    author: String,
    ts: u64,
    msg: String,
    zones: BTreeSet<String>,
}

fn zone_of(path: &str, depth: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 {
        return None;
    }
    let two = match depth {
        "1" => false,
        "2" => parts.len() >= 3,
        _ => WORKSPACE_DIRS.contains(&parts[0]) && parts.len() >= 3,
    };
    Some(if two { format!("{}/{}", parts[0], parts[1]) } else { parts[0].to_owned() })
}

fn history(dir: &str, depth: &str, max: usize) -> Vec<Commit> {
    let mut cmd = Command::new("git");
    cmd.args(["-C", dir, "-c", "core.quotepath=off", "log", "--no-merges", "--no-renames", "--reverse", "--name-only", "--format=%x01%ae%x09%ct%x09%s"]);
    if max > 0 {
        cmd.arg(format!("--max-count={max}"));
    }
    let out = cmd.output().expect("git");
    assert!(out.status.success(), "git log failed: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let mut v: Vec<Commit> = Vec::new();
    for l in text.lines() {
        if let Some(h) = l.strip_prefix('\u{1}') {
            let mut f = h.splitn(3, '\t');
            let (a, t, m) = (f.next().unwrap_or(""), f.next().unwrap_or("0"), f.next().unwrap_or(""));
            v.push(Commit { author: a.to_owned(), ts: t.parse().unwrap_or(0), msg: m.to_owned(), zones: BTreeSet::new() });
        } else if !l.is_empty() {
            if let (Some(c), Some(z)) = (v.last_mut(), zone_of(l, depth)) {
                c.zones.insert(z);
            }
        }
    }
    v.retain(|c| !c.zones.is_empty());
    // git --reverse is topological; make time order explicit and stable
    v.sort_by_key(|c| c.ts);
    v
}

fn xorshift(x: &mut u64) -> u64 {
    *x ^= *x << 13;
    *x ^= *x >> 7;
    *x ^= *x << 17;
    *x
}

#[derive(Default, Clone)]
struct Acc {
    sum: f64,
    n: u32,
}

const NAMES: [&str; 6] = ["random", "recency", "frequency", "ours", "ours+cochange", "ours+cochange, filled to k"];

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}
fn pct(v: &mut Vec<f64>, q: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * q).round() as usize]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = args.first().expect("usage: zone_hitrate <git-dir> [options]").clone();
    let opt = |name: &str, d: &str| -> String {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| d.to_owned())
    };
    let k: usize = opt("--k", "6").parse().unwrap();
    let train: usize = opt("--train", "10").parse().unwrap();
    let min_days: usize = opt("--min-days", "5").parse().unwrap();
    let max_commits: usize = opt("--max-commits", "0").parse().unwrap();
    let depth = opt("--depth", "auto");
    let label = opt("--label", &dir);
    let bucket_s: u64 = (opt("--bucket-hours", "24").parse::<f64>().unwrap() * 3600.0) as u64;
    let refresh_s: u64 = (opt("--cochange-refresh-hours", "168").parse::<f64>().unwrap() * 3600.0) as u64;
    let tsv = args.iter().any(|a| a == "--tsv");

    let commits = history(&dir, &depth, max_commits);
    let all_zones: Vec<String> = commits.iter().flat_map(|c| c.zones.iter().cloned()).collect::<BTreeSet<_>>().into_iter().collect();
    let recs_all: Vec<ChangeRec> = commits
        .iter()
        .flat_map(|c| c.zones.iter().map(|z| ChangeRec { zone: z.clone(), author: c.author.clone(), message: c.msg.clone(), ts: c.ts }))
        .collect();
    // co-change mined from everything before the start of the week, newest 300 commits per zone (what the CLI reads)
    let mut cache: HashMap<u64, Cochange> = HashMap::new();
    let mut cochange_at = |week: u64| -> Cochange {
        cache
            .entry(week)
            .or_insert_with(|| {
                let cutoff = week * refresh_s;
                let mut per: BTreeMap<&str, usize> = BTreeMap::new();
                let recs: Vec<ChangeRec> = recs_all
                    .iter()
                    .rev()
                    .filter(|r| r.ts < cutoff)
                    .filter(|r| {
                        let n = per.entry(r.zone.as_str()).or_default();
                        *n += 1;
                        *n <= 300
                    })
                    .cloned()
                    .collect();
                mine_cochange(&recs, 120, 8)
            })
            .clone()
    };

    let mut by_author: BTreeMap<&str, Vec<&Commit>> = BTreeMap::new();
    for c in &commits {
        by_author.entry(c.author.as_str()).or_default().push(c);
    }
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let (mut micro, mut authors_used, mut days_total) = (vec![Acc::default(); NAMES.len()], 0usize, 0usize);
    let mut per_author: Vec<[f64; 6]> = Vec::new();
    for (_, cs) in &by_author {
        if cs.len() <= train {
            continue;
        }
        let mut acc = vec![Acc::default(); NAMES.len()];
        let mut events: Vec<Event> = Vec::new();
        let mut i = 0;
        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        let mut last: BTreeMap<String, u64> = BTreeMap::new();
        while i < cs.len() {
            let day = cs[i].ts / bucket_s;
            let mut j = i;
            let mut truth: BTreeSet<&str> = BTreeSet::new();
            while j < cs.len() && cs[j].ts / bucket_s == day {
                truth.extend(cs[j].zones.iter().map(String::as_str));
                j += 1;
            }
            if i >= train && !truth.is_empty() {
                let now_ms = day * bucket_s * 1000;
                let params = Params { now_ms, ..Params::default() };
                let prof = Profile { disabled: false, events: events.clone() };
                let hit = |hot: &[String]| truth.iter().filter(|z| hot.iter().any(|h| h == *z)).count() as f64 / truth.len() as f64;
                let mut results = [0.0f64; 6];
                // random
                let mut pool = all_zones.clone();
                let mut hot = Vec::new();
                for _ in 0..k.min(pool.len()) {
                    let at = (xorshift(&mut seed) % pool.len() as u64) as usize;
                    hot.push(pool.swap_remove(at));
                }
                results[0] = hit(&hot);
                // recency, frequency
                let mut r: Vec<(&String, &u64)> = last.iter().collect();
                r.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
                results[1] = hit(&r.iter().take(k).map(|(z, _)| (*z).clone()).collect::<Vec<_>>());
                let mut f: Vec<(&String, &u32)> = counts.iter().collect();
                f.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
                results[2] = hit(&f.iter().take(k).map(|(z, _)| (*z).clone()).collect::<Vec<_>>());
                // ours, ours + co-change
                let top = |g: &Graph, floor: f64| -> Vec<String> {
                    rank(&prof, &Hints::default(), g, &all_zones, &params)
                        .into_iter()
                        .filter(|x| x.score >= floor && x.score > 0.0)
                        .take(k)
                        .map(|x| x.zone)
                        .collect()
                };
                results[3] = hit(&top(&Graph::default(), params.min_score));
                let g = Graph { cochange: cochange_at(day * bucket_s / refresh_s), ..Graph::default() };
                results[4] = hit(&top(&g, params.min_score));
                results[5] = hit(&top(&g, 0.0));
                for (a, r) in acc.iter_mut().zip(results) {
                    a.sum += r;
                    a.n += 1;
                }
            }
            for c in &cs[i..j] {
                for z in &c.zones {
                    events.push(Event { ts_ms: c.ts * 1000, kind: Kind::Edit, zone: z.clone(), dir: String::new() });
                    *counts.entry(z.clone()).or_default() += 1;
                    last.insert(z.clone(), c.ts);
                }
            }
            i = j;
        }
        if acc[0].n as usize >= min_days {
            authors_used += 1;
            days_total += acc[0].n as usize;
            let mut row = [0.0; 6];
            for (n, a) in acc.iter().enumerate() {
                row[n] = a.sum / f64::from(a.n);
                micro[n].sum += a.sum;
                micro[n].n += a.n;
            }
            per_author.push(row);
        }
    }
    let zones_n = all_zones.len();
    if tsv {
        print!("{label}\t{}\t{zones_n}\t{k}\t{authors_used}\t{days_total}", commits.len());
        for m in &micro {
            print!("\t{:.4}", m.sum / f64::from(m.n.max(1)));
        }
        println!();
        return;
    }
    println!("## {label}");
    println!(
        "{} commits (merges excluded), {zones_n} zones, k = {k}, {authors_used} people with more than {train} commits and at least {min_days} scored buckets, {days_total} scored person-buckets",
        commits.len()
    );
    println!("\n| predictor | per bucket (all person-buckets) | per person, mean | per person p10..p90 |\n|---|---|---|---|");
    for (n, name) in NAMES.iter().enumerate() {
        let mut col: Vec<f64> = per_author.iter().map(|r| r[n]).collect();
        let (p10, p90) = (pct(&mut col, 0.1), pct(&mut col, 0.9));
        println!(
            "| {name} | {:.1}% | {:.1}% | {:.1}..{:.1}% |",
            100.0 * micro[n].sum / f64::from(micro[n].n.max(1)),
            100.0 * mean(&col),
            100.0 * p10,
            100.0 * p90
        );
    }
}
