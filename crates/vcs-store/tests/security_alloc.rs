//! Security regression (docs/security-review.md): the length a chunk manifest *claims* must not
//! size an allocation before the chunks are fetched and verified.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use vcs_objects::{ChunkRef, ChunkedBlob, Object, ObjectId};
use vcs_store::{MemoryStore, ObjectStore};

struct Peak;
static BIGGEST: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Peak {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        BIGGEST.fetch_max(l.size(), Ordering::Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        BIGGEST.fetch_max(n, Ordering::Relaxed);
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static A: Peak = Peak;

#[test]
fn chunk_manifest_total_len_does_not_preallocate() {
    // 60 000 chunk refs of 64 KiB each = a manifest that claims ~3.9 GB, in a 2 MB object whose
    // chunks do not exist. Reading it must fail without ever asking for gigabytes.
    let chunks: Vec<ChunkRef> = (0..60_000u32)
        .map(|i| {
            let mut b = [0u8; 32];
            b[..4].copy_from_slice(&i.to_le_bytes());
            ChunkRef {
                id: ObjectId::from_bytes(b),
                len: 64 << 10,
            }
        })
        .collect();
    let total_len = chunks.iter().map(|c| c.len as u64).sum();
    let store = MemoryStore::new();
    let id = store.put(&Object::Chunked(ChunkedBlob { total_len, chunks, large: false })).unwrap();
    BIGGEST.store(0, Ordering::Relaxed);
    assert!(store.get_content(&id).is_err(), "missing chunks must be an error");
    let peak = BIGGEST.load(Ordering::Relaxed);
    assert!(peak < 64 << 20, "get_content asked for a {peak} byte allocation up front");
}
