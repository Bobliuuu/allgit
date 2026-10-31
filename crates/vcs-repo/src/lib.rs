//! Native local repository: workspace, staging, commits, zones, snapshots.
//! See docs/architecture.md.

pub mod diff;
pub mod error;
pub mod gc;
pub mod index;
pub mod merge;
pub mod meta;
pub mod ops;
pub mod rebase;
pub mod repo;
pub mod sync;
pub mod treeops;

pub use error::{Error, Result};
pub use gc::GcReport;
pub use merge::{ConflictRec, MergeOutcome, MergeState};
pub use meta::{DEFAULT_BRANCH, ROOT_ZONE};
pub use rebase::{RebaseOutcome, RebaseState};
pub use repo::{AddReport, ZoneStatus, Change, ChangeKind, CommitReport, FsckReport, Repo, Status};
pub use sync::{PublishReport, SyncAction, SyncReport};
