//! Mock providers: deterministic stand-ins for the model, used to measure the harness and to give a
//! floor to compare a real model against. No network, no key.
//!
//! * `xours` / `xtheirs`: the line merge with every conflicting region taken from one side (what
//!   `git merge -X ours|theirs` does). Always a syntactically plausible answer, never reasons.
//! * `refuse`: always declines (the cautious floor).
//!
//! Their answers go through the same extraction and validation as a real model's text.

use crate::Req;
use vcs_merge::{merge3, Algorithm, Hunk, TextMerge};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Baseline {
    XOurs,
    XTheirs,
    Refuse,
}

impl Baseline {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "xours" => Some(Self::XOurs),
            "xtheirs" => Some(Self::XTheirs),
            "refuse" => Some(Self::Refuse),
            _ => None,
        }
    }
}

/// The text a model following this baseline would answer with.
pub fn respond(b: Baseline, req: &Req) -> String {
    if b == Baseline::Refuse {
        return "<refusal>mock provider: always refuses</refusal>".into();
    }
    let base = req.base.as_deref().unwrap_or("");
    let file = match merge3(Algorithm::Myers, base.as_bytes(), req.ours.as_bytes(), req.theirs.as_bytes()) {
        TextMerge::Clean(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        TextMerge::Conflicted(hunks) => {
            let mut out: Vec<u8> = Vec::new();
            for h in hunks {
                match h {
                    Hunk::Clean(ls) => ls.iter().for_each(|l| out.extend_from_slice(l)),
                    Hunk::Conflict { left, right, .. } => {
                        let side = if b == Baseline::XOurs { left } else { right };
                        side.iter().for_each(|l| out.extend_from_slice(l));
                    }
                }
            }
            String::from_utf8_lossy(&out).into_owned()
        }
    };
    format!("<explanation>mock provider: conflicting regions taken from one side</explanation>\n<resolved_file>\n{file}\n</resolved_file>")
}
