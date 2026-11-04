//! Extraction of the model's answer and the checks a proposal must pass before it is printed.

use crate::Req;
use vcs_merge::{merge3, Algorithm, Hunk, TextMerge};

/// Largest resolved file accepted, in bytes. The host protocol allows 1 MiB of JSON; leave room for escaping.
pub const MAX_RESOLUTION_BYTES: usize = 512 * 1024;
/// Largest combined input (base + ours + theirs) that is sent to the API at all.
pub const MAX_INPUT_BYTES: usize = 200 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    Resolved { file: String, explanation: String },
    Refused(String),
}

/// How the proposal fared against the structural parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseCheck {
    /// Parsed without error.
    Parsed,
    /// No parser for this language in this build.
    NoParser,
    /// Ours or theirs already fails to parse, so a parse failure would not be the model's fault.
    InputsUnparsable,
}

fn between<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let a = s.find(&open)? + open.len();
    let b = s.rfind(&close)?;
    (b >= a).then(|| &s[a..b])
}

/// Pull the answer out of the model's text.
pub fn extract(text: &str, req: &Req) -> Result<Answer, String> {
    if let Some(r) = between(text, "refusal") {
        if !text.contains("<resolved_file>") {
            return Ok(Answer::Refused(r.trim().to_owned()));
        }
    }
    let Some(raw) = between(text, "resolved_file") else {
        return Err("model answer has neither a complete <resolved_file> nor a <refusal>".into());
    };
    let mut file = raw
        .strip_prefix("\r\n")
        .or_else(|| raw.strip_prefix('\n'))
        .unwrap_or(raw)
        .to_owned();
    // The line break before the closing tag is framing.
    if file.ends_with('\n') {
        file.pop();
        if file.ends_with('\r') && raw.contains("\r\n") {
            file.pop();
        }
    }
    // Models often drop the framing/blank line for files that end in a newline; restore it when
    // both sides end with one (the base is not consulted: a side that ends with one decides).
    if !file.ends_with('\n') && req.ours.ends_with('\n') && req.theirs.ends_with('\n') {
        file.push('\n');
    }
    let explanation = between(text, "explanation").map(|e| e.trim().to_owned()).unwrap_or_default();
    Ok(Answer::Resolved { file, explanation })
}

fn lines_concat(ls: &[Vec<u8>]) -> Vec<u8> {
    ls.concat()
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from);
    }
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// The non-conflicting parts of the line merge must survive byte for byte and in order; only the
/// regions between them may differ.
pub fn preserves_stable_hunks(req: &Req, resolution: &str) -> Result<(), String> {
    let base = req.base.as_deref().unwrap_or("");
    let hunks = match merge3(Algorithm::Myers, base.as_bytes(), req.ours.as_bytes(), req.theirs.as_bytes()) {
        TextMerge::Clean(bytes) => {
            return if bytes == resolution.as_bytes() {
                Ok(())
            } else {
                Err("the sides merge cleanly but the resolution differs from the clean merge".into())
            };
        }
        TextMerge::Conflicted(h) => h,
    };
    // Segments: stable text pieces, with a gap (a conflict) between consecutive ones.
    let mut pieces: Vec<Vec<u8>> = vec![Vec::new()];
    for h in &hunks {
        match h {
            Hunk::Clean(ls) => pieces.last_mut().unwrap().extend(lines_concat(ls)),
            Hunk::Conflict { .. } => pieces.push(Vec::new()),
        }
    }
    let out = resolution.as_bytes();
    let first = &pieces[0];
    if !out.starts_with(first) {
        return Err("text before the first conflicting region was changed".into());
    }
    let mut pos = first.len();
    let last_i = pieces.len() - 1;
    for p in &pieces[1..last_i] {
        match find(out, p, pos) {
            Some(i) => pos = i + p.len(),
            None => return Err("a non-conflicting region was changed or reordered".into()),
        }
    }
    let last = &pieces[last_i];
    if last_i > 0 && (out.len() < pos + last.len() || !out.ends_with(last)) {
        return Err("text after the last conflicting region was changed".into());
    }
    Ok(())
}

#[cfg(any(
    feature = "structural",
    feature = "structural-python",
    feature = "structural-typescript",
    feature = "structural-go"
))]
fn parse_check(req: &Req, resolution: &str) -> Result<ParseCheck, String> {
    use vcs_merge::structural::{lang_for_path, parses_as};
    let Some(lang) = lang_for_path(&req.path) else {
        return Ok(ParseCheck::NoParser);
    };
    let base_ok = req.base.as_ref().is_none_or(|b| parses_as(lang, b.as_bytes()));
    if !(base_ok && parses_as(lang, req.ours.as_bytes()) && parses_as(lang, req.theirs.as_bytes())) {
        return Ok(ParseCheck::InputsUnparsable);
    }
    if parses_as(lang, resolution.as_bytes()) {
        Ok(ParseCheck::Parsed)
    } else {
        Err(format!("the resolution does not parse as {}", req.language))
    }
}

#[cfg(not(any(
    feature = "structural",
    feature = "structural-python",
    feature = "structural-typescript",
    feature = "structural-go"
)))]
fn parse_check(_: &Req, _: &str) -> Result<ParseCheck, String> {
    Ok(ParseCheck::NoParser)
}

/// All checks. `Err` is a one-line reason; the proposal must then not be printed.
pub fn check(req: &Req, resolution: &str) -> Result<ParseCheck, String> {
    if resolution.is_empty() && !(req.ours.is_empty() && req.theirs.is_empty()) {
        return Err("the resolution is empty".into());
    }
    if resolution.len() > MAX_RESOLUTION_BYTES {
        return Err(format!("the resolution is too large ({} bytes)", resolution.len()));
    }
    if resolution.contains('\0') {
        return Err("the resolution contains a NUL byte".into());
    }
    if vcs_merge::assist::has_conflict_markers(resolution) {
        return Err("the resolution contains conflict markers".into());
    }
    preserves_stable_hunks(req, resolution)?;
    parse_check(req, resolution)
}
