//! Anthropic Messages API client: one blocking call with retries. The API key is read from the
//! environment once, only ever put in the `x-api-key` header, and scrubbed from every error text.

use std::io::Read;
use std::time::{Duration, Instant};

pub const DEFAULT_URL: &str = "https://api.anthropic.com/v1/messages";
pub const DEFAULT_MODEL: &str = "claude-sonnet-5-5";
const MAX_BODY: u64 = 8 << 20;

#[derive(Clone)]
pub struct ApiConfig {
    pub key: String,
    pub model: String,
    pub url: String,
    /// Wall-clock budget for the whole call including retries.
    pub total_timeout: Duration,
    pub max_retries: u32,
    pub backoff: Duration,
}

// Deliberately no Debug that prints the key.
impl std::fmt::Debug for ApiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ApiConfig {{ model: {:?}, url: {:?}, key: <redacted> }}", self.model, self.url)
    }
}

impl ApiConfig {
    /// Read `ANTHROPIC_API_KEY`, `VCS_ASSIST_MODEL`, and the tuning variables
    /// `VCS_ASSIST_API_URL`, `VCS_ASSIST_API_TIMEOUT_SECS` (default 25), `VCS_ASSIST_MAX_RETRIES`
    /// (default 3) and `VCS_ASSIST_BACKOFF_MS` (default 500).
    pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Result<ApiConfig, String> {
        let nonempty = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        let key = nonempty("ANTHROPIC_API_KEY").ok_or("ANTHROPIC_API_KEY is not set")?;
        let num = |k: &str, d: u64| -> Result<u64, String> {
            match nonempty(k) {
                None => Ok(d),
                Some(v) => v.trim().parse().map_err(|_| format!("{k} must be a non-negative integer")),
            }
        };
        let url = nonempty("VCS_ASSIST_API_URL").unwrap_or_else(|| DEFAULT_URL.to_owned());
        check_url(&url)?;
        Ok(ApiConfig {
            key,
            model: nonempty("VCS_ASSIST_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
            url,
            total_timeout: Duration::from_secs(num("VCS_ASSIST_API_TIMEOUT_SECS", 25)?.max(1)),
            max_retries: num("VCS_ASSIST_MAX_RETRIES", 3)?.min(8) as u32,
            backoff: Duration::from_millis(num("VCS_ASSIST_BACKOFF_MS", 500)?),
        })
    }
}

/// The key must never travel in clear text to a remote host: https, or http to loopback only.
fn check_url(url: &str) -> Result<(), String> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let host = rest.split(['/', '?']).next().unwrap_or("");
        let host = if host.starts_with('[') {
            host.split(']').next().unwrap_or("").trim_start_matches('[')
        } else {
            host.rsplit_once(':').map_or(host, |h| h.0)
        };
        if matches!(host, "127.0.0.1" | "localhost" | "::1") {
            return Ok(());
        }
    }
    Err("VCS_ASSIST_API_URL must be https:// (or http:// to localhost)".into())
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApiError {
    /// Non-retryable (4xx other than 429) or retries exhausted; text is already scrubbed.
    Failed(String),
    Timeout,
    /// The response parsed but is unusable (malformed, truncated, refused).
    BadResponse(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Failed(s) => write!(f, "API call failed: {s}"),
            ApiError::Timeout => write!(f, "API call timed out"),
            ApiError::BadResponse(s) => write!(f, "bad API response: {s}"),
        }
    }
}

fn scrub(s: &str, key: &str) -> String {
    let mut out = if key.is_empty() {
        s.to_owned()
    } else {
        s.replace(key, "[redacted]")
    };
    out.truncate(out.char_indices().nth(300).map_or(out.len(), |(i, _)| i));
    out
}

/// Pure: the model text out of a Messages API response body.
pub fn parse_body(body: &str) -> Result<String, ApiError> {
    let bad = |s: &str| ApiError::BadResponse(s.to_owned());
    let v: serde_json::Value = serde_json::from_str(body).map_err(|_| bad("not JSON"))?;
    if v.get("type").and_then(|t| t.as_str()) == Some("error") {
        return Err(bad("API returned an error object"));
    }
    match v.get("stop_reason").and_then(|s| s.as_str()) {
        Some("max_tokens") => return Err(bad("the answer was cut off (max_tokens)")),
        Some("refusal") => return Err(bad("the model declined to answer")),
        _ => {}
    }
    let blocks = v.get("content").and_then(|c| c.as_array()).ok_or_else(|| bad("no content array"))?;
    let mut text = String::new();
    for b in blocks {
        if b.get("type").and_then(|t| t.as_str()) == Some("text") {
            match b.get("text").and_then(|t| t.as_str()) {
                Some(t) => text.push_str(t),
                None => return Err(bad("text block without text")),
            }
        }
    }
    if text.is_empty() {
        return Err(bad("no text in the response"));
    }
    Ok(text)
}

pub fn request_body(model: &str, system: &str, user: &str, max_tokens: u32) -> String {
    serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "temperature": 0,
        "system": system,
        "messages": [{"role": "user", "content": user}],
    })
    .to_string()
}

/// One logical call: retries 429, 5xx (including 529) and connection errors with exponential
/// backoff (honouring a small `retry-after`), all within `total_timeout`.
pub fn complete(cfg: &ApiConfig, system: &str, user: &str, max_tokens: u32) -> Result<String, ApiError> {
    let body = request_body(&cfg.model, system, user, max_tokens);
    let start = Instant::now();
    let mut last = String::from("no attempt made");
    for attempt in 0..=cfg.max_retries {
        let remaining = cfg.total_timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Err(ApiError::Timeout);
        }
        let agent = ureq::AgentBuilder::new().timeout(remaining).redirects(0).build();
        let res = agent
            .post(&cfg.url)
            .set("x-api-key", &cfg.key)
            .set("anthropic-version", "2023-06-01")
            .set("content-type", "application/json")
            .send_string(&body);
        let mut wait = cfg.backoff.saturating_mul(1 << attempt.min(6));
        match res {
            Ok(resp) => {
                let mut s = String::new();
                return match resp.into_reader().take(MAX_BODY).read_to_string(&mut s) {
                    Ok(_) => parse_body(&s),
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Err(ApiError::Timeout),
                    Err(e) => Err(ApiError::BadResponse(scrub(&format!("unreadable body: {e}"), &cfg.key))),
                };
            }
            Err(ureq::Error::Status(code, resp)) => {
                let retry_after = resp
                    .header("retry-after")
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .map(Duration::from_secs);
                let mut detail = String::new();
                let _ = resp.into_reader().take(4096).read_to_string(&mut detail);
                last = scrub(&format!("HTTP {code}: {detail}"), &cfg.key);
                if !(code == 429 || code >= 500) {
                    return Err(ApiError::Failed(last));
                }
                // retry-after is honoured only when short; a long one is not worth the wait.
                if let Some(ra) = retry_after {
                    wait = wait.max(ra.min(Duration::from_secs(10)));
                }
            }
            Err(ureq::Error::Transport(t)) => {
                last = scrub(&format!("transport error: {t}"), &cfg.key);
                if t.kind() == ureq::ErrorKind::Io && last.contains("timed out") {
                    return Err(ApiError::Timeout);
                }
            }
        }
        if attempt == cfg.max_retries {
            break;
        }
        let left = cfg.total_timeout.saturating_sub(start.elapsed());
        if wait >= left {
            return Err(ApiError::Timeout);
        }
        std::thread::sleep(wait);
    }
    Err(ApiError::Failed(format!(
        "giving up after {} attempt(s): {last}",
        cfg.max_retries + 1
    )))
}
