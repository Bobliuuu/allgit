//! Credential provider chain (#566): static environment keys, then the container credentials endpoint
//! (`AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` / `_FULL_URI`, ECS and EKS pod identity), then EC2 instance
//! metadata (IMDSv2). Role credentials last 1 to 12 hours, so they are cached and refreshed before they expire.
//!
//! Not implemented: web identity (IRSA, needs an STS call) and the SSO cache. Use the container or instance
//! role, or a pod identity agent, for those environments.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vcs_remote::BackendError;

/// Refresh this long before the reported expiry.
pub const REFRESH_MARGIN: Duration = Duration::from_secs(300);
const FETCH_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct Creds {
    pub access_key: String,
    pub secret_key: String,
    pub token: Option<String>,
    /// `None` = does not expire.
    pub expires: Option<SystemTime>,
}

#[derive(Clone, Debug)]
enum Source {
    /// Full URL and optional `Authorization` value.
    Container { url: String, auth: Option<String> },
    /// IMDS base, e.g. `http://169.254.169.254`.
    Imds { base: String },
}

pub struct CredentialProvider {
    source: Source,
    cache: Mutex<Option<Creds>>,
    /// Test hook: how far before expiry a refresh happens.
    margin: Duration,
}

impl std::fmt::Debug for CredentialProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.source {
            Source::Container { url, .. } => write!(f, "CredentialProvider::Container({url})"),
            Source::Imds { base } => write!(f, "CredentialProvider::Imds({base})"),
        }
    }
}

impl CredentialProvider {
    /// Container credentials if the container env vars are set, else `None`.
    pub fn container_from_env(env: &dyn Fn(&str) -> Option<String>) -> Option<Self> {
        let get = |k: &str| env(k).filter(|v| !v.is_empty());
        let url = if let Some(rel) = get("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI") {
            format!("http://169.254.170.2{rel}")
        } else {
            get("AWS_CONTAINER_CREDENTIALS_FULL_URI")?
        };
        let auth = get("AWS_CONTAINER_AUTHORIZATION_TOKEN").or_else(|| {
            let f = get("AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE")?;
            std::fs::read_to_string(f).ok().map(|s| s.trim().to_owned())
        });
        Some(Self::container(&url, auth))
    }

    pub fn container(url: &str, auth: Option<String>) -> Self {
        Self::new(Source::Container { url: url.to_owned(), auth })
    }

    /// IMDSv2 against `base` (`http://169.254.169.254` unless `AWS_EC2_METADATA_SERVICE_ENDPOINT` says otherwise).
    pub fn imds(base: &str) -> Self {
        Self::new(Source::Imds {
            base: base.trim_end_matches('/').to_owned(),
        })
    }

