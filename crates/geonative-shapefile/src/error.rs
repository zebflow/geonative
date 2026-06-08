//! Driver-specific error type for shapefile reading.
//!
//! Notable variant: `MissingFile` — the shapefile triad requires all three
//! of `.shp` / `.shx` / `.dbf`. We surface a clear "missing X" error rather
//! than treating it as generic I/O so consumers can prompt the user.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ShpError>;

#[derive(Debug, Error)]
pub enum ShpError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("malformed input: {0}")]
    Malformed(String),

    #[error("unsupported feature: {0}")]
    Unsupported(String),

    #[error("missing required file: {0}")]
    MissingFile(String),

    #[error("schema mismatch: {0}")]
    Schema(String),
}

impl ShpError {
    pub fn malformed(msg: impl Into<String>) -> Self {
        Self::Malformed(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }
}

impl From<ShpError> for geonative_core::Error {
    fn from(e: ShpError) -> Self {
        match e {
            ShpError::Io(io) => geonative_core::Error::Io(io),
            ShpError::Unsupported(s) => geonative_core::Error::unsupported(s),
            other => geonative_core::Error::malformed(other.to_string()),
        }
    }
}
