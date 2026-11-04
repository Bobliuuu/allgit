//! OpenAI chat-completions client: blocking calls with retries and a hard per-run budget. The key is read from the
//! environment once, only put in the `authorization` header, and scrubbed from every error text.

use std::cell::Cell;
use std::io::Read;
use std::time::{Duration, Instant};
use vcs_assist_claude::api::ApiError;

pub const DEFAULT_URL: &str = "https://api.openai.com/v1/chat/completions";
pub const DEFAULT_MODEL: &str = "gpt-4.1-mini";
const MAX_BODY: u64 = 8 << 20;

#[derive(Clone)]
pub struct ApiConfig {
    pub key: String,
    pub model: String,
    pub url: String,
    pub total_timeout: Duration,
    pub max_retries: u32,
    pub backoff: Duration,
    /// Cap on completion tokens per call (the caller's own estimate is lowered to this).
    pub max_output_tokens: u32,
    /// Hard budget for the whole process: HTTP requests (retries included) and total tokens (prompt + completion).
    pub max_requests: u64,
    pub max_total_tokens: u64,
    /// Name of the token-limit field: `max_completion_tokens` (OpenAI) or `max_tokens` (older compatible servers).
    pub token_param: String,
}

impl std::fmt::Debug for ApiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ApiConfig {{ model: {:?}, url: {:?}, key: <redacted> }}", self.model, self.url)
    }
}

