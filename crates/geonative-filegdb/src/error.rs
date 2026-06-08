//! Driver-specific error type and its conversion into `geonative_core::Error`.
//!
//! Internal code returns `GdbError` so error sites have a dialect that fits
//! the FileGDB format (Eof with byte position, VarintOverflow with the start
//! offset, etc.). The public API maps via `From<GdbError> for core::Error`
//! so consumers can use a single error enum across formats.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, GdbError>;

#[derive(Debug, Error)]
pub enum GdbError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("unexpected end of input at pos {pos} (needed {need} more bytes)")]
    Eof { pos: usize, need: usize },

    #[error("varint overflow at pos {pos}")]
    VarintOverflow { pos: usize },

    #[error("invalid UTF-16 at pos {pos}")]
    InvalidUtf16 { pos: usize },

    #[error("malformed input: {0}")]
    Malformed(String),

    #[error("unsupported FileGDB feature: {0}")]
    Unsupported(String),
}

impl GdbError {
    pub fn malformed(msg: impl Into<String>) -> Self {
        Self::Malformed(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }
}

impl From<GdbError> for geonative_core::Error {
    fn from(e: GdbError) -> Self {
        match e {
            GdbError::Io(io) => geonative_core::Error::Io(io),
            GdbError::Unsupported(s) => geonative_core::Error::unsupported(s),
            other => geonative_core::Error::malformed(other.to_string()),
        }
    }
}
