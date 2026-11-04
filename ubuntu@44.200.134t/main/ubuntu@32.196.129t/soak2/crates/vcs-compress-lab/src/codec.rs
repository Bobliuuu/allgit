use std::io::Cursor;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Codec {
    None,
    Lz4,
    Zstd(i32),
    Brotli(u32),
    /// byte-plane shuffle with element width `w` (2 or 4) followed by zstd level `l`
    ShufZstd(usize, i32),
}
impl Codec {
    pub fn name(&self) -> String {
        match self {
            Codec::None => "none".into(),
            Codec::Lz4 => "lz4".into(),
            Codec::Zstd(l) => format!("zstd{l}"),
            Codec::Brotli(q) => format!("brotli{q}"),
            Codec::ShufZstd(w, l) => format!("shuf{w}+zstd{l}"),
        }
    }
    pub fn parse(s: &str) -> Codec {
        match s {
            "none" => Codec::None,
            "lz4" => Codec::Lz4,
            _ if s.starts_with("shuf") => {
                let (w, l) = s[4..].split_once("+zstd").unwrap();
                Codec::ShufZstd(w.parse().unwrap(), l.parse().unwrap())
            }
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
            Codec::Zstd(l) | Codec::ShufZstd(_, l) => (
                Some(zstd::bulk::Compressor::new(l).unwrap()),
                Some(zstd::bulk::Decompressor::new().unwrap()),
            ),
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
            Codec::ShufZstd(w, _) => self.zc.as_mut().unwrap().compress(&shuffle(d, w)).unwrap(),
            Codec::Brotli(q) => {
                let mut out = Vec::new();
                let p = brotli::enc::BrotliEncoderParams {
                    quality: q as i32,
                    lgwin: 22,
                    ..Default::default()
                };
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
            Codec::ShufZstd(w, _) => unshuffle(&self.zd.as_mut().unwrap().decompress(c, n.max(1)).unwrap(), w),
            Codec::Brotli(_) => {
                let mut out = Vec::with_capacity(n);
                brotli::BrotliDecompress(&mut Cursor::new(c), &mut out).unwrap();
                out
            }
        }
    }
}

/// Byte-plane transpose over whole elements of width w; the tail (len % w bytes) is appended unchanged.
pub fn shuffle(d: &[u8], w: usize) -> Vec<u8> {
    let n = d.len() / w;
    let mut o = vec![0u8; d.len()];
    for i in 0..n {
        for j in 0..w {
            o[j * n + i] = d[i * w + j];
        }
    }
    o[n * w..].copy_from_slice(&d[n * w..]);
    o
}
pub fn unshuffle(d: &[u8], w: usize) -> Vec<u8> {
    let n = d.len() / w;
    let mut o = vec![0u8; d.len()];
    for i in 0..n {
        for j in 0..w {
            o[i * w + j] = d[j * n + i];
        }
    }
    o[n * w..].copy_from_slice(&d[n * w..]);
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shuffle_roundtrip() {
        let d: Vec<u8> = (0..1003u32).map(|x| (x * 7) as u8).collect();
        for w in [2, 4] {
            assert_eq!(unshuffle(&shuffle(&d, w), w), d);
        }
        let mut e = Engine::new(Codec::ShufZstd(4, 3));
        let c = e.compress(&d);
        assert_eq!(e.decompress(&c, d.len()), d);
    }
}