impl ApiConfig {
    /// `OPENAI_API_KEY` (required), `VCS_ASSIST_MODEL`, `VCS_ASSIST_API_URL` (base URL of the chat-completions
    /// endpoint), `VCS_ASSIST_API_TIMEOUT_SECS` (25), `VCS_ASSIST_MAX_RETRIES` (2), `VCS_ASSIST_BACKOFF_MS` (500),
    /// `VCS_ASSIST_MAX_OUTPUT_TOKENS` (8192), `VCS_ASSIST_MAX_REQUESTS` (10), `VCS_ASSIST_MAX_TOTAL_TOKENS` (100000),
    /// `VCS_ASSIST_TOKEN_PARAM` (`max_completion_tokens`).
    pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Result<ApiConfig, String> {
        let nonempty = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        let key = nonempty("OPENAI_API_KEY").ok_or("OPENAI_API_KEY is not set")?;
        let num = |k: &str, d: u64| -> Result<u64, String> {
            match nonempty(k) {
                None => Ok(d),
                Some(v) => v.trim().parse().map_err(|_| format!("{k} must be a non-negative integer")),
            }
        };
        let url = nonempty("VCS_ASSIST_API_URL").unwrap_or_else(|| DEFAULT_URL.to_owned());
        check_url(&url)?;
        let token_param = nonempty("VCS_ASSIST_TOKEN_PARAM").unwrap_or_else(|| "max_completion_tokens".into());
        if !matches!(token_param.as_str(), "max_completion_tokens" | "max_tokens") {
            return Err("VCS_ASSIST_TOKEN_PARAM must be max_completion_tokens or max_tokens".into());
        }
        Ok(ApiConfig {
            key,
            model: nonempty("VCS_ASSIST_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
            url,
            total_timeout: Duration::from_secs(num("VCS_ASSIST_API_TIMEOUT_SECS", 25)?.max(1)),
            max_retries: num("VCS_ASSIST_MAX_RETRIES", 2)?.min(8) as u32,
            backoff: Duration::from_millis(num("VCS_ASSIST_BACKOFF_MS", 500)?),
            max_output_tokens: num("VCS_ASSIST_MAX_OUTPUT_TOKENS", 8192)?.clamp(1, u32::MAX as u64) as u32,
            max_requests: num("VCS_ASSIST_MAX_REQUESTS", 10)?,
            max_total_tokens: num("VCS_ASSIST_MAX_TOTAL_TOKENS", 100_000)?,
            token_param,
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

fn scrub(s: &str, key: &str) -> String {
    let mut out = if key.is_empty() {
        s.to_owned()
    } else {
        s.replace(key, "[redacted]")
    };
    out.truncate(out.char_indices().nth(300).map_or(out.len(), |(i, _)| i));
    out
}

/// What was spent so far, from the API `usage` fields (requests are counted when sent).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

impl Usage {
    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// Client with the running budget. Not thread-safe by design (one process, one conflict at a time).
pub struct Client {
    pub cfg: ApiConfig,
    used: Cell<Usage>,
}

impl Client {
    pub fn new(cfg: ApiConfig) -> Client {
        Client {
            cfg,
            used: Cell::new(Usage::default()),
        }
    }
    pub fn usage(&self) -> Usage {
        self.used.get()
    }
    fn add(&self, f: impl FnOnce(&mut Usage)) {
        let mut u = self.used.get();
        f(&mut u);
        self.used.set(u);
    }
    fn over_budget(&self, next_prompt_estimate: u64) -> Option<String> {
        let u = self.used.get();
        if u.requests >= self.cfg.max_requests {
            return Some(format!("request budget exhausted ({} requests)", self.cfg.max_requests));
        }
        if u.total_tokens() + next_prompt_estimate > self.cfg.max_total_tokens {
            return Some(format!(
                "token budget would be exceeded ({} used, limit {})",
                u.total_tokens(),
                self.cfg.max_total_tokens
            ));
        }
        None
    }

    /// One logical call: retries 429, 5xx and connection errors with exponential backoff within `total_timeout`.
    /// Every HTTP attempt counts against the request budget; the budget is checked before sending.
    pub fn complete(&self, system: &str, user: &str, max_tokens: u32) -> Result<String, ApiError> {
        let cfg = &self.cfg;
        let cap = max_tokens.min(cfg.max_output_tokens);
        // Rough lower bound of the prompt size (4 bytes per token is optimistic, so this never blocks a fitting call).
        let estimate = (system.len() + user.len()) as u64 / 4;
        let body = request_body(cfg, system, user, cap);
        let start = Instant::now();
        let mut last = String::from("no attempt made");
        for attempt in 0..=cfg.max_retries {
            if let Some(why) = self.over_budget(estimate) {
                return Err(ApiError::Failed(format!("budget: {why}")));
            }
            let remaining = cfg.total_timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(ApiError::Timeout);
            }
            let agent = ureq::AgentBuilder::new().timeout(remaining).redirects(0).build();
            self.add(|u| u.requests += 1);
            let res = agent
                .post(&cfg.url)
                .set("authorization", &format!("Bearer {}", cfg.key))
                .set("content-type", "application/json")
                .send_string(&body);
            let mut wait = cfg.backoff.saturating_mul(1 << attempt.min(6));
            match res {
                Ok(resp) => {
                    let mut s = String::new();
                    return match resp.into_reader().take(MAX_BODY).read_to_string(&mut s) {
                        Ok(_) => {
                            let (text, usage) = parse_body(&s);
                            if let Some((p, c)) = usage {
                                self.add(|u| {
                                    u.prompt_tokens += p;
                                    u.completion_tokens += c;
                                });
                            }
                            text
                        }
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
}

/// Models that only accept the default temperature (the o-series and gpt-5 family).
fn fixed_temperature(model: &str) -> bool {
    let m = model.rsplit('/').next().unwrap_or(model);
    m.starts_with("gpt-5") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4")
}

pub fn request_body(cfg: &ApiConfig, system: &str, user: &str, max_tokens: u32) -> String {
    let mut v = serde_json::json!({
        "model": cfg.model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
    });
    v[cfg.token_param.as_str()] = max_tokens.into();
    if !fixed_temperature(&cfg.model) {
        v["temperature"] = 0.into();
    }
    v.to_string()
}

/// Pure: the model text out of a chat-completions body, and the (prompt, completion) token usage if reported.
pub fn parse_body(body: &str) -> (Result<String, ApiError>, Option<(u64, u64)>) {
    let bad = |s: &str| ApiError::BadResponse(s.to_owned());
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return (Err(bad("not JSON")), None);
    };
    let usage = v.get("usage").map(|u| {
        let g = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        (g("prompt_tokens"), g("completion_tokens"))
    });
    if v.get("error").is_some() {
        return (Err(bad("API returned an error object")), usage);
    }
    let Some(choice) = v.get("choices").and_then(|c| c.as_array()).and_then(|c| c.first()) else {
        return (Err(bad("no choices")), usage);
    };
    match choice.get("finish_reason").and_then(|s| s.as_str()) {
        Some("length") => return (Err(bad("the answer was cut off (max tokens)")), usage),
        Some("content_filter") => return (Err(bad("the answer was blocked by a content filter")), usage),
        _ => {}
    }
    let msg = choice.get("message");
    if msg.and_then(|m| m.get("refusal")).is_some_and(|r| !r.is_null()) {
        return (Err(bad("the model declined to answer")), usage);
    }
    match msg.and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
        Some(t) if !t.is_empty() => (Ok(t.to_owned()), usage),
        _ => (Err(bad("no text in the response")), usage),
    }
}
