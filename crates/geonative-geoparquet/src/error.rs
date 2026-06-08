//! Driver-specific error type. Wraps Arrow/Parquet errors and converts
//! cleanly to/from `geonative_core::Error`.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, GeoParquetError>;

#[derive(Debug, Error)]
pub enum GeoParquetError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),

    #[error("geonative-core error: {0}")]
    Core(#[from] geonative_core::Error),

    #[error("schema: {0}")]
    Schema(String),

    #[error("unsupported: {0}")]
    Unsupported(String),
}

impl GeoParquetError {
    pub fn schema(msg: impl Into<String>) -> Self {
        Self::Schema(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }
}
