use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Store(#[from] vcs_store::StoreError),
    #[error(transparent)]
    Name(#[from] vcs_objects::names::NameError),
    #[error("decode error: {0}")]
    Decode(#[from] vcs_objects::DecodeError),
    #[error("i/o error at {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("not inside a vcs repository (no .vcs directory found)")]
    NotARepo,
    #[error(
        "repository lock held: {0}\nAnother vcs process is running, or one crashed. If you are sure none is running, delete that file."
    )]
    Locked(PathBuf),
    #[error("repository state corrupt: {0}")]
    Corrupt(String),
    #[error("nothing to commit (no staged changes)")]
    NothingToCommit,
    #[error("{0}")]
    Msg(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io<T>(path: &std::path::Path, r: std::io::Result<T>) -> Result<T> {
    r.map_err(|source| Error::Io {
        path: path.to_owned(),
        source,
    })
}

pub(crate) fn msg<T>(s: impl Into<String>) -> Result<T> {
    Err(Error::Msg(s.into()))
}
