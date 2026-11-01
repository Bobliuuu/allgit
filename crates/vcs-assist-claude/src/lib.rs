//! `vcs-assist-claude`: the assistant side of `vcs merge --assist`. Reads one conflict request
//! (JSON, see docs/merge.md) and answers with a validated proposal, or refuses. The file contents
//! are sent to the Anthropic API: see docs/assist.md.

pub mod api;
pub mod eval;
pub mod prompt;
pub mod validate;

use validate::{Answer, ParseCheck};

/// One conflict, as read from the protocol request.
#[derive(Clone, Debug)]
pub struct Req {
    pub path: String,
    pub language: String,
    pub base: Option<String>,
    pub ours: String,
    pub theirs: String,
}

/// Parse the request JSON; unknown fields are ignored.
pub fn parse_request(bytes: &[u8]) -> Result<Req, String> {
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| format!("request is not JSON: {e}"))?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_owned);
    if v.get("version").and_then(|x| x.as_u64()) != Some(1) {
        return Err("unsupported request version (expected 1)".into());
    }
    let base = match v.get("base") {
        Some(serde_json::Value::Null) | None => None,
        Some(serde_json::Value::String(b)) => Some(b.clone()),
        Some(_) => return Err("\"base\" must be a string or null".into()),
    };
    let path = s("path").ok_or("request has no \"path\"")?;
    Ok(Req {
        language: s("language").unwrap_or_else(|| vcs_merge::assist::language_of(&path).to_owned()),
        path,
        base,
        ours: s("ours").ok_or("request has no \"ours\"")?,
        theirs: s("theirs").ok_or("request has no \"theirs\"")?,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Proposal {
        resolution: String,
        explanation: String,
        parse: ParseCheck,
    },
    /// The model (or a size limit) declined.
    Refused(String),
    /// The answer failed validation, or the call failed.
    Failed(String),
}

/// The model call: (system, user, max_tokens) -> model text.
pub type Completer<'a> = dyn Fn(&str, &str, u32) -> Result<String, api::ApiError> + 'a;

/// Turn a model text into an outcome (extraction plus all validation). Pure.
pub fn judge(req: &Req, model_text: &str) -> Outcome {
    match validate::extract(model_text, req) {
        Err(e) => Outcome::Failed(e),
        Ok(Answer::Refused(r)) => Outcome::Refused(if r.is_empty() { "the model declined".into() } else { r }),
        Ok(Answer::Resolved { file, explanation }) => match validate::check(req, &file) {
            Ok(parse) => Outcome::Proposal {
                resolution: file,
                explanation,
                parse,
            },
            Err(e) => Outcome::Failed(format!("proposal rejected: {e}")),
        },
    }
}

/// Size gate, prompt, call, judge.
pub fn resolve(req: &Req, complete: &Completer) -> Outcome {
    let total = req.base.as_ref().map_or(0, String::len) + req.ours.len() + req.theirs.len();
    if total > validate::MAX_INPUT_BYTES {
        return Outcome::Refused(format!(
            "file is too large to send ({total} bytes, limit {})",
            validate::MAX_INPUT_BYTES
        ));
    }
    if req.ours.contains('\0') || req.theirs.contains('\0') {
        return Outcome::Refused("binary content".into());
    }
    let longest = req.ours.len().max(req.theirs.len());
    let max_tokens = (longest / 2 + 2048).clamp(4096, 64_000) as u32;
    match complete(prompt::SYSTEM, &prompt::user_message(req), max_tokens) {
        Ok(text) => judge(req, &text),
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// The protocol response JSON for a proposal.
pub fn response_json(resolution: &str, explanation: &str) -> String {
    serde_json::json!({"resolution": resolution, "explanation": explanation}).to_string()
}
