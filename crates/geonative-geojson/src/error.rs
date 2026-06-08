//! GeoJSON-specific error type.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, GeoJsonError>;

#[derive(Debug, Error)]
pub enum GeoJsonError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("malformed GeoJSON: {0}")]
    Malformed(String),

    #[error("unsupported GeoJSON feature: {0}")]
    Unsupported(String),
}

impl GeoJsonError {
    pub fn malformed(msg: impl Into<String>) -> Self {
        Self::Malformed(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }
}

impl From<GeoJsonError> for geonative_core::Error {
    fn from(e: GeoJsonError) -> Self {
        match e {
            GeoJsonError::Io(io) => geonative_core::Error::Io(io),
            GeoJsonError::Unsupported(s) => geonative_core::Error::unsupported(s),
            other => geonative_core::Error::malformed(other.to_string()),
        }
    }
}
