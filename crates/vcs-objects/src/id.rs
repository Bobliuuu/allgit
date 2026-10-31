use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdParseError {
    #[error("expected {expected} hex characters, got {got}")]
    Length { expected: usize, got: usize },
    #[error("invalid hex character")]
    Char,
}

fn to_hex(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(H[(x >> 4) as usize] as char);
        s.push(H[(x & 15) as usize] as char);
    }
    s
}

fn from_hex<const N: usize>(s: &str) -> Result<[u8; N], IdParseError> {
    if s.len() != N * 2 {
        return Err(IdParseError::Length {
            expected: N * 2,
            got: s.len(),
        });
    }
    let mut out = [0u8; N];
    let b = s.as_bytes();
    let nib = |c: u8| match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(IdParseError::Char),
    };
    for i in 0..N {
        out[i] = (nib(b[2 * i])? << 4) | nib(b[2 * i + 1])?;
    }
    Ok(out)
}

/// 256-bit BLAKE3 identity of an encoded object (kind byte + version byte + payload).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId([u8; 32]);

impl ObjectId {
    pub const LEN: usize = 32;
    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }
    pub fn from_hex(s: &str) -> Result<Self, IdParseError> {
        from_hex::<32>(s).map(Self)
    }
    /// First 12 hex chars, for display only (never for lookup without disambiguation).
    pub fn short(&self) -> String {
        to_hex(&self.0[..6])
    }
}
impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}
impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", self.short())
    }
}

/// 128-bit stable logical-change identity. Survives rewrites (see docs/object-model.md).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChangeId([u8; 16]);

impl ChangeId {
    pub fn from_bytes(b: [u8; 16]) -> Self {
        Self(b)
    }
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }
    pub fn from_hex(s: &str) -> Result<Self, IdParseError> {
        from_hex::<16>(s).map(Self)
    }
    pub fn short(&self) -> String {
        to_hex(&self.0[..6])
    }
}
impl fmt::Display for ChangeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}
impl fmt::Debug for ChangeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChangeId({})", self.short())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hex_roundtrip() {
        let id = ObjectId::from_bytes([0xab; 32]);
        assert_eq!(ObjectId::from_hex(&id.to_hex()).unwrap(), id);
        assert!(ObjectId::from_hex("zz").is_err());
        assert!(
            ObjectId::from_hex(&"A".repeat(64)).is_err(),
            "uppercase rejected: one canonical form"
        );
    }
}
