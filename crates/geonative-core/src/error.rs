//! The crate-wide error type plus its `Result` alias.
//!
//! ## Design
//!
//! `geonative-core` defines one neutral error enum that every downstream
//! crate (`filegdb`, `geoparquet`, `shapefile`, `mvt`, …) can map into via
//! `From`. Drivers keep their own dialect-specific error types (`GdbError`,
//! `ShpError`, `MvtError`, …) for in-crate use; conversion to this
//! `Error` happens at the public-API boundary.
//!
//! ## Variants and when to use them
//!
//! - **`Io`** — wraps any `std::io::Error` unchanged (auto via `From`).
//! - **`Malformed`** — bytes parsed but didn't match the expected format
//!   (truncated header, bad magic, varint overflow, etc.).
//! - **`Unsupported`** — input is valid but uses a feature we deliberately
//!   don't handle (Z/M variants in v0.1, compressed FileGDB tables, etc.).
//!   Distinguishes "the file is broken" from "we haven't built this yet".
//! - **`Schema`** — runtime schema mismatch (wrong attribute arity, type
//!   mismatch on a non-null field, etc.).
//! - **`LayerNotFound`** — caller asked for a layer name that doesn't exist
//!   in this dataset.
//! - **`Other`** — escape hatch for caller-supplied messages. Try not to
//!   reach for this in new code.

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
