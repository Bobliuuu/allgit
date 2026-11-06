//! Minimum-reader format marker (issue #567, docs/upgrade-rollback.md).
//!
//! A repository's remote can come to use encodings an older binary cannot read: the compact v5
//! manifest (written above [`crate::manifest::COMPACT_THRESHOLD`]), pack/index v3 (written by
//! compaction), sharded manifests (`migrate-sharded`, one way). Without a marker an old binary
//! finds out by hitting a decode error ("unsupported version 5", "bad index header") or, worse,
//! by writing around data it cannot parse.
//!
//! The marker is one small object under [`KEY`]: the *minimum reader level* the repository needs,
//! plus the version string of the binary that last raised it (for the error message). Rules:
//!
//! * Absent marker = level 1 (the baseline: manifest v1-v4, packs v1/v2). Repositories that never
//!   use a newer feature never get a marker and stay byte-identical to what old binaries wrote.
//! * Writers **raise** the level, with compare-and-swap, *before* the first write that needs it
//!   (before the manifest CAS that stores a v5 manifest, before uploading a v3 pack, before the
//!   sharded freeze). A crash in between leaves the marker higher than strictly needed, which is
//!   the safe direction.
//! * Readers and writers **check** the marker before they read or write the manifest, and the
//!   server checks at startup. A binary whose [`LEVEL`] is below the marker refuses with
//!   [`FormatError::TooNew`] and changes nothing.
//! * The level never goes down on its own. [`lower`] exists for the documented rollback and
//!   refuses while the manifest or any pack still needs the current level.
//!
//! A binary that predates this module does not know the marker and is not protected by it; the
//! first release containing it is the floor for rollbacks (docs/upgrade-rollback.md).

use crate::backend::{Backend, BackendError};
use vcs_objects::codec::{Reader, Writer};

/// Backend key of the marker.
pub const KEY: &str = "format";

/// Level 1: manifest v1-v4, pack/index v1/v2. What every repository starts at.
pub const LEVEL_BASELINE: u32 = 1;
/// Level 2: compact v5 manifest and/or v3 packs.
pub const LEVEL_COMPACT: u32 = 2;
/// Level 3: sharded manifests (`migrate-sharded`).
pub const LEVEL_SHARDED: u32 = 3;
/// The highest level this binary can read and write.
pub const LEVEL: u32 = LEVEL_SHARDED;

const MAGIC: &[u8; 4] = b"AGFM";
const VERSION: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error(
        "this repository requires vcs format level {needed} (raised by {writer}) but this binary only supports level {supported} \
         (vcs {me}). Nothing was read or changed. Upgrade vcs, or roll the repository back as described in docs/upgrade-rollback.md"
    )]
    TooNew {
        needed: u32,
        supported: u32,
        writer: String,
        me: &'static str,
    },
    #[error("malformed format marker `{KEY}`: {0}")]
    Malformed(String),
    #[error("cannot lower the format level to {want}: {why}")]
    InUse { want: u32, why: String },
    #[error("format marker did not settle after repeated compare-and-swap attempts")]
    Contended,
    #[error(transparent)]
    Backend(#[from] BackendError),
}

/// Decoded marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pub min_reader: u32,
    /// `vcs` version that last raised the level.
    pub writer: String,
}

impl Format {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.raw(MAGIC).u8(VERSION).varint(u64::from(self.min_reader)).str(&self.writer);
        w.finish()
    }

    /// Strict decode. An unknown marker version is itself "too new": a later binary may have
    /// extended the marker, and refusing is the only safe answer.
    pub fn decode(bytes: &[u8]) -> Result<Format, FormatError> {
        let bad = |e: vcs_objects::DecodeError| FormatError::Malformed(e.to_string());
        let mut r = Reader::new(bytes);
        if r.raw(4).map_err(bad)? != MAGIC {
            return Err(FormatError::Malformed("bad magic".into()));
        }
        let v = r.u8().map_err(bad)?;
        if v != VERSION {
            return Err(FormatError::TooNew {
                needed: u32::MAX,
                supported: LEVEL,
                writer: format!("a newer vcs (marker version {v})"),
                me: env!("CARGO_PKG_VERSION"),
            });
        }
        let min_reader = u32::try_from(r.varint().map_err(bad)?).map_err(|_| FormatError::Malformed("level out of range".into()))?;
        let writer = r.str(128).map_err(bad)?;
        r.finish().map_err(bad)?;
        if min_reader == 0 {
            return Err(FormatError::Malformed("level 0".into()));
        }
        Ok(Format { min_reader, writer })
    }
}

