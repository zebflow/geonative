//! Crate-specific error type.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, PmtilesError>;

#[derive(Debug, Error)]
pub enum PmtilesError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// File doesn't start with the PMTiles magic ("PMTiles" + version).
    #[error("not a PMTiles file: bad header (got bytes {0:02x?})")]
    NotPmtiles([u8; 8]),

    /// Magic ok, but the version byte isn't one we support.
    #[error("unsupported PMTiles version: {0} (we support v3 only)")]
    UnsupportedVersion(u8),

    /// File ended sooner than the spec requires.
    #[error("truncated PMTiles: needed {needed} bytes at offset {offset}, file is {total} bytes")]
    Truncated {
        offset: u64,
        needed: u64,
        total: u64,
    },

    #[error("malformed PMTiles: {0}")]
    Malformed(String),

    #[error("unsupported PMTiles feature: {0}")]
    Unsupported(String),

    /// A tile coordinate `(z, x, y)` is outside the valid range for its
    /// zoom or wasn't found in the archive's directories.
    #[error("tile not found: ({z},{x},{y})")]
    TileNotFound { z: u8, x: u32, y: u32 },

    /// gzip / brotli / zstd decompression failed.
    #[error("decompression failed: {0}")]
    Decompress(String),

    /// Object-store backend error (only present with the `s3` feature).
    #[cfg(feature = "s3")]
    #[error("object_store error: {0}")]
    ObjectStore(#[from] object_store::Error),
}

impl PmtilesError {
    pub fn malformed(msg: impl Into<String>) -> Self {
        Self::Malformed(msg.into())
    }
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }
}
