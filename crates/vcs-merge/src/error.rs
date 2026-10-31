use vcs_objects::ObjectId;
use vcs_store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("object {0} not found")]
    Missing(ObjectId),
    #[error("corrupt object graph: {0}")]
    Corrupt(String),
}
