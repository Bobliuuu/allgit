//! Mapping from a client certificate's subject common name to a principal (mutual TLS).
//!
//! The map file has one `<common-name> <principal>` pair per line (`#` comments). When it is
//! configured (`--tls-client-map`), a request is accepted only if the connection's client
//! certificate has a mapped common name AND the bearer token authenticates as that same principal.
//! The certificate therefore narrows who may use which token; it never grants anything by itself.

use std::collections::BTreeMap;

#[derive(Debug, Default, Clone)]
pub struct ClientCertMap {
    map: BTreeMap<String, String>,
}

impl ClientCertMap {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut map = BTreeMap::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() != 2 {
                return Err(format!("client map line {}: expected `<common-name> <principal>`", n + 1));
            }
            if map.insert(f[0].to_owned(), f[1].to_owned()).is_some() {
                return Err(format!("client map line {}: duplicate common name `{}`", n + 1, f[0]));
            }
        }
        Ok(Self { map })
    }

    pub fn principal_for(&self, common_name: &str) -> Option<&str> {
        self.map.get(common_name).map(String::as_str)
    }

    /// True if a client with certificate subject CN `cn` (None = no usable CN) may act as `principal`.
    pub fn allows(&self, cn: Option<&str>, principal: &str) -> bool {
        cn.and_then(|c| self.principal_for(c)) == Some(principal)
    }
}

/// Read one DER element at the start of `b`: `(tag, content, rest)`. Definite lengths only.
fn tlv(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, b) = b.split_first()?;
    if tag & 0x1f == 0x1f {
        return None; // multi-byte tags do not occur on the path we walk
    }
    let (&l0, b) = b.split_first()?;
    let (len, b) = if l0 < 0x80 {
        (l0 as usize, b)
    } else {
        let n = (l0 & 0x7f) as usize;
        if n == 0 || n > 4 || b.len() < n {
            return None;
        }
        let len = b[..n].iter().fold(0usize, |a, x| (a << 8) | *x as usize);
        (len, &b[n..])
    };
    if b.len() < len {
        return None;
    }
    Some((tag, &b[..len], &b[len..]))
}

/// The subject common name of a DER X.509 certificate: `None` when there is none, more than one,
/// or it is not a plain string type. Not a validator: the certificate was already verified by the
/// TLS handshake; this only reads a name out of it.
pub fn subject_common_name(der: &[u8]) -> Option<String> {
    let (t, cert, _) = tlv(der)?;
    if t != 0x30 {
        return None;
    }
    let (t, mut tbs, _) = tlv(cert)?;
    if t != 0x30 {
        return None;
    }
    // [0] version (optional), serial, signature alg, issuer, validity, subject
    let (mut tag, mut _c, mut rest) = tlv(tbs)?;
    if tag == 0xa0 {
        tbs = rest;
        (tag, _c, rest) = tlv(tbs)?;
    }
    if tag != 0x02 {
        return None; // serial
    }
    let mut cur = rest;
    for _ in 0..3 {
        // signature alg, issuer, validity
        let (_, _, r) = tlv(cur)?;
        cur = r;
    }
    let (t, subject, _) = tlv(cur)?;
    if t != 0x30 {
        return None;
    }
    let mut found: Option<String> = None;
    let mut rdns = subject;
    while !rdns.is_empty() {
        let (t, set, r) = tlv(rdns)?;
        rdns = r;
        if t != 0x31 {
            return None;
        }
        let mut atvs = set;
        while !atvs.is_empty() {
            let (t, atv, r) = tlv(atvs)?;
            atvs = r;
            if t != 0x30 {
                return None;
            }
            let (t, oid, val) = tlv(atv)?;
            if t != 0x06 || oid != [0x55, 0x04, 0x03] {
                continue;
            }
            let (vt, v, _) = tlv(val)?;
            if !matches!(vt, 0x0c | 0x13 | 0x16) {
                return None;
            }
            let s = std::str::from_utf8(v).ok()?.to_owned();
            if found.replace(s).is_some() {
                return None; // two CNs are ambiguous
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rules() {
        let m = ClientCertMap::parse("# c\nalice-laptop alice\nci ci-bot # x\n").unwrap();
        assert!(m.allows(Some("alice-laptop"), "alice"));
        assert!(!m.allows(Some("alice-laptop"), "ci-bot"));
        assert!(!m.allows(None, "alice"));
        assert!(!m.allows(Some("unknown"), "alice"));
        assert!(ClientCertMap::parse("a b c").is_err());
        assert!(ClientCertMap::parse("a x\na y").is_err());
    }

    #[test]
    fn garbage_der_is_none_not_a_panic() {
        for n in 0..64usize {
            let v: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(subject_common_name(&v), None);
        }
        assert_eq!(subject_common_name(&[0x30, 0xff, 0xff, 0xff, 0xff, 0xff]), None);
    }
}