    pub fn imds_from_env(env: &dyn Fn(&str) -> Option<String>) -> Option<Self> {
        if env("AWS_EC2_METADATA_DISABLED").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            return None;
        }
        let base = env("AWS_EC2_METADATA_SERVICE_ENDPOINT")
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "http://169.254.169.254".into());
        Some(Self::imds(&base))
    }

    fn new(source: Source) -> Self {
        Self {
            source,
            cache: Mutex::new(None),
            margin: REFRESH_MARGIN,
        }
    }

    /// Refresh when less than `m` of validity is left (tests).
    pub fn with_margin(mut self, m: Duration) -> Self {
        self.margin = m;
        self
    }

    /// Current credentials, fetching or refreshing when missing or within the margin of expiry. If a refresh
    /// fails but the cached credentials have not actually expired yet, they are returned and the next call retries.
    pub fn current(&self) -> Result<Creds, BackendError> {
        let mut g = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let now = SystemTime::now();
        if let Some(c) = g.as_ref() {
            if c.expires.is_none_or(|e| e > now + self.margin) {
                return Ok(c.clone());
            }
        }
        match self.fetch() {
            Ok(c) => {
                crate::STATS.cred_refreshes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                *g = Some(c.clone());
                Ok(c)
            }
            Err(e) => {
                crate::STATS.cred_refresh_errors.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                match g.as_ref() {
                    Some(c) if c.expires.is_none_or(|x| x > now) => Ok(c.clone()),
                    _ => Err(e),
                }
            }
        }
    }

    /// Forget the cached credentials (the service said they are expired).
    pub fn invalidate(&self) {
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn fetch(&self) -> Result<Creds, BackendError> {
        let agent = ureq::AgentBuilder::new().timeout(FETCH_TIMEOUT).redirects(0).build();
        let err = |what: &str, e: String| BackendError::Other(format!("credential provider {what}: {e}"));
        let body = match &self.source {
            Source::Container { url, auth } => {
                let mut r = agent.get(url);
                if let Some(a) = auth {
                    r = r.set("Authorization", a);
                }
                r.call()
                    .map_err(|e| err("container endpoint", e.to_string()))?
                    .into_string()
                    .map_err(|e| err("container endpoint", e.to_string()))?
            }
            Source::Imds { base } => {
                let token = agent
                    .put(&format!("{base}/latest/api/token"))
                    .set("X-aws-ec2-metadata-token-ttl-seconds", "21600")
                    .call()
                    .map_err(|e| err("IMDSv2 token", e.to_string()))?
                    .into_string()
                    .map_err(|e| err("IMDSv2 token", e.to_string()))?;
                let get = |path: &str| -> Result<String, BackendError> {
                    agent
                        .get(&format!("{base}{path}"))
                        .set("X-aws-ec2-metadata-token", token.trim())
                        .call()
                        .map_err(|e| err("IMDS", e.to_string()))?
                        .into_string()
                        .map_err(|e| err("IMDS", e.to_string()))
                };
                let roles = get("/latest/meta-data/iam/security-credentials/")?;
                let role = roles.lines().next().map(str::trim).filter(|r| !r.is_empty());
                let role = role.ok_or_else(|| err("IMDS", "no instance role attached".into()))?;
                get(&format!("/latest/meta-data/iam/security-credentials/{role}"))?
            }
        };
        parse_creds_json(&body).map_err(|m| err("response", m))
    }
}

/// Value of `"key": "value"` in a flat JSON object (the credential documents are flat).
fn json_str(body: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let after = &body[body.find(&pat)? + pat.len()..];
    let after = after.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let mut out = String::new();
    let mut it = after.chars();
    while let Some(c) = it.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(it.next()?),
            c => out.push(c),
        }
    }
    None
}

pub(crate) fn parse_creds_json(body: &str) -> Result<Creds, String> {
    if let Some(code) = json_str(body, "Code") {
        if code != "Success" {
            return Err(format!("provider answered Code={code}"));
        }
    }
    let ak = json_str(body, "AccessKeyId").ok_or("no AccessKeyId")?;
    let sk = json_str(body, "SecretAccessKey").ok_or("no SecretAccessKey")?;
    let token = json_str(body, "Token").filter(|t| !t.is_empty());
    let expires = json_str(body, "Expiration")
        .and_then(|e| crate::parse_iso_ms(&e))
        .map(|ms| UNIX_EPOCH + Duration::from_millis(ms));
    Ok(Creds {
        access_key: ak,
        secret_key: sk,
        token,
        expires,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_container_and_imds_documents() {
        let c = parse_creds_json(
            r#"{"AccessKeyId":"AK","SecretAccessKey":"S\/K","Token":"T","Expiration":"2030-01-01T00:00:00Z","RoleArn":"x"}"#,
        )
        .unwrap();
        assert_eq!(
            (c.access_key.as_str(), c.secret_key.as_str(), c.token.as_deref()),
            ("AK", "S/K", Some("T"))
        );
        assert!(c.expires.is_some());
        let c = parse_creds_json("{\n  \"Code\" : \"Success\",\n  \"AccessKeyId\" : \"A\",\n  \"SecretAccessKey\" : \"B\"\n}").unwrap();
        assert!(c.token.is_none() && c.expires.is_none());
        assert!(parse_creds_json(r#"{"Code":"AssumeRoleUnauthorizedAccess"}"#).is_err());
        assert!(parse_creds_json("{}").is_err());
    }
}
