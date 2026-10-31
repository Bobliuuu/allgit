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

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Clone, Debug, Default)]
pub struct TokenTable {
    entries: Vec<(String, Principal)>,
}

impl TokenTable {
    /// One entry per line: `<principal> <read|write|admin> <zones|*> <blake3-hex-of-token>`.
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
            if f.len() != 4 {
                return Err(err("expected `<principal> <role> <zones|*> <token-hash>`"));
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
            entries.push((
                f[3].to_ascii_lowercase(),
                Principal {
                    name: f[0].to_owned(),
                    role,
                    zones,
                },
            ));
        }
        Ok(Self { entries })
    }

    pub fn from_entries(entries: Vec<(&str, Principal)>) -> Self {
        Self {
            entries: entries.into_iter().map(|(t, p)| (hash_token(t), p)).collect(),
        }
    }

    pub fn authenticate(&self, token: &str) -> Option<&Principal> {
        let h = hash_token(token);
        let mut found = None;
        for (eh, p) in &self.entries {
            if ct_eq(eh.as_bytes(), h.as_bytes()) {
                found = Some(p);
            }
        }
        found
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
