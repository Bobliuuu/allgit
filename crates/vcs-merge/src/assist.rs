//! Assisted-merge protocol: the JSON a conflict is sent to an external assistant as, and the
//! strict validation of what comes back. Pure: no processes, no filesystem. No model is bundled.
//!
//! Request (one JSON object on the assistant's stdin):
//! `{"version":1,"path":"src/a.rs","language":"rust","base":"..","ours":"..","theirs":".."}`
//! (`base` is `null` for an add/add conflict).
//!
//! Response (one JSON object on stdout): `{"resolution":"<full file text>","explanation":"<optional>"}`.
//! Unknown fields are ignored. Anything else is rejected with a [`Reject`] reason.

use crate::diff::{matches, Algorithm};
use std::collections::BTreeMap;

/// Protocol version carried in every request.
pub const VERSION: u32 = 1;
/// Largest response accepted from an assistant, in bytes (stdout is read up to this plus one).
pub const MAX_RESPONSE_BYTES: usize = 1 << 20;
/// Largest explanation kept, in bytes; longer ones are truncated.
pub const MAX_EXPLANATION_BYTES: usize = 4096;
const MAX_DEPTH: usize = 32;

/// One conflict as the assistant sees it.
pub struct Request<'a> {
    pub path: &'a str,
    pub language: &'a str,
    pub base: Option<&'a str>,
    pub ours: &'a str,
    pub theirs: &'a str,
}

/// A validated proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub resolution: String,
    pub explanation: String,
}

/// Why a response was ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reject {
    TooLarge(usize),
    NotUtf8,
    NotJson(String),
    NotObject,
    MissingResolution,
    ResolutionNotString,
    ConflictMarkers,
    Nul,
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reject::TooLarge(n) => write!(f, "response too large ({n} bytes)"),
            Reject::NotUtf8 => write!(f, "response is not UTF-8"),
            Reject::NotJson(e) => write!(f, "response is not valid JSON ({e})"),
            Reject::NotObject => write!(f, "response is not a JSON object"),
            Reject::MissingResolution => write!(f, "response has no \"resolution\" field"),
            Reject::ResolutionNotString => write!(f, "\"resolution\" is not a string"),
            Reject::ConflictMarkers => write!(f, "resolution still contains conflict markers"),
            Reject::Nul => write!(f, "resolution contains a NUL byte"),
        }
    }
}

/// Language name from a file extension (`"text"` when unknown).
pub fn language_of(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "jsx" => "jsx",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        "java" => "java",
        "cs" => "csharp",
        "rb" => "ruby",
        "sh" => "shell",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" => "markdown",
        "html" => "html",
        "css" => "css",
        _ => "text",
    }
}

/// Encode a request as compact JSON.
pub fn encode_request(r: &Request) -> String {
    let mut o = String::from("{\"version\":");
    o.push_str(&VERSION.to_string());
    o.push_str(",\"path\":");
    write_str(&mut o, r.path);
    o.push_str(",\"language\":");
    write_str(&mut o, r.language);
    o.push_str(",\"base\":");
    match r.base {
        Some(b) => write_str(&mut o, b),
        None => o.push_str("null"),
    }
    o.push_str(",\"ours\":");
    write_str(&mut o, r.ours);
    o.push_str(",\"theirs\":");
    write_str(&mut o, r.theirs);
    o.push('}');
    o
}

/// JSON-escape `s` into `o`, with quotes.
pub fn write_str(o: &mut String, s: &str) {
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
}

/// True when any line of `text` starts like a conflict marker (`<<<<<<<`, `|||||||`, `=======`, `>>>>>>>`).
pub fn has_conflict_markers(text: &str) -> bool {
    text.lines().any(|l| {
        ["<<<<<<<", "|||||||", "=======", ">>>>>>>"]
            .iter()
            .any(|m| l.starts_with(m) && l[m.len()..].chars().next().is_none_or(|c| c == ' '))
    })
}

/// Validate raw assistant output. Never panics on any input.
pub fn parse_response(bytes: &[u8]) -> Result<Proposal, Reject> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(Reject::TooLarge(bytes.len()));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| Reject::NotUtf8)?;
    let v = parse_json(text).map_err(Reject::NotJson)?;
    let Json::Object(m) = v else { return Err(Reject::NotObject) };
    let res = match m.get("resolution") {
        None => return Err(Reject::MissingResolution),
        Some(Json::Str(s)) => s.clone(),
        Some(_) => return Err(Reject::ResolutionNotString),
    };
    if res.contains('\0') {
        return Err(Reject::Nul);
    }
    if has_conflict_markers(&res) {
        return Err(Reject::ConflictMarkers);
    }
    let mut explanation = match m.get("explanation") {
        Some(Json::Str(s)) => s.clone(),
        _ => String::new(),
    };
    if explanation.len() > MAX_EXPLANATION_BYTES {
        let mut n = MAX_EXPLANATION_BYTES;
        while !explanation.is_char_boundary(n) {
            n -= 1;
        }
        explanation.truncate(n);
    }
    Ok(Proposal {
        resolution: res,
        explanation,
    })
}

