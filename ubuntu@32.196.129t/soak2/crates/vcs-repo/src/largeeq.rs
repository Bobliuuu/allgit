//! Byte equality of two file-content ids. The same bytes can have two ids (a v3 small-chunk manifest and a
//! large-file kind 9 manifest, see docs/object-model.md "Large files"); merge, diff and status use these
//! helpers so such content counts as equal. Only manifests are compared chunk by chunk, never reassembled.
use crate::error::Result;
use vcs_objects::{ChunkedBlob, Object, ObjectId};
use vcs_store::ObjectStore;

struct Cursor<'a> {
    m: &'a ChunkedBlob,
    i: usize,
    buf: Vec<u8>,
    pos: usize,
}

impl Cursor<'_> {
    fn at_boundary(&self) -> bool {
        self.pos == self.buf.len()
    }
    /// Next chunk id when sitting on a chunk boundary.
    fn next_id(&self) -> Option<ObjectId> {
        self.m.chunks.get(self.i).map(|c| c.id)
    }
    fn fill(&mut self, store: &dyn ObjectStore) -> Result<bool> {
        if !self.at_boundary() {
            return Ok(true);
        }
        let Some(c) = self.m.chunks.get(self.i) else {
            return Ok(false);
        };
        let Object::Blob(b) = store.get(&c.id)? else {
            return Ok(false);
        };
        self.buf = b;
        self.pos = 0;
        self.i += 1;
        Ok(true)
    }
}

/// True when `a` and `b` name byte-identical file content, whatever their layouts.
pub(crate) fn same_content(store: &dyn ObjectStore, a: &ObjectId, b: &ObjectId) -> Result<bool> {
    if a == b {
        return Ok(true);
    }
    let (Object::Chunked(x), Object::Chunked(y)) = (store.get(a)?, store.get(b)?) else {
        // a blob is below the chunking threshold and a manifest above it: different ids mean different bytes
        return Ok(false);
    };
    if x.total_len != y.total_len {
        return Ok(false);
    }
    let mut ca = Cursor {
        m: &x,
        i: 0,
        buf: Vec::new(),
        pos: 0,
    };
    let mut cb = Cursor {
        m: &y,
        i: 0,
        buf: Vec::new(),
        pos: 0,
    };
    loop {
        // identical chunk at the same place on both sides: skip it unread
        if ca.at_boundary() && cb.at_boundary() && ca.next_id().is_some() && ca.next_id() == cb.next_id() {
            ca.i += 1;
            cb.i += 1;
            continue;
        }
        let (fa, fb) = (ca.fill(store)?, cb.fill(store)?);
        if !fa || !fb {
            return Ok(!fa && !fb);
        }
        let n = (ca.buf.len() - ca.pos).min(cb.buf.len() - cb.pos);
        if ca.buf[ca.pos..ca.pos + n] != cb.buf[cb.pos..cb.pos + n] {
            return Ok(false);
        }
        ca.pos += n;
        cb.pos += n;
    }
}
