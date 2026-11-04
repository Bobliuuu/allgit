//! Provider that shells out to the authenticated Claude Code CLI (`claude -p --output-format json`)
//! instead of calling the HTTP API. Needs no API key (the CLI holds its own login). The CLI is run
//! headless with every tool disabled (`--tools ""`), no MCP servers, no slash commands, no session
//! files and no settings sources, in an empty scratch directory, so it can only return text and
//! cannot read or edit any file. The conflict prompt goes in on stdin, never on the command line.

use crate::api::ApiError;
use serde_json::Value;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What one model call cost, when the provider reports it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metrics {
    pub latency_ms: u64,
    pub cost_usd: Option<f64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

impl Metrics {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "latency_ms": self.latency_ms, "cost_usd": self.cost_usd,
            "input_tokens": self.input_tokens, "output_tokens": self.output_tokens,
            "cache_read_tokens": self.cache_read_tokens, "cache_creation_tokens": self.cache_creation_tokens,
        })
    }
    pub fn from_json(v: &Value) -> Metrics {
        let u = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        Metrics {
            latency_ms: u("latency_ms"),
            cost_usd: v.get("cost_usd").and_then(Value::as_f64),
            input_tokens: u("input_tokens"),
            output_tokens: u("output_tokens"),
            cache_read_tokens: u("cache_read_tokens"),
            cache_creation_tokens: u("cache_creation_tokens"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CliConfig {
    /// The executable (`VCS_ASSIST_CLAUDE_BIN`, default `claude`).
    pub bin: String,
    /// Model alias or id passed to `--model` (`VCS_ASSIST_MODEL`, default `haiku`).
    pub model: String,
    pub timeout: Duration,
}

impl CliConfig {
    pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> CliConfig {
        let ne = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        CliConfig {
            bin: ne("VCS_ASSIST_CLAUDE_BIN").unwrap_or_else(|| "claude".into()),
            model: ne("VCS_ASSIST_MODEL").unwrap_or_else(|| "haiku".into()),
            timeout: Duration::from_secs(ne("VCS_ASSIST_API_TIMEOUT_SECS").and_then(|v| v.trim().parse().ok()).unwrap_or(300)),
        }
    }
}

/// Parse the CLI's `--output-format json` result object into the model text and its metrics.
pub fn parse_cli_json(body: &str) -> Result<(String, Metrics), ApiError> {
    let bad = |s: &str| ApiError::BadResponse(s.to_owned());
    let v: Value = serde_json::from_str(body.trim()).map_err(|_| bad("CLI output is not JSON"))?;
    if v.get("is_error").and_then(Value::as_bool) == Some(true) {
        let r = v.get("result").and_then(Value::as_str).unwrap_or("error");
        return Err(ApiError::Failed(r.chars().take(200).collect()));
    }
    if let Some(r) = v.get("stop_reason").and_then(Value::as_str) {
        if r == "max_tokens" {
            return Err(bad("the answer was cut off (max_tokens)"));
        }
    }
    let text = v
        .get("result")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("CLI output has no result text"))?;
    let u = v.get("usage");
    let n = |k: &str| u.and_then(|u| u.get(k)).and_then(Value::as_u64).unwrap_or(0);
    let m = Metrics {
        latency_ms: v.get("duration_ms").and_then(Value::as_u64).unwrap_or(0),
        cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cache_read_tokens: n("cache_read_input_tokens"),
        cache_creation_tokens: n("cache_creation_input_tokens"),
    };
    Ok((text.to_owned(), m))
}

/// The provider. `complete` has the shape of `crate::Completer`; the metrics of the last call are kept
/// for `take_metrics` (the eval harness reads them after each case).
pub struct CliProvider {
    pub cfg: CliConfig,
    last: std::cell::RefCell<Option<Metrics>>,
    scratch: std::path::PathBuf,
}

impl CliProvider {
    pub fn new(cfg: CliConfig) -> Result<CliProvider, String> {
        let scratch = std::env::temp_dir().join(format!("vcs-assist-cli-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).map_err(|e| format!("cannot create scratch dir: {e}"))?;
        Ok(CliProvider {
            cfg,
            last: Default::default(),
            scratch,
        })
    }

    pub fn take_metrics(&self) -> Option<Metrics> {
        self.last.borrow_mut().take()
    }

    pub fn complete(&self, system: &str, user: &str, _max_tokens: u32) -> Result<String, ApiError> {
        *self.last.borrow_mut() = None;
        let start = Instant::now();
        let mut child = Command::new(&self.cfg.bin)
            .args(["-p", "--output-format", "json", "--model", &self.cfg.model])
            // No tools at all, no MCP, no skills, no session files, no user/project settings or CLAUDE.md.
            .args([
                "--tools",
                "",
                "--strict-mcp-config",
                "--disable-slash-commands",
                "--no-session-persistence",
            ])
            .args(["--setting-sources", ""])
            .args(["--system-prompt", system])
            .current_dir(&self.scratch)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ApiError::Failed(format!("cannot run {}: {e}", self.cfg.bin)))?;
        let mut stdin = child.stdin.take().unwrap();
        let input = user.to_owned();
        let w = std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
        let (mut so, mut se) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
        let ro = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = so.read_to_string(&mut s);
            s
        });
        let re = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = se.read_to_string(&mut s);
            s
        });
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if start.elapsed() > self.cfg.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ApiError::Timeout);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return Err(ApiError::Failed(e.to_string())),
            }
        };
        let _ = w.join();
        let out = ro.join().unwrap_or_default();
        let err = re.join().unwrap_or_default();
        let wall = start.elapsed().as_millis() as u64;
        if out.trim().is_empty() {
            let e: String = err.trim().chars().take(200).collect();
            return Err(ApiError::Failed(format!("claude exited with {status}: {e}")));
        }
        let (text, mut m) = parse_cli_json(&out)?;
        // Wall clock of the whole process (what a user waits for), not the API-side duration.
        m.latency_ms = wall;
        *self.last.borrow_mut() = Some(m);
        Ok(text)
    }
}

impl Drop for CliProvider {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}
