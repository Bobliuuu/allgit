use std::io::Cursor;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Codec {
    None,
    Lz4,
    Zstd(i32),
    Brotli(u32),
}
impl Codec {
    pub fn name(&self) -> String {
        match self {
            Codec::None => "none".into(),
            Codec::Lz4 => "lz4".into(),
            Codec::Zstd(l) => format!("zstd{l}"),
            Codec::Brotli(q) => format!("brotli{q}"),
        }
    }
    pub fn parse(s: &str) -> Codec {
        match s {
            "none" => Codec::None,
            "lz4" => Codec::Lz4,
            _ if s.starts_with("zstd") => Codec::Zstd(s[4..].parse().unwrap()),
            _ if s.starts_with("brotli") => Codec::Brotli(s[6..].parse().unwrap()),
            _ => panic!("bad codec {s}"),
        }
    }
}

pub struct Engine {
    pub c: Codec,
    zc: Option<zstd::bulk::Compressor<'static>>,
    zd: Option<zstd::bulk::Decompressor<'static>>,
}
impl Engine {
    pub fn new(c: Codec) -> Engine {
        let (zc, zd) = match c {
            Codec::Zstd(l) => (Some(zstd::bulk::Compressor::new(l).unwrap()), Some(zstd::bulk::Decompressor::new().unwrap())),
            _ => (None, None),
        };
        Engine { c, zc, zd }
    }
    pub fn zstd_dict(level: i32, dict: &[u8]) -> Engine {
        Engine {
            c: Codec::Zstd(level),
            zc: Some(zstd::bulk::Compressor::with_dictionary(level, dict).unwrap()),
            zd: Some(zstd::bulk::Decompressor::with_dictionary(dict).unwrap()),
        }
    }
    pub fn compress(&mut self, d: &[u8]) -> Vec<u8> {
        match self.c {
            Codec::None => d.to_vec(),
            Codec::Lz4 => lz4_flex::block::compress(d),
            Codec::Zstd(_) => self.zc.as_mut().unwrap().compress(d).unwrap(),
            Codec::Brotli(q) => {
                let mut out = Vec::new();
                let p = brotli::enc::BrotliEncoderParams { quality: q as i32, lgwin: 22, ..Default::default() };
                brotli::BrotliCompress(&mut Cursor::new(d), &mut out, &p).unwrap();
                out
            }
        }
    }
    pub fn decompress(&mut self, c: &[u8], n: usize) -> Vec<u8> {
        match self.c {
            Codec::None => c.to_vec(),
            Codec::Lz4 => lz4_flex::block::decompress(c, n).unwrap(),
            Codec::Zstd(_) => self.zd.as_mut().unwrap().decompress(c, n.max(1)).unwrap(),
            Codec::Brotli(_) => {
                let mut out = Vec::with_capacity(n);
                brotli::BrotliDecompress(&mut Cursor::new(c), &mut out).unwrap();
                out
            }
        }
    }
}
