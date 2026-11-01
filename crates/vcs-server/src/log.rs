//! Structured logging: JSON lines or text on a sink (stderr by default), with a level filter.
//! Callers pass only fields they chose; nothing here ever sees a request's `Authorization` header.

use std::io::Write;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
        }
    }
    pub fn parse(s: &str) -> Option<Level> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "error" => Level::Error,
            "warn" | "warning" => Level::Warn,
            "info" => Level::Info,
            "debug" | "trace" => Level::Debug,
            _ => return None,
        })
    }
    /// RUST_LOG-like: `info`, or `warn,vcs_server=debug` (bare level and `vcs_server=`/`vcs-server=` entries; the last wins).
    pub fn parse_env_style(s: &str) -> Option<Level> {
        let mut out = None;
        for part in s.split(',') {
            let lvl = match part.split_once('=') {
                Some((t, l)) if matches!(t.trim(), "vcs_server" | "vcs-server") => Level::parse(l),
                Some(_) => None,
                None => Level::parse(part),
            };
            if lvl.is_some() {
                out = lvl;
            }
        }
        out
    }
    /// Level from `VCS_SERVER_LOG`, else `RUST_LOG`.
    pub fn from_env() -> Option<Level> {
        ["VCS_SERVER_LOG", "RUST_LOG"]
            .iter()
            .find_map(|k| std::env::var(k).ok().and_then(|v| Level::parse_env_style(&v)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        match s {
            "text" => Some(Format::Text),
            "json" => Some(Format::Json),
            _ => None,
        }
    }
}

pub enum V<'a> {
    S(&'a str),
    N(u64),
    F(f64),
}

pub struct Logger {
    format: Format,
    level: Level,
    sink: Mutex<Box<dyn Write + Send>>,
}

impl Logger {
    pub fn new(format: Format, level: Level) -> Self {
        Self::with_sink(format, level, Box::new(std::io::stderr()))
    }
    pub fn with_sink(format: Format, level: Level, sink: Box<dyn Write + Send>) -> Self {
        Self {
            format,
            level,
            sink: Mutex::new(sink),
        }
    }
    pub fn enabled(&self, l: Level) -> bool {
        l <= self.level
    }
    pub fn error(&self, msg: &str, f: &[(&str, V)]) {
        self.log(Level::Error, msg, f)
    }
    pub fn warn(&self, msg: &str, f: &[(&str, V)]) {
        self.log(Level::Warn, msg, f)
    }
    pub fn info(&self, msg: &str, f: &[(&str, V)]) {
        self.log(Level::Info, msg, f)
    }
    pub fn debug(&self, msg: &str, f: &[(&str, V)]) {
        self.log(Level::Debug, msg, f)
    }

    pub fn log(&self, level: Level, msg: &str, fields: &[(&str, V)]) {
        if !self.enabled(level) {
            return;
        }
        let ts = rfc3339_now();
        let mut line = String::new();
        match self.format {
            Format::Json => {
                line.push_str(&format!("{{\"ts\":\"{ts}\",\"level\":\"{}\",\"msg\":{}", level.name(), jstr(msg)));
                for (k, v) in fields {
                    line.push(',');
                    line.push_str(&jstr(k));
                    line.push(':');
                    match v {
                        V::S(s) => line.push_str(&jstr(s)),
                        V::N(n) => line.push_str(&n.to_string()),
                        V::F(f) if f.is_finite() => line.push_str(&format!("{f:.3}")),
                        V::F(_) => line.push('0'),
                    }
                }
                line.push('}');
            }
            Format::Text => {
                line.push_str(&format!("{ts} {:<5} {}", level.name().to_ascii_uppercase(), one_line(msg)));
                for (k, v) in fields {
                    line.push(' ');
                    line.push_str(k);
                    line.push('=');
                    match v {
                        V::S(s) => {
                            let s = one_line(s);
                            if s.is_empty() || s.contains([' ', '"', '=']) {
                                line.push_str(&jstr(&s));
                            } else {
                                line.push_str(&s);
                            }
                        }
                        V::N(n) => line.push_str(&n.to_string()),
                        V::F(f) => line.push_str(&format!("{f:.3}")),
                    }
                }
            }
        }
        line.push('\n');
        let mut s = self.sink.lock().unwrap_or_else(|e| e.into_inner());
        let _ = s.write_all(line.as_bytes());
        let _ = s.flush();
    }
}

fn one_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// JSON string literal.
pub fn jstr(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
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
    o
}

fn rfc3339_now() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    rfc3339(d.as_secs(), d.subsec_millis())
}

/// UTC timestamp from unix seconds (civil-from-days, Howard Hinnant).
pub fn rfc3339(secs: u64, ms: u32) -> String {
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamps_and_levels() {
        assert_eq!(rfc3339(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(1_709_210_096, 7), "2024-02-29T12:34:56.007Z");
        assert_eq!(Level::parse_env_style("warn,vcs_server=debug,other=error"), Some(Level::Debug));
        assert_eq!(Level::parse_env_style("hyper=debug"), None);
        assert!(Level::Error < Level::Debug);
    }
    #[test]
    fn json_escaping() {
        assert_eq!(jstr("a\"b\n\u{1}"), "\"a\\\"b\\n\\u0001\"");
    }
}
