//! Native content-addressed object model: identifiers, canonical codec, object types.
//!
//! No I/O, no async, no storage dependencies. See `docs/object-model.md`.

pub mod codec;
pub mod id;
pub mod names;
pub mod object;

pub use codec::DecodeError;
pub use id::{ChangeId, IdParseError, ObjectId};
pub use object::{
    blob_id, id_of_encoded, id_of_parts, AnnotatedTag, Commit, EntryMode, Kind, LockOp, Object, OpView, Operation, RefUpdate, Signature,
    Snapshot, Transaction, Tree, TreeEntry, FORMAT_VERSION,
};
