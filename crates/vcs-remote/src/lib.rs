//! Remote object-store protocol: backend abstraction, manifest, publish (WAL-less CAS
//! protocol) and sparse fetch. See docs/architecture.md §Remote.
//!
//! The publish protocol is an explicit step machine ([`Publisher::step`]) so that the
//! model-checking test can interleave writers and inject crashes between any two steps.

pub mod backend;
pub mod compact;
pub mod fsbackend;
pub mod gc;
pub mod locks;
pub mod manifest;
pub mod membackend;
pub mod packfetch;
pub mod protocol;
pub mod shelf;
#[cfg(feature = "sharded")]
pub mod shard;
pub mod stats;

pub use backend::{Backend, BackendError, Conditional, Versioned};
pub use compact::{compact, CompactOptions, CompactReport};
pub use fsbackend::FsBackend;
pub use gc::{remote_gc, GcError, RemoteGcOptions, RemoteGcReport};
pub use locks::LockFault;
pub use manifest::{Lock, Manifest};
pub use membackend::{CasFault, MemBackend};
pub use packfetch::fetch_many;
pub use protocol::{
    changed_paths, fetch_heads, fetch_heads_lazy, fetch_heads_lazy_shared, fetch_heads_lazy_with, fetch_object, fetch_objects,
    fetch_objects_with, fetch_tags_lazy, fetch_tags_lazy_shared, fetch_tags_lazy_with, load_manifest, refresh_manifest, repair_remote,
    Outcome, PublishError, Publisher, Step, PUBLISH_VERIFY_BUDGET,
};
pub use stats::{CountingBackend, LoggingBackend, TransferStats};
