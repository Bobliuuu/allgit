//! Native local repository: workspace, staging, commits, zones, snapshots.
//! See docs/architecture.md.

pub mod assist;
pub mod diff;
pub mod edit;
pub mod error;
pub mod export;
pub mod gc;
pub mod import;
pub mod index;
pub mod manage;
pub mod merge;
pub mod meta;
pub mod ops;
pub mod pick;
pub mod rebase;
pub mod repo;
pub mod sync;
pub mod treeops;

pub use assist::{AssistResult, ProposalInfo};
pub use error::{Error, Result};
pub use export::ExportReport;
pub use gc::GcReport;
pub use import::ImportReport;
pub use manage::SnapshotCheckoutReport;
pub use merge::{ConflictRec, MergeOutcome, MergeState, Resolution, ResolveCheck};
pub use meta::{DEFAULT_BRANCH, ROOT_ZONE};
pub use rebase::{RebaseOutcome, RebaseState};
pub use repo::{AddReport, Change, ChangeKind, CommitReport, FsckReport, Repo, Status, ZoneStatus};
pub use sync::{PublishReport, SyncAction, SyncReport};
