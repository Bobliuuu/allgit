//! Bearer-token authentication and roles. The table stores only BLAKE3 hashes of tokens.

use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    /// get, get_range, exists, list.
    Read,
    /// Read plus puts; manifest updates only in allowed zones.
    Write,
    /// Write in every zone plus breaking other principals' locks.
    Admin,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Read => "read",
            Role::Write => "write",
            Role::Admin => "admin",
        }
    }
    fn parse(s: &str) -> Option<Role> {
        match s {
            "read" => Some(Role::Read),
            "write" => Some(Role::Write),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub name: String,
    pub role: Role,
    /// Zones this principal may change on the manifest. `None` = every zone.
    pub zones: Option<BTreeSet<String>>,
}

impl Principal {
    pub fn may_write_zone(&self, zone: &str) -> bool {
        match self.role {
            Role::Read => false,
            Role::Admin => true,
            Role::Write => self.zones.as_ref().is_none_or(|z| z.contains(zone)),
        }
    }
}

pub fn hash_token(token: &str) -> String {
    blake3::hash(token.as_bytes()).to_hex().to_string()
}

pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Clone, Debug)]
struct Entry {
    hash: String,
    principal: Principal,
    /// Unix milliseconds after which the token no longer authenticates. `None` = never.
    expires_at_ms: Option<u64>,
}

/// Why a presented token did not authenticate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    Unknown,
    Expired,
}

#[derive(Clone, Debug, Default)]
pub struct TokenTable {
    entries: Vec<Entry>,
}

impl TokenTable {
    /// One entry per line: `<principal> <read|write|admin> <zones|*> <blake3-hex-of-token> [expires=<unix-seconds>]`.
    /// `zones` is a comma-separated list or `*`. `#` starts a comment. Produce the hash with
    /// `vcs-server hash-token <token>`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut entries = vec![];
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            let err = |m: &str| format!("tokens line {}: {m}", n + 1);
            if f.len() != 4 && f.len() != 5 {
                return Err(err("expected `<principal> <role> <zones|*> <token-hash> [expires=<unix-seconds>]`"));
            }
            let role = Role::parse(f[1]).ok_or_else(|| err("role must be read, write or admin"))?;
            let zones = if f[2] == "*" {
                None
            } else {
                Some(f[2].split(',').filter(|s| !s.is_empty()).map(str::to_owned).collect())
            };
            if f[3].len() != 64 || !f[3].chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(err("token hash must be 64 hex characters"));
            }
            let expires_at_ms = match f.get(4) {
                None => None,
                Some(e) => {
                    let secs = e
                        .strip_prefix("expires=")
                        .and_then(|v| v.parse::<u64>().ok())
                        .ok_or_else(|| err("fifth field must be `expires=<unix-seconds>`"))?;
                    Some(secs.saturating_mul(1000))
                }
            };
            entries.push(Entry {
                hash: f[3].to_ascii_lowercase(),
                principal: Principal {
                    name: f[0].to_owned(),
                    role,
                    zones,
                },
                expires_at_ms,
            });
        }
        Ok(Self { entries })
    }

    pub fn from_entries(entries: Vec<(&str, Principal)>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(t, principal)| Entry {
                    hash: hash_token(t),
                    principal,
                    expires_at_ms: None,
                })
                .collect(),
        }
    }

    /// Like [`from_entries`](Self::from_entries) with an optional expiry in unix seconds per token.
    pub fn from_entries_expiring(entries: Vec<(&str, Principal, Option<u64>)>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(t, principal, exp)| Entry {
                    hash: hash_token(t),
                    principal,
                    expires_at_ms: exp.map(|s| s.saturating_mul(1000)),
                })
                .collect(),
        }
    }

    /// Authenticate against the system clock.
    pub fn authenticate(&self, token: &str) -> Option<&Principal> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.authenticate_at(token, now).ok()
    }

    /// Authenticate at `now_ms` (unix ms). The token comparison is constant-time over all entries.
    pub fn authenticate_at(&self, token: &str, now_ms: u64) -> Result<&Principal, AuthError> {
        let h = hash_token(token);
        let mut found = None;
        for e in &self.entries {
            if ct_eq(e.hash.as_bytes(), h.as_bytes()) {
                found = Some(e);
            }
        }
        match found {
            None => Err(AuthError::Unknown),
            Some(e) if e.expires_at_ms.is_some_and(|x| now_ms >= x) => Err(AuthError::Expired),
            Some(e) => Ok(&e.principal),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
