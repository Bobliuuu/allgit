//! Minimal HTTP/1.1 request parsing and response writing (persistent connections: HTTP/1.1 keep-alive unless the client says close).

use std::io::{BufRead, Read, Write};

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The client allows the connection to be reused (HTTP/1.1 without `Connection: close`).
    pub keep_alive: bool,
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
    /// Large payload sent after `body` without copying (a window into a memoized pack).
    pub shared: Option<vcs_remote::SharedBytes>,
    /// Large payload read from the backend in windows while it is sent (never fully in memory): reader and length.
    pub stream: Option<(std::sync::Mutex<Box<dyn std::io::Read + Send>>, u64)>,
}

impl Response {
    /// A 200-style response whose body is shared bytes (sent zero-copy).
    pub fn shared(status: u16, bytes: vcs_remote::SharedBytes) -> Self {
        Self {
            status,
            headers: vec![],
            body: Vec::new(),
            shared: Some(bytes),
            stream: None,
        }
    }
    /// Total payload length.
    pub fn body_len(&self) -> usize {
        self.body.len() + self.shared.as_ref().map_or(0, |s| s.len()) + self.stream.as_ref().map_or(0, |s| s.1 as usize)
    }
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![],
            body: body.into(),
            shared: None,
            stream: None,
        }
    }
    /// A response whose body of `len` bytes is read from `reader` in windows while it is sent.
    pub fn streamed(status: u16, reader: Box<dyn std::io::Read + Send>, len: u64) -> Self {
        let mut r = Self::new(status, Vec::new());
        r.stream = Some((std::sync::Mutex::new(reader), len));
        r
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
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        412 => "Precondition Failed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        422 => "Unprocessable Entity",
        428 => "Precondition Required",
        501 => "Not Implemented",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

/// Window size for streamed bodies.
pub const STREAM_WINDOW: usize = 256 * 1024;

pub fn write_response(w: &mut dyn Write, r: &Response, head_only: bool) -> std::io::Result<()> {
    write_response_ka(w, r, head_only, false)
}

/// Like [`write_response`]; `keep_alive` advertises that the connection stays open.
pub fn write_response_ka(w: &mut dyn Write, r: &Response, head_only: bool, keep_alive: bool) -> std::io::Result<()> {
    use std::fmt::Write as _;
    // Small responses: one buffer, one write (separate head and body writes would trip Nagle against
    // the client's delayed ACK; connections also set TCP_NODELAY). Large bodies are not copied into
    // the head buffer: head and body go out in one vectored write.
    let mut head = String::with_capacity(256);
    let _ = write!(head, "HTTP/1.1 {} {}\r\n", r.status, reason(r.status));
    for (k, v) in &r.headers {
        let _ = write!(head, "{k}: {v}\r\n");
    }
    let conn = if keep_alive { "keep-alive" } else { "close" };
    let _ = write!(head, "Content-Length: {}\r\nConnection: {conn}\r\n\r\n", r.body_len());
    let mut head = head.into_bytes();
    if head_only {
        w.write_all(&head)?;
        return w.flush();
    }
    if let Some((rd, len)) = &r.stream {
        w.write_all(&head)?;
        let mut rd = rd.lock().unwrap_or_else(|e| e.into_inner());
        let mut buf = vec![0u8; STREAM_WINDOW];
        let mut left = *len;
        while left > 0 {
            let want = buf.len().min(left as usize);
            let n = rd.read(&mut buf[..want])?;
            if n == 0 {
                // the object shrank under us: the declared length cannot be honoured, so drop the connection
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            w.write_all(&buf[..n])?;
            left -= n as u64;
        }
        return w.flush();
    }
    let big = r.shared.is_some() || r.body.len() > 16 * 1024;
    if !big {
        head.extend_from_slice(&r.body);
        w.write_all(&head)?;
        return w.flush();
    }
    let tail: &[u8] = match &r.shared {
        Some(s) => s,
        None => &r.body,
    };
    write_all_vectored(w, &head, tail)?;
    w.flush()
}

/// `write_all(a ++ b)` without concatenating, using `write_vectored` (one `writev` on a socket).
fn write_all_vectored(w: &mut dyn Write, mut a: &[u8], mut b: &[u8]) -> std::io::Result<()> {
    use std::io::IoSlice;
    while !a.is_empty() || !b.is_empty() {
        let n = if a.is_empty() {
            w.write(b)?
        } else {
            w.write_vectored(&[IoSlice::new(a), IoSlice::new(b)])?
        };
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        let from_a = n.min(a.len());
        a = &a[from_a..];
        b = &b[n - from_a..];
    }
    Ok(())
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
    /// The server-wide budget for bytes of request bodies in flight is exhausted (answer 503).
    Overloaded,
    HeadersTooLarge,
    Unsupported(&'static str),
}

/// Limits applied while reading a request.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Request line plus all header lines, in bytes.
    pub max_header_bytes: usize,
    pub max_headers: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_header_bytes: 32 * 1024,
            max_headers: 100,
        }
    }
}

