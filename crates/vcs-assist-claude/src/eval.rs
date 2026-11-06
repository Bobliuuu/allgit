//! Deterministic scoring harness: `vcs-assist-claude eval <dir>`.
//!
//! Each case is a subdirectory with files `ours`, `theirs`, `expected`, optional `base` (absent
//! means add/add) and `path` (one line, e.g. `src/a.rs`; picks the language), and for offline runs
//! `response.txt` (the recorded model text). Offline runs make no network call. With `live` set,
//! the model is called instead (and `response.txt` is ignored).

use crate::cli::Metrics;
use crate::validate::ParseCheck;
use crate::{judge, resolve, Completer, Outcome, Req};
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Exact,
    Different,
    Refused,
    Invalid,
    NoResponse,
}

#[derive(Debug)]
pub struct CaseResult {
    pub name: String,
    pub verdict: Verdict,
    pub parse: Option<ParseCheck>,
    pub detail: String,
    /// Latency/tokens/cost of the live call (also read back from a recorded `metrics.json`).
    pub metrics: Option<Metrics>,
    /// Proposal equals the reference after trimming trailing whitespace and dropping blank lines.
    pub ws_equal: bool,
}

#[derive(Debug, Default)]
pub struct Report {
    pub cases: Vec<CaseResult>,
}

