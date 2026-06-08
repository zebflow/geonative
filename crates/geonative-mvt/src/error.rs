//! Driver-specific error type for MVT encoding.
//!
//! Kept narrow — MVT encoding only fails for two reasons in practice:
//! either the input geometry is something the format can't represent
//! (`Unsupported` — e.g. `GeometryCollection`) or the caller-supplied
//! features don't match the schema they declared (`Schema`).

use thiserror::Error;

pub type Result<T> = std::result::Result<T, MvtError>;

#[derive(Debug, Error)]
pub enum MvtError {
    #[error("unsupported MVT input: {0}")]
    Unsupported(String),

    #[error("schema mismatch: {0}")]
    Schema(String),
}
