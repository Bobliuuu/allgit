//! Pure merge engine: merge-base, line-based diff3 text merge, three-way tree merge.
//!
//! No filesystem, network or async. Stores are only touched through read-only traits.
//! See `docs/merge.md`.

pub mod base;
pub mod cgraph;
pub mod diff;
pub mod error;
#[cfg(any(
    feature = "structural",
    feature = "structural-python",
    feature = "structural-typescript",
    feature = "structural-go"
))]
pub mod structural;
pub mod text;
pub mod tree;

pub use base::{is_ancestor, merge_base, CommitGraph, MemGraph, StoreGraph};
pub use cgraph::CommitGraphFile;
pub use diff::Algorithm;
pub use error::MergeError;
pub use text::{merge3, render_markers, Hunk, Line, MarkerLabels, TextMerge};
pub use tree::{merge_flat, merge_flat_with, merge_trees, BlobSource, Conflict, ConflictKind, Flat, MapBlobs, StoreBlobs, TreeMerge};
