//! Native local repository: workspace, staging, commits, zones, snapshots.
//! See docs/architecture.md.

pub mod assist;
pub mod cgraph;
pub mod diff;
mod dircache;
pub mod edit;
pub mod error;
pub mod export;
pub mod failpoint;
pub mod gc;
pub mod ignore;
pub mod import;
pub mod import_p4;
pub mod index;
pub mod manage;
pub mod merge;
pub mod meta;
pub mod ops;
pub mod p4_marshal;
pub mod pick;
pub mod plumbing;
pub mod portable;
pub mod rebase;
pub mod repo;
pub mod stash;
pub mod sync;
pub mod tags;
pub mod treeops;

pub use assist::{AssistResult, ProposalInfo};
pub use error::{Error, Result};
pub use export::{ExportExt, ExportOptions, ExportReport};
pub use gc::{GcOptions, GcReport, GcRun};
pub use import::{ImportOptions, ImportReport};
pub use import_p4::{P4Report, P4View};
pub use manage::SnapshotCheckoutReport;
pub use merge::{ConflictRec, MergeOutcome, MergeState, Resolution, ResolveCheck};
pub use meta::{DEFAULT_BRANCH, ROOT_ZONE};
pub use rebase::{RebaseOutcome, RebaseState};
pub use repo::{AddReport, Change, ChangeKind, CommitReport, FsckReport, Repo, Status, StatusOpts, WorkCounts, ZoneStatus};
pub use stash::{StashEntry, StashPopOutcome, StashZone};
pub use sync::{PublishReport, SyncAction, SyncReport};
pub use tags::{BlameLine, ShowReport, TagInfo};
