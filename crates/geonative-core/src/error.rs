//! Common error type. Drivers define their own dialect-specific errors and
//! convert into this at the public-API boundary.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("malformed input: {0}")]
    Malformed(String),

    #[error("unsupported feature: {0}")]
    Unsupported(String),

    #[error("schema mismatch: {0}")]
    Schema(String),

    #[error("layer not found: {0}")]
    LayerNotFound(String),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn malformed(msg: impl Into<String>) -> Self {
        Self::Malformed(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    pub fn schema(msg: impl Into<String>) -> Self {
        Self::Schema(msg.into())
    }
}