fn rd(p: &Path) -> Result<Option<String>, String> {
    match std::fs::read(p) {
        Ok(b) => String::from_utf8(b).map(Some).map_err(|_| format!("{}: not UTF-8", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

/// Where the answers come from.
pub enum Source<'a> {
    /// `response.txt` in each case directory.
    Recorded,
    /// The model. With `record`, the raw model text is written to `response.txt` in the case directory
    /// (also when validation rejects it, so a later offline run reproduces the verdict).
    Live {
        complete: &'a Completer<'a>,
        record: bool,
        /// Called after each case to fetch the metrics of the call just made.
        metrics: Option<&'a dyn Fn() -> Option<Metrics>>,
    },
    /// A deterministic mock provider.
    Mock(crate::baseline::Baseline),
}

pub fn run(dir: &Path, live: Option<&Completer>) -> Result<Report, String> {
    run_source(
        dir,
        match live {
            Some(c) => Source::Live {
                complete: c,
                record: false,
                metrics: None,
            },
            None => Source::Recorded,
        },
    )
}

/// Trailing whitespace trimmed and blank lines dropped: a looser comparison than byte equality.
fn norm(s: &str) -> Vec<&str> {
    s.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect()
}

/// Deterministic dev/test split by case name: about one case in five is held out as `test`.
pub fn is_test_case(name: &str) -> bool {
    let mut h: u32 = 0x811c9dc5;
    for b in name.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    h.is_multiple_of(5)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    All,
    Dev,
    Test,
}

pub fn run_source(dir: &Path, src: Source) -> Result<Report, String> {
    run_split(dir, src, Split::All)
}

pub fn run_split(dir: &Path, src: Source, split: Split) -> Result<Report, String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names.retain(|n| match split {
        Split::All => true,
        Split::Dev => !is_test_case(n),
        Split::Test => is_test_case(n),
    });
    let mut rep = Report::default();
    for name in names {
        let d = dir.join(&name);
        let need = |f: &str| rd(&d.join(f))?.ok_or_else(|| format!("{name}: missing file {f}"));
        let path = rd(&d.join("path"))?
            .map(|p| p.trim().to_owned())
            .unwrap_or_else(|| "file.txt".into());
        let req = Req {
            language: vcs_merge::assist::language_of(&path).to_owned(),
            path,
            base: rd(&d.join("base"))?,
            ours: need("ours")?,
            theirs: need("theirs")?,
        };
        let expected = need("expected")?;
        let mut case_metrics: Option<Metrics> = None;
        let outcome = match &src {
            Source::Mock(b) => judge(&req, &crate::baseline::respond(*b, &req)),
            Source::Live { complete, record, metrics } => {
                if *record {
                    let text = std::cell::RefCell::new(None);
                    let rec = |s: &str, u: &str, m: u32| {
                        let r = complete(s, u, m)?;
                        *text.borrow_mut() = Some(r.clone());
                        Ok(r)
                    };
                    let o = resolve(&req, &rec);
                    if let Some(t) = text.into_inner() {
                        std::fs::write(d.join("response.txt"), t).map_err(|e| format!("{name}: {e}"))?;
                    }
                    case_metrics = metrics.and_then(|f| f());
                    if let Some(m) = &case_metrics {
                        std::fs::write(d.join("metrics.json"), m.to_json().to_string()).map_err(|e| format!("{name}: {e}"))?;
                    }
                    o
                } else {
                    let o = resolve(&req, *complete);
                    case_metrics = metrics.and_then(|f| f());
                    o
                }
            }
            Source::Recorded => match rd(&d.join("response.txt"))? {
                Some(t) => {
                    case_metrics = rd(&d.join("metrics.json"))?
                        .and_then(|m| serde_json::from_str(&m).ok())
                        .map(|v| Metrics::from_json(&v));
                    judge(&req, &t)
                }
                None => {
                    rep.cases.push(CaseResult {
                        name,
                        verdict: Verdict::NoResponse,
                        parse: None,
                        detail: "no response.txt".into(),
                        metrics: None,
                        ws_equal: false,
                    });
                    continue;
                }
            },
        };
        let mut ws_equal = false;
        let (verdict, parse, detail) = match outcome {
            Outcome::Proposal { resolution, parse, .. } => {
                ws_equal = norm(&resolution) == norm(&expected);
                let v = if resolution == expected {
                    Verdict::Exact
                } else {
                    Verdict::Different
                };
                (v, Some(parse), String::new())
            }
            Outcome::Refused(r) => (Verdict::Refused, None, r),
            Outcome::Failed(r) => (Verdict::Invalid, None, r),
        };
        rep.cases.push(CaseResult {
            name,
            verdict,
            parse,
            detail,
            metrics: case_metrics,
            ws_equal,
        });
    }
    Ok(rep)
}

#[derive(Debug)]
pub struct MetricStats {
    pub calls: usize,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub mean_ms: u64,
    pub cost_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Report {
    pub fn count(&self, v: Verdict) -> usize {
        self.cases.iter().filter(|c| c.verdict == v).count()
    }
    /// Proposals that passed validation.
    pub fn proposals(&self) -> usize {
        self.count(Verdict::Exact) + self.count(Verdict::Different)
    }
    /// (parsed, checkable): proposals the structural parser could judge, and how many parsed.
    /// Validation already rejects non-parsing proposals, so rejected ones count as checkable misses
    /// only through `invalid_parse`.
    pub fn parse_stats(&self) -> (usize, usize) {
        let parsed = self.cases.iter().filter(|c| c.parse == Some(ParseCheck::Parsed)).count();
        let bad = self
            .cases
            .iter()
            .filter(|c| c.verdict == Verdict::Invalid && c.detail.contains("does not parse"))
            .count();
        (parsed, parsed + bad)
    }
    /// (calls with metrics, latency ms p50, p95, mean, total cost, input tokens, output tokens)
    pub fn metric_stats(&self) -> Option<MetricStats> {
        let ms: Vec<&Metrics> = self.cases.iter().filter_map(|c| c.metrics.as_ref()).collect();
        if ms.is_empty() {
            return None;
        }
        let mut lat: Vec<u64> = ms.iter().map(|m| m.latency_ms).collect();
        lat.sort_unstable();
        let q = |p: f64| lat[(((lat.len() - 1) as f64) * p).round() as usize];
        Some(MetricStats {
            calls: ms.len(),
            p50_ms: q(0.5),
            p95_ms: q(0.95),
            mean_ms: lat.iter().sum::<u64>() / lat.len() as u64,
            cost_usd: ms.iter().filter_map(|m| m.cost_usd).sum(),
            input_tokens: ms
                .iter()
                .map(|m| m.input_tokens + m.cache_read_tokens + m.cache_creation_tokens)
                .sum(),
            output_tokens: ms.iter().map(|m| m.output_tokens).sum(),
        })
    }
    pub fn text(&self) -> String {
        let mut s = String::new();
        for c in &self.cases {
            let v = match c.verdict {
                Verdict::Exact => "exact",
                Verdict::Different => "different",
                Verdict::Refused => "refused",
                Verdict::Invalid => "invalid",
                Verdict::NoResponse => "no-response",
            };
            s.push_str(&format!(
                "{:<28} {v}{}\n",
                c.name,
                if c.detail.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", c.detail)
                }
            ));
        }
        let n = self.cases.len();
        let rate = |a: usize, b: usize| {
            if b == 0 {
                "n/a".to_owned()
            } else {
                format!("{:.1}% ({a}/{b})", 100.0 * a as f64 / b as f64)
            }
        };
        let (p, pc) = self.parse_stats();
        let ws = self.cases.iter().filter(|c| c.ws_equal).count();
        s.push_str(&format!(
            "cases: {n}\nexact-match: {}\nmatch ignoring blank lines and trailing whitespace: {}\nproposals passing validation: {}\nparse-valid (of proposals a parser could judge): {}\nrefused: {}, invalid: {}, no response: {}\n",
            rate(self.count(Verdict::Exact), n),
            rate(ws, n),
            rate(self.proposals(), n),
            rate(p, pc),
            self.count(Verdict::Refused),
            self.count(Verdict::Invalid),
            self.count(Verdict::NoResponse),
        ));
        if let Some(m) = self.metric_stats() {
            s.push_str(&format!(
                "latency: p50 {} ms, p95 {} ms, mean {} ms over {} calls\ntokens: {} in, {} out; cost: ${:.4} (${:.4}/call)\n",
                m.p50_ms,
                m.p95_ms,
                m.mean_ms,
                m.calls,
                m.input_tokens,
                m.output_tokens,
                m.cost_usd,
                m.cost_usd / m.calls as f64
            ));
        }
        s
    }
    pub fn json(&self) -> String {
        let (p, pc) = self.parse_stats();
        let ms = self.metric_stats();
        serde_json::json!({
            "latency_p50_ms": ms.as_ref().map(|m| m.p50_ms),
            "latency_p95_ms": ms.as_ref().map(|m| m.p95_ms),
            "latency_mean_ms": ms.as_ref().map(|m| m.mean_ms),
            "cost_usd": ms.as_ref().map(|m| m.cost_usd),
            "input_tokens": ms.as_ref().map(|m| m.input_tokens),
            "output_tokens": ms.as_ref().map(|m| m.output_tokens),
            "cases": self.cases.len(),
            "exact": self.count(Verdict::Exact),
            "ws_equal": self.cases.iter().filter(|c| c.ws_equal).count(),
            "different": self.count(Verdict::Different),
            "refused": self.count(Verdict::Refused),
            "invalid": self.count(Verdict::Invalid),
            "no_response": self.count(Verdict::NoResponse),
            "parse_checkable": pc,
            "parse_valid": p,
            "exact_rate": if self.cases.is_empty() { 0.0 } else { self.count(Verdict::Exact) as f64 / self.cases.len() as f64 },
            "parse_valid_rate": if pc == 0 { serde_json::Value::Null } else { serde_json::json!(p as f64 / pc as f64) },
        })
        .to_string()
    }
}