/// Line diff of `old` to `new`: lines prefixed `-`, `+` or two spaces (unchanged lines far from a
/// change are elided with `...`).
pub fn diff_lines(old: &str, new: &str) -> String {
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    let mut tab: BTreeMap<String, u32> = BTreeMap::new();
    let mut num = |s: &str| -> u32 {
        let n = tab.len() as u32;
        *tab.entry(s.to_owned()).or_insert(n)
    };
    let ai: Vec<u32> = a.iter().map(|s| num(s)).collect();
    let bi: Vec<u32> = b.iter().map(|s| num(s)).collect();
    let m = matches(Algorithm::Myers, &ai, &bi);
    // (tag, text) rows
    let mut rows: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    for (mi, mj) in m.into_iter().chain(std::iter::once((a.len(), b.len()))) {
        rows.extend(a[i..mi].iter().map(|l| ('-', *l)));
        rows.extend(b[j..mj].iter().map(|l| ('+', *l)));
        if mi < a.len() {
            rows.push((' ', a[mi]));
        }
        i = mi + 1;
        j = mj + 1;
    }
    let mut out = String::new();
    let near = |k: usize| (k.saturating_sub(2)..=(k + 2).min(rows.len().saturating_sub(1))).any(|x| rows[x].0 != ' ');
    let mut elided = false;
    for (k, (t, l)) in rows.iter().enumerate() {
        if *t == ' ' && !near(k) {
            elided = true;
            continue;
        }
        if elided {
            out.push_str("  ...\n");
            elided = false;
        }
        out.push(*t);
        out.push(' ');
        out.push_str(l);
        if !l.ends_with('\n') {
            out.push_str("\n\\ no newline at end of file\n");
        }
    }
    if elided {
        out.push_str("  ...\n");
    }
    out
}

// ---- minimal JSON ----

#[derive(Clone, Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num,
    Str(String),
    Array,
    Object(BTreeMap<String, Json>),
}

