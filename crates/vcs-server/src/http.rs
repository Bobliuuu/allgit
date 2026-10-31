//! Minimal HTTP/1.1 request parsing and response writing (one request per connection).

use std::io::{BufRead, Read, Write};

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn query_param(&self, name: &str) -> Option<String> {
        self.query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (k == name).then(|| pct_decode(v)).flatten()
        })
    }
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![],
            body: body.into(),
        }
    }
    pub fn with(mut self, k: &str, v: impl Into<String>) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
}

fn reason(s: u16) -> &'static str {
    match s {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        412 => "Precondition Failed",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        428 => "Precondition Required",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

pub fn write_response(w: &mut dyn Write, r: &Response, head_only: bool) -> std::io::Result<()> {
    write!(w, "HTTP/1.1 {} {}\r\n", r.status, reason(r.status))?;
    for (k, v) in &r.headers {
        write!(w, "{k}: {v}\r\n")?;
    }
    write!(w, "Content-Length: {}\r\nConnection: close\r\n\r\n", r.body.len())?;
    if !head_only {
        w.write_all(&r.body)?;
    }
    w.flush()
}

pub fn pct_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())?;
            out.push(h);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Percent-encode everything except unreserved characters (and `/` when `keep_slash`).
pub fn pct_encode(s: &str, keep_slash: bool) -> String {
    let mut o = String::new();
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || b"-._~".contains(&c) || (keep_slash && c == b'/') {
            o.push(c as char);
        } else {
            o.push_str(&format!("%{c:02X}"));
        }
    }
    o
}

#[derive(Debug)]
pub enum ParseError {
    Io(std::io::Error),
    Bad(&'static str),
    TooLarge,
    Unsupported(&'static str),
}

const MAX_HEADERS: usize = 100;
const MAX_LINE: u64 = 16 * 1024;

fn read_line(r: &mut dyn BufRead) -> Result<String, ParseError> {
    let mut buf = Vec::new();
    let n = r.take(MAX_LINE).read_until(b'\n', &mut buf).map_err(ParseError::Io)?;
    if n == 0 {
        return Err(ParseError::Bad("connection closed"));
    }
    if buf.last() != Some(&b'\n') {
        return Err(ParseError::Bad("line too long"));
    }
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    String::from_utf8(buf).map_err(|_| ParseError::Bad("non-UTF-8 header"))
}

pub fn read_request(r: &mut dyn BufRead, max_body: usize) -> Result<Request, ParseError> {
    let line = read_line(r)?;
    let mut it = line.split(' ');
    let (method, target, ver) = (it.next(), it.next(), it.next());
    let (Some(method), Some(target), Some(ver)) = (method, target, ver) else {
        return Err(ParseError::Bad("malformed request line"));
    };
    if !ver.starts_with("HTTP/1.") {
        return Err(ParseError::Bad("unsupported HTTP version"));
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut headers = vec![];
    loop {
        let l = read_line(r)?;
        if l.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err(ParseError::Bad("too many headers"));
        }
        let (k, v) = l.split_once(':').ok_or(ParseError::Bad("malformed header"))?;
        headers.push((k.trim().to_owned(), v.trim().to_owned()));
    }
    let mut req = Request {
        method: method.to_owned(),
        path: path.to_owned(),
        query: query.to_owned(),
        headers,
        body: vec![],
    };
    if req.header("transfer-encoding").is_some() {
        return Err(ParseError::Unsupported("chunked request bodies are not supported"));
    }
    if let Some(cl) = req.header("content-length") {
        let n: usize = cl.parse().map_err(|_| ParseError::Bad("bad Content-Length"))?;
        if n > max_body {
            return Err(ParseError::TooLarge);
        }
        let mut body = vec![0u8; n];
        r.read_exact(&mut body).map_err(ParseError::Io)?;
        req.body = body;
    }
    Ok(req)
}