/// Read one line within `budget` bytes; returns the line without terminator and the bytes consumed.
fn read_line(r: &mut dyn BufRead, budget: usize) -> Result<(String, usize), ParseError> {
    let mut buf = Vec::new();
    let n = r.take(budget as u64 + 1).read_until(b'\n', &mut buf).map_err(ParseError::Io)?;
    if n == 0 {
        return Err(ParseError::Bad("connection closed"));
    }
    if buf.last() != Some(&b'\n') {
        return Err(if n > budget {
            ParseError::HeadersTooLarge
        } else {
            ParseError::Bad("connection closed mid-line")
        });
    }
    if n > budget {
        return Err(ParseError::HeadersTooLarge);
    }
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    let line = String::from_utf8(buf).map_err(|_| ParseError::Bad("non-UTF-8 header"))?;
    if line.bytes().any(|b| b == 0) {
        return Err(ParseError::Bad("NUL byte in header"));
    }
    Ok((line, n))
}

/// Parse one request. `on_headers` runs once the header block is complete (used to switch from the
/// header deadline to the body deadline); `body_limit` gives the maximum body size for that request.
pub fn read_request(
    r: &mut dyn BufRead,
    limits: &Limits,
    body_limit: &dyn Fn(&Request) -> usize,
    on_headers: &mut dyn FnMut(),
    reserve: &mut dyn FnMut(usize) -> bool,
) -> Result<Request, ParseError> {
    let mut budget = limits.max_header_bytes;
    let (line, n) = read_line(r, budget)?;
    budget -= n;
    let mut it = line.split(' ');
    let (method, target, ver, extra) = (it.next(), it.next(), it.next(), it.next());
    let (Some(method), Some(target), Some(ver), None) = (method, target, ver, extra) else {
        return Err(ParseError::Bad("malformed request line"));
    };
    if !ver.starts_with("HTTP/1.") {
        return Err(ParseError::Bad("unsupported HTTP version"));
    }
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(ParseError::Bad("malformed method"));
    }
    if !target.starts_with('/') {
        return Err(ParseError::Bad("request target must be an absolute path"));
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut headers = vec![];
    loop {
        let (l, n) = read_line(r, budget)?;
        budget -= n;
        if l.is_empty() {
            break;
        }
        if headers.len() >= limits.max_headers {
            return Err(ParseError::HeadersTooLarge);
        }
        let (k, v) = l.split_once(':').ok_or(ParseError::Bad("malformed header"))?;
        if k.is_empty() || k.bytes().any(|b| b.is_ascii_whitespace() || b.is_ascii_control()) {
            return Err(ParseError::Bad("malformed header name"));
        }
        headers.push((k.to_owned(), v.trim().to_owned()));
    }
    on_headers();
    let mut req = Request {
        method: method.to_owned(),
        path: path.to_owned(),
        query: query.to_owned(),
        headers,
        body: vec![],
        keep_alive: ver == "HTTP/1.1",
    };
    if req
        .header("connection")
        .is_some_and(|c| c.split(',').any(|t| t.trim().eq_ignore_ascii_case("close")))
    {
        req.keep_alive = false;
    }
    if req.header("transfer-encoding").is_some() {
        return Err(ParseError::Unsupported("chunked request bodies are not supported"));
    }
    let cls: Vec<&str> = req
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .map(|(_, v)| v.as_str())
        .collect();
    if cls.len() > 1 {
        return Err(ParseError::Bad("multiple Content-Length headers"));
    }
    if let Some(cl) = cls.first() {
        if cl.is_empty() || !cl.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ParseError::Bad("bad Content-Length"));
        }
        let n: usize = cl.parse().map_err(|_| ParseError::TooLarge)?;
        if n > body_limit(&req) {
            return Err(ParseError::TooLarge);
        }
        if !reserve(n) {
            return Err(ParseError::Overloaded);
        }
        // Grow as data arrives (a client that announces a large body and stalls costs no memory), by
        // doubling up to exactly `n`: no over-allocation and no probe read past the end.
        let mut body: Vec<u8> = Vec::with_capacity(n.min(64 * 1024));
        let mut chunk = vec![0u8; 64 * 1024];
        while body.len() < n {
            if body.len() == body.capacity() {
                body.reserve_exact(body.capacity().max(64 * 1024).min(n - body.len()));
            }
            let want = chunk.len().min(n - body.len());
            match r.read(&mut chunk[..want]) {
                Ok(0) => return Err(ParseError::Io(std::io::ErrorKind::UnexpectedEof.into())),
                Ok(k) => body.extend_from_slice(&chunk[..k]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(ParseError::Io(e)),
            }
        }
        req.body = body;
    }
    Ok(req)
}