/// The marker, or `None` when the repository has none (level 1).
pub fn read(b: &dyn Backend) -> Result<Option<(Format, String)>, FormatError> {
    match b.get(KEY)? {
        None => Ok(None),
        Some(v) => Ok(Some((Format::decode(&v.data)?, v.tag))),
    }
}

/// The repository's required level (1 without a marker).
pub fn level(b: &dyn Backend) -> Result<u32, FormatError> {
    Ok(read(b)?.map_or(LEVEL_BASELINE, |(f, _)| f.min_reader))
}

/// Refuse when the repository needs a newer reader than this binary.
pub fn check(b: &dyn Backend) -> Result<(), FormatError> {
    check_as(b, LEVEL)
}

/// [`check`] for a reader that supports levels up to `supported` (tests simulate an old binary).
pub fn check_as(b: &dyn Backend, supported: u32) -> Result<(), FormatError> {
    match read(b)? {
        Some((f, _)) if f.min_reader > supported => Err(FormatError::TooNew {
            needed: f.min_reader,
            supported,
            writer: f.writer,
            me: env!("CARGO_PKG_VERSION"),
        }),
        _ => Ok(()),
    }
}

/// Make sure the marker is at least `want`, before the caller writes something that needs it.
/// Returns whether it wrote. Concurrent raisers are safe (compare-and-swap, never lowers).
pub fn raise(b: &dyn Backend, want: u32) -> Result<bool, FormatError> {
    raise_as(b, want, LEVEL)
}

/// [`raise`] for a writer that supports levels up to `supported`.
pub fn raise_as(b: &dyn Backend, want: u32, supported: u32) -> Result<bool, FormatError> {
    if want <= LEVEL_BASELINE {
        return Ok(false);
    }
    let new = Format {
        min_reader: want,
        writer: format!("vcs {}", env!("CARGO_PKG_VERSION")),
    }
    .encode();
    for _ in 0..16 {
        match read(b)? {
            None => {
                if b.put_if_absent(KEY, &new)? {
                    return Ok(true);
                }
            }
            Some((f, tag)) => {
                if f.min_reader > supported {
                    return Err(FormatError::TooNew {
                        needed: f.min_reader,
                        supported,
                        writer: f.writer,
                        me: env!("CARGO_PKG_VERSION"),
                    });
                }
                if f.min_reader >= want {
                    return Ok(false);
                }
                if b.put_if_match(KEY, &new, &tag)?.is_some() {
                    return Ok(true);
                }
            }
        }
    }
    Err(FormatError::Contended)
}

/// The level the stored data needs right now: the manifest layout and every pack index header.
/// Used to refuse an unsafe [`lower`].
pub fn needed_by_data(b: &dyn Backend) -> Result<(u32, String), FormatError> {
    if let Some(v) = b.get(crate::manifest::KEY)? {
        if v.data.len() >= 5 && &v.data[..4] == b"AGMF" && v.data[4] == 5 {
            return Ok((LEVEL_COMPACT, "the manifest uses the compact v5 layout".into()));
        }
        if v.data.len() >= 5 && &v.data[..4] == b"AGMF" && v.data[4] == 9 {
            return Ok((LEVEL_SHARDED, "the manifest key holds a sharded-manifest marker".into()));
        }
    }
    for k in b.list("packs/")? {
        if !k.ends_with(".idx") {
            continue;
        }
        if let Some(m) = b.get_range(&k, 0, 8)? {
            if m == vcs_store::pack::IDX_MAGIC_V3 || m == vcs_store::pack::IDX_MAGIC_V4 {
                return Ok((LEVEL_COMPACT, format!("{k} is a v3/v4 pack index")));
            }
        }
    }
    Ok((LEVEL_BASELINE, String::new()))
}

/// Lower the marker to `want` for a rollback (docs/upgrade-rollback.md). Refuses while the data
/// still needs a higher level, so it cannot turn a clean refusal back into a decode error.
pub fn lower(b: &dyn Backend, want: u32) -> Result<bool, FormatError> {
    let want = want.max(LEVEL_BASELINE);
    let Some((f, tag)) = read(b)? else { return Ok(false) };
    if f.min_reader <= want {
        return Ok(false);
    }
    let (needed, why) = needed_by_data(b)?;
    if needed > want {
        return Err(FormatError::InUse { want, why });
    }
    if want == LEVEL_BASELINE {
        // back to "no marker": the repository is again byte-compatible with unmarked binaries
        b.delete(KEY)?;
        return Ok(true);
    }
    let new = Format {
        min_reader: want,
        writer: format!("vcs {}", env!("CARGO_PKG_VERSION")),
    }
    .encode();
    Ok(b.put_if_match(KEY, &new, &tag)?.is_some())
}