fn parse_json(s: &str) -> Result<Json, String> {
    let mut p = P { b: s.as_bytes(), i: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("trailing data at byte {}", p.i));
    }
    Ok(v)
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> Result<(), String> {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at byte {}", c as char, self.i))
        }
    }
    fn lit(&mut self, w: &str, v: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(v)
        } else {
            Err(format!("bad literal at byte {}", self.i))
        }
    }
    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err("nested too deeply".into());
        }
        match self.b.get(self.i) {
            None => Err("unexpected end".into()),
            Some(b'{') => {
                self.i += 1;
                let mut m = BTreeMap::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Object(m));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    self.eat(b':')?;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    m.insert(k, v);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Object(m));
                        }
                        _ => return Err(format!("expected ',' or '}}' at byte {}", self.i)),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Array);
                }
                loop {
                    self.ws();
                    self.value(depth + 1)?;
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Array);
                        }
                        _ => return Err(format!("expected ',' or ']' at byte {}", self.i)),
                    }
                }
            }
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.lit("true", Json::Bool(true)),
            Some(b'f') => self.lit("false", Json::Bool(false)),
            Some(b'n') => self.lit("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => {
                let st = self.i;
                while matches!(self.b.get(self.i), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[st..self.i])
                    .ok()
                    .and_then(|t| t.parse::<f64>().ok())
                    .map(|_| Json::Num)
                    .ok_or_else(|| format!("bad number at byte {st}"))
            }
            Some(c) => Err(format!("unexpected '{}' at byte {}", *c as char, self.i)),
        }
    }
    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.b.get(self.i..self.i + 4).ok_or("short \\u escape")?;
        let s = std::str::from_utf8(h).map_err(|_| "bad \\u escape")?;
        if !s.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err("bad \\u escape".into());
        }
        self.i += 4;
        u32::from_str_radix(s, 16).map_err(|_| "bad \\u escape".into())
    }
    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or("unterminated string")?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|_| "invalid UTF-8".into()),
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or("unterminated escape")?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut u = self.hex4()?;
                            if (0xD800..0xDC00).contains(&u) {
                                if self.b.get(self.i..self.i + 2) != Some(b"\\u") {
                                    return Err("lone surrogate".into());
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err("bad surrogate pair".into());
                                }
                                u = 0x10000 + ((u - 0xD800) << 10) + (lo - 0xDC00);
                            }
                            char::from_u32(u).ok_or("lone surrogate")?
                        }
                        _ => return Err(format!("bad escape at byte {}", self.i)),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if c < 0x20 => return Err("raw control character in string".into()),
                c => out.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrips_through_the_parser() {
        let r = Request {
            path: "a\"b.rs",
            language: "rust",
            base: None,
            ours: "x\n\t\u{1}é\n",
            theirs: "y",
        };
        let j = encode_request(&r);
        let Json::Object(m) = parse_json(&j).unwrap() else { panic!() };
        assert_eq!(m["path"], Json::Str("a\"b.rs".into()));
        assert_eq!(m["base"], Json::Null);
        assert_eq!(m["ours"], Json::Str("x\n\t\u{1}é\n".into()));
        assert_eq!(m["version"], Json::Num);
    }

    #[test]
    fn accepts_valid_and_ignores_unknown_fields() {
        let p = parse_response(br#"{"resolution":"a\nb\n","explanation":"why","extra":[1,{"x":null}]}"#).unwrap();
        assert_eq!(p.resolution, "a\nb\n");
        assert_eq!(p.explanation, "why");
        assert_eq!(parse_response(br#"{"resolution":""}"#).unwrap().resolution, "");
        assert_eq!(parse_response(br#"{"resolution":"\ud83d\ude00"}"#).unwrap().resolution, "\u{1F600}");
    }

    type Case<'a> = (&'a [u8], fn(&Reject) -> bool);

    #[test]
    fn rejects_garbage() {
        let cases: &[Case] = &[
            (b"", |r| matches!(r, Reject::NotJson(_))),
            (b"not json", |r| matches!(r, Reject::NotJson(_))),
            (b"{\"resolution\":\"x\"} trailing", |r| matches!(r, Reject::NotJson(_))),
            (b"{\"resolution\":\"x\"", |r| matches!(r, Reject::NotJson(_))),
            (b"[1,2]", |r| matches!(r, Reject::NotObject)),
            (b"\"x\"", |r| matches!(r, Reject::NotObject)),
            (b"{}", |r| matches!(r, Reject::MissingResolution)),
            (b"{\"resolution\":5}", |r| matches!(r, Reject::ResolutionNotString)),
            (b"{\"resolution\":null}", |r| matches!(r, Reject::ResolutionNotString)),
            (b"\xff\xfe", |r| matches!(r, Reject::NotUtf8)),
            (b"{\"resolution\":\"a\\u0000b\"}", |r| matches!(r, Reject::Nul)),
            (b"{\"resolution\":\"\\ud800\"}", |r| matches!(r, Reject::NotJson(_))),
            (b"{\"resolution\":\"a\nb\"}", |r| matches!(r, Reject::NotJson(_))),
            (
                b"{\"resolution\":\"ok\\n<<<<<<< ours\\nx\\n=======\\ny\\n>>>>>>> theirs\\n\"}",
                |r| matches!(r, Reject::ConflictMarkers),
            ),
        ];
        for (inp, ok) in cases {
            let e = parse_response(inp).unwrap_err();
            assert!(ok(&e), "{:?} -> {e:?}", String::from_utf8_lossy(inp));
        }
    }

    #[test]
    fn rejects_oversized_and_deep() {
        let big = vec![b' '; MAX_RESPONSE_BYTES + 1];
        assert!(matches!(parse_response(&big), Err(Reject::TooLarge(_))));
        let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
        assert!(matches!(parse_response(deep.as_bytes()), Err(Reject::NotJson(_))));
    }

    #[test]
    fn markers_need_line_start() {
        assert!(has_conflict_markers("a\n=======\nb"));
        assert!(has_conflict_markers("<<<<<<< ours\n"));
        assert!(!has_conflict_markers("let x = \"=======\";\n"));
        assert!(!has_conflict_markers("========= heading\n"));
    }

    #[test]
    fn truncates_long_explanations_on_a_char_boundary() {
        let e = "é".repeat(MAX_EXPLANATION_BYTES);
        let j = format!("{{\"resolution\":\"\",\"explanation\":\"{e}\"}}");
        assert!(parse_response(j.as_bytes()).unwrap().explanation.len() <= MAX_EXPLANATION_BYTES);
    }

    #[test]
    fn diff_marks_changes() {
        let d = diff_lines("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(d, "  a\n- b\n+ B\n  c\n");
        assert_eq!(diff_lines("x", "x\n"), "- x\n\\ no newline at end of file\n+ x\n");
    }
}
