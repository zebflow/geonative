//! Per-crate error type. Wraps the underlying decoder errors plus the
//! sidecar / format failure modes.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ImageError>;

#[derive(Debug, Error)]
pub enum ImageError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JPEG decode: {0}")]
    Jpeg(String),

    #[error("PNG decode: {0}")]
    Png(String),

    #[error("missing world file: expected {expected} next to image {image}")]
    MissingWorldFile { image: String, expected: String },

    #[error("world file parse: {0}")]
    WorldFile(String),

    #[error("unsupported image feature: {0}")]
    Unsupported(String),

    #[error("malformed input: {0}")]
    Malformed(String),
}

impl ImageError {
    pub fn malformed(msg: impl Into<String>) -> Self {
        Self::Malformed(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    pub fn world_file(msg: impl Into<String>) -> Self {
        Self::WorldFile(msg.into())
    }
}

impl From<ImageError> for geonative_core::Error {
    fn from(e: ImageError) -> Self {
        match e {
            ImageError::Io(io) => geonative_core::Error::Io(io),
            ImageError::Unsupported(s) => geonative_core::Error::unsupported(s),
            other => geonative_core::Error::malformed(other.to_string()),
        }
    }
}
