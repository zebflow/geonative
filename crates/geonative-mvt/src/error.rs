//! Errors that can surface during MVT encoding.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, MvtError>;

#[derive(Debug, Error)]
pub enum MvtError {
    #[error("unsupported MVT input: {0}")]
    Unsupported(String),

    #[error("schema mismatch: {0}")]
    Schema(String),
}
