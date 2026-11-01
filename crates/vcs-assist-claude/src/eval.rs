//! Deterministic scoring harness: `vcs-assist-claude eval <dir>`.
//!
//! Each case is a subdirectory with files `ours`, `theirs`, `expected`, optional `base` (absent
//! means add/add) and `path` (one line, e.g. `src/a.rs`; picks the language), and for offline runs
//! `response.txt` (the recorded model text). Offline runs make no network call. With `live` set,
//! the model is called instead (and `response.txt` is ignored).

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

pub fn run(dir: &Path, live: Option<&Completer>) -> Result<Report, String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
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
        let outcome = match live {
            Some(c) => resolve(&req, c),
            None => match rd(&d.join("response.txt"))? {
                Some(t) => judge(&req, &t),
                None => {
                    rep.cases.push(CaseResult {
                        name,
                        verdict: Verdict::NoResponse,
                        parse: None,
                        detail: "no response.txt".into(),
                    });
                    continue;
                }
            },
        };
        let (verdict, parse, detail) = match outcome {
            Outcome::Proposal { resolution, parse, .. } => {
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
        });
    }
    Ok(rep)
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
        s.push_str(&format!(
            "cases: {n}\nexact-match: {}\nproposals passing validation: {}\nparse-valid (of proposals a parser could judge): {}\nrefused: {}, invalid: {}, no response: {}\n",
            rate(self.count(Verdict::Exact), n),
            rate(self.proposals(), n),
            rate(p, pc),
            self.count(Verdict::Refused),
            self.count(Verdict::Invalid),
            self.count(Verdict::NoResponse),
        ));
        s
    }
    pub fn json(&self) -> String {
        let (p, pc) = self.parse_stats();
        serde_json::json!({
            "cases": self.cases.len(),
            "exact": self.count(Verdict::Exact),
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
