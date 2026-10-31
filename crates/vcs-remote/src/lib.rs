//! Remote object-store protocol: backend abstraction, manifest, publish (WAL-less CAS
//! protocol) and sparse fetch. See docs/architecture.md §Remote.
//!
//! The publish protocol is an explicit step machine ([`Publisher::step`]) so that the
//! model-checking test can interleave writers and inject crashes between any two steps.

pub mod backend;
pub mod fsbackend;
pub mod manifest;
pub mod membackend;
pub mod packfetch;
pub mod protocol;

pub use backend::{Backend, BackendError, Versioned};
pub use fsbackend::FsBackend;
pub use manifest::Manifest;
pub use membackend::{CasFault, MemBackend};
pub use protocol::{fetch_heads, fetch_object, load_manifest, Outcome, PublishError, Publisher, Step};
