//! Reader for the output of `p4 -G`: a stream of Python-marshalled dictionaries.
//!
//! Format: the Python `marshal` encoding (version 0-2 subset) as documented in CPython's
//! `Python/marshal.c` and Perforce's "P4 -G" description in the Command-Line Reference
//! (global option `-G`: "Format output as marshalled Python dictionaries"). `p4 -G` writes one
//! dictionary per record, back to back, so a file may hold many values. The type codes handled:
//! `0` NULL (dict terminator), `N` None, `T`/`F` booleans, `i` int32 LE, `I` int64 LE,
//! `g` binary float64, `s`/`t`/`u`/`a` length-prefixed (u32 LE) strings, `{` dict (pairs until
//! `0`), `[` list and `(` tuple (u32 LE count). References (flag bit 0x80, marshal v3+) are
//! refused. Perforce emits only `{`, `s` and `i` in practice.
//!
//! Input is untrusted: depth and sizes are bounded and every read is checked.

use crate::error::{msg, Result};

const MAX_DEPTH: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dict(Vec<(Value, Value)>),
}

impl Value {
    /// Dictionary lookup by string key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(d) => d
                .iter()
                .find(|(k, _)| matches!(k, Value::Bytes(b) if b == key.as_bytes()))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// String view; ints are rendered (p4 sends most numbers as strings, some as ints).
    pub fn text(&self) -> Option<String> {
        match self {
            Value::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
            Value::Int(i) => Some(i.to_string()),
            _ => None,
        }
    }

    pub fn get_text(&self, key: &str) -> Option<String> {
        self.get(key).and_then(Value::text)
    }
}

struct Rd<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        if self.b.len() - self.pos < n {
            return msg("marshal: truncated input");
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("4 bytes")))
    }
    fn value(&mut self, depth: usize) -> Result<Value> {
        if depth > MAX_DEPTH {
            return msg("marshal: nesting too deep");
        }
        let code = self.take(1)?[0];
        match code {
            b'N' => Ok(Value::Null),
            b'T' => Ok(Value::Bool(true)),
            b'F' => Ok(Value::Bool(false)),
            b'i' => Ok(Value::Int(i32::from_le_bytes(self.take(4)?.try_into().expect("4 bytes")) as i64)),
            b'I' => Ok(Value::Int(i64::from_le_bytes(self.take(8)?.try_into().expect("8 bytes")))),
            b'g' => Ok(Value::Float(f64::from_le_bytes(self.take(8)?.try_into().expect("8 bytes")))),
            b's' | b't' | b'u' | b'a' => {
                let n = self.u32()? as usize;
                Ok(Value::Bytes(self.take(n)?.to_vec()))
            }
            b'[' | b'(' => {
                let n = self.u32()? as usize;
                let mut v = Vec::new(); // never preallocate from untrusted counts
                for _ in 0..n {
                    v.push(self.value(depth + 1)?);
                }
                Ok(Value::List(v))
            }
            b'{' => {
                let mut d = Vec::new();
                loop {
                    if self.b.get(self.pos) == Some(&b'0') {
                        self.pos += 1;
                        return Ok(Value::Dict(d));
                    }
                    let k = self.value(depth + 1)?;
                    let v = self.value(depth + 1)?;
                    d.push((k, v));
                }
            }
            c => msg(format!("marshal: unsupported type code {:?}", c as char)),
        }
    }
}

/// Parse every top-level value in `bytes` (a `p4 -G` output file).
pub fn parse_all(bytes: &[u8]) -> Result<Vec<Value>> {
    let mut r = Rd { b: bytes, pos: 0 };
    let mut out = Vec::new();
    while r.pos < bytes.len() {
        out.push(r.value(0)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> Vec<u8> {
        let mut v = vec![b's'];
        v.extend((x.len() as u32).to_le_bytes());
        v.extend(x.as_bytes());
        v
    }

    #[test]
    fn dict_two_records() {
        let mut b = vec![b'{'];
        b.extend(s("code"));
        b.extend(s("stat"));
        b.extend(s("n"));
        b.push(b'i');
        b.extend(7i32.to_le_bytes());
        b.push(b'0');
        let one = b.clone();
        b.extend(one);
        let v = parse_all(&b).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].get_text("code").as_deref(), Some("stat"));
        assert_eq!(v[1].get_text("n").as_deref(), Some("7"));
    }

    #[test]
    fn rejects_garbage_and_truncation() {
        assert!(parse_all(b"{s\xff\xff\xff\xff").is_err());
        assert!(parse_all(b"{").is_err());
        assert!(parse_all(b"Z").is_err());
        assert!(parse_all(&[b'['; 100]).is_err());
        assert!(parse_all(b"\xf3").is_err()); // flag-ref bit set
    }
}
