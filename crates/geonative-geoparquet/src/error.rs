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

    /// Object-store I/O error (S3 / Azure / GCS / R2 / HTTP / local-fs)
    /// surfaced from the async readers and writers. Only present when the
    /// `s3` feature is enabled.
    #[cfg(feature = "s3")]
    #[error("object_store error: {0}")]
    ObjectStore(#[from] object_store::Error),

    /// Returned by [`crate::GeoParquetWriter::write`] when the
    /// `hilbert_sort` buffer would exceed
    /// [`crate::WriterOptions::hilbert_memory_budget_bytes`].
    ///
    /// The writer surfaces this as a normal `Result::Err` **before** the
    /// next feature is buffered, so the caller can clean up the partial
    /// output file and retry without Hilbert (or with a larger budget)
    /// without involving the OS OOM-killer.
    #[error(
        "hilbert_sort memory budget exceeded: {used_bytes} B buffered (limit {budget_bytes} B) \
         after {features_buffered} features — retry with hilbert_sort=false or raise \
         WriterOptions::hilbert_memory_budget_bytes"
    )]
    HilbertBudgetExceeded {
        budget_bytes: usize,
        used_bytes: usize,
        features_buffered: usize,
    },
}

impl GeoParquetError {
    pub fn schema(msg: impl Into<String>) -> Self {
        Self::Schema(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }
}
