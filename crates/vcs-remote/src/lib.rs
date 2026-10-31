//! Remote object-store protocol: backend abstraction, manifest, publish (WAL-less CAS
//! protocol) and sparse fetch. See docs/architecture.md §Remote.
//!
//! The publish protocol is an explicit step machine ([`Publisher::step`]) so that the
//! model-checking test can interleave writers and inject crashes between any two steps.

pub mod backend;
pub mod fsbackend;
pub mod locks;
pub mod manifest;
pub mod membackend;
pub mod protocol;

pub use backend::{Backend, BackendError, Versioned};
pub use fsbackend::FsBackend;
pub use locks::LockFault;
pub use manifest::{Lock, Manifest};
pub use membackend::{CasFault, MemBackend};
pub use protocol::{changed_paths, fetch_heads, fetch_object, load_manifest, Outcome, PublishError, Publisher, Step};
