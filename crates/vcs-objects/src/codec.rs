//! Small canonical binary codec.
//!
//! * integers: unsigned LEB128, **minimal form only** (non-minimal rejected on decode)
//! * byte strings / strings: varint length + bytes (strings must be UTF-8)
//! * decoding is strict: trailing bytes are an error, so one logical value has one encoding.

use crate::id::ObjectId;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("unexpected end of input")]
    Eof,
    #[error("invalid {0}")]
    Invalid(&'static str),
    #[error("trailing bytes after object")]
    Trailing,
    #[error("limit exceeded: {0}")]
    Limit(&'static str),
    #[error("unsupported format version {0}")]
    Version(u8),
    #[error("unknown object kind {0}")]
    Kind(u8),
}

#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }
    pub fn varint(&mut self, mut v: u64) -> &mut Self {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.buf.push(b);
                return self;
            }
            self.buf.push(b | 0x80);
        }
    }
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.varint(b.len() as u64);
        self.buf.extend_from_slice(b);
        self
    }
    pub fn str(&mut self, s: &str) -> &mut Self {
        self.bytes(s.as_bytes())
    }
    pub fn raw(&mut self, b: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(b);
        self
    }
    pub fn id(&mut self, id: &ObjectId) -> &mut Self {
        self.raw(id.as_bytes())
    }
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        let b = *self.buf.get(self.pos).ok_or(DecodeError::Eof)?;
        self.pos += 1;
        Ok(b)
    }
    pub fn varint(&mut self) -> Result<u64, DecodeError> {
        let mut v: u64 = 0;
        let mut shift = 0u32;
        loop {
            let b = self.u8()?;
            if shift == 63 && b > 1 {
                return Err(DecodeError::Invalid("varint overflow"));
            }
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                if b == 0 && shift > 0 {
                    return Err(DecodeError::Invalid("non-minimal varint"));
                }
                return Ok(v);
            }
            shift += 7;
            if shift > 63 {
                return Err(DecodeError::Invalid("varint overflow"));
            }
        }
    }
    pub fn raw(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.remaining() < n {
            return Err(DecodeError::Eof);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn bytes(&mut self, max: usize) -> Result<&'a [u8], DecodeError> {
        let n = self.varint()?;
        if n > max as u64 {
            return Err(DecodeError::Limit("byte string too long"));
        }
        self.raw(n as usize)
    }
    pub fn str(&mut self, max: usize) -> Result<String, DecodeError> {
        let b = self.bytes(max)?;
        std::str::from_utf8(b).map(str::to_owned).map_err(|_| DecodeError::Invalid("utf-8"))
    }
    pub fn id(&mut self) -> Result<ObjectId, DecodeError> {
        let mut a = [0u8; 32];
        a.copy_from_slice(self.raw(32)?);
        Ok(ObjectId::from_bytes(a))
    }
    /// Element count, bounded by remaining input so hostile counts cannot force big allocations.
    pub fn count(&mut self, min_elem_size: usize) -> Result<usize, DecodeError> {
        let n = self.varint()?;
        if n > (self.remaining() / min_elem_size.max(1)) as u64 {
            return Err(DecodeError::Limit("element count exceeds input"));
        }
        Ok(n as usize)
    }
    pub fn finish(self) -> Result<(), DecodeError> {
        if self.remaining() != 0 {
            Err(DecodeError::Trailing)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn varint_roundtrip(v in any::<u64>()) {
            let mut w = Writer::new(); w.varint(v);
            let buf = w.finish();
            let mut r = Reader::new(&buf);
            prop_assert_eq!(r.varint().unwrap(), v);
            r.finish().unwrap();
        }
        #[test]
        fn string_roundtrip(s in ".{0,100}") {
            let mut w = Writer::new(); w.str(&s);
            let buf = w.finish();
            let mut r = Reader::new(&buf);
            prop_assert_eq!(r.str(1000).unwrap(), s);
        }
        #[test]
        fn decode_never_panics(data in proptest::collection::vec(any::<u8>(), 0..64)) {
            let mut r = Reader::new(&data);
            let _ = r.varint(); let _ = r.str(16); let _ = r.count(4);
        }
    }

    #[test]
    fn rejects_non_minimal_varint() {
        assert!(Reader::new(&[0x80, 0x00]).varint().is_err());
        assert_eq!(Reader::new(&[0x00]).varint().unwrap(), 0);
    }
}
