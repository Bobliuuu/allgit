//! Staging file content that never existed in the working tree as such: the primitive behind
//! `add -p` and `restore -p --staged` (hunk-wise staging).

use crate::error::{msg, Result};
use crate::index::IndexEntry;
use crate::repo::Repo;
use vcs_objects::EntryMode;
use vcs_store::ObjectStore;

impl Repo {
    /// Make the index hold `data` for the regular file `path`, leaving the working tree alone.
    /// An existing entry keeps its mode; a new one takes `mode` (default: a plain file). The entry's
    /// stat data is left unknown so that `status` re-hashes the working file against it.
    pub fn stage_content(&self, path: &str, data: &[u8], mode: Option<EntryMode>) -> Result<()> {
        let _l = self.lock()?;
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let owner = zones.owner(path);
        if !ws.subs.contains_key(owner) {
            return msg(format!(
                "{path:?} belongs to zone {owner:?}, which this workspace has not subscribed to (vcs zone add {owner})"
            ));
        }
        let mut ix = self.index()?;
        let mode = ix.entries.get(path).map(|e| e.mode).or(mode).unwrap_or(EntryMode::File);
        if matches!(mode, EntryMode::Symlink | EntryMode::Directory) {
            return msg(format!("cannot stage partial content of {path:?}: not a regular file"));
        }
        let id = self.store.put_blob(data)?;
        ix.entries.remove(path);
        ix.insert(
            path,
            IndexEntry {
                id,
                mode,
                size: data.len() as u64,
                mtime_ns: 0,
                ctime_ns: 0,
            },
        )?;
        self.store.sync()?;
        self.save_index(&mut ix)?;
        self.record_op(&format!("stage {path} (partial)"), None)?;
        Ok(())
    }
}
