//! GeoTIFF-specific error type. Wraps the underlying codec errors plus the
//! TIFF-format-specific failure modes.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, GtiffError>;

#[derive(Debug, Error)]
pub enum GtiffError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("not a TIFF file: bad header (got bytes {0:02x?})")]
    NotATiff([u8; 4]),

    #[error("unsupported TIFF magic: {0}")]
    UnsupportedMagic(u16),

    #[error("truncated file: needed {needed} bytes at offset {offset}, file is {total} bytes")]
    Truncated {
        offset: u64,
        needed: u64,
        total: u64,
    },

    #[error("malformed TIFF: {0}")]
    Malformed(String),

    #[error("unsupported TIFF feature: {0}")]
    Unsupported(String),

    #[error("LZW decode failed: {0}")]
    Lzw(String),

    #[error("DEFLATE decode failed: {0}")]
    Deflate(String),

    #[error("tile out of range: requested ({x}, {y}) at level {level}, grid is {grid_x}×{grid_y}")]
    TileOutOfRange {
        level: u8,
        x: u32,
        y: u32,
        grid_x: u32,
        grid_y: u32,
    },

    #[error("pyramid level out of range: requested {requested}, file has {available}")]
    LevelOutOfRange { requested: u8, available: u8 },
}

impl GtiffError {
    pub fn malformed(msg: impl Into<String>) -> Self {
        Self::Malformed(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }
}

impl From<GtiffError> for geonative_core::Error {
    fn from(e: GtiffError) -> Self {
        match e {
            GtiffError::Io(io) => geonative_core::Error::Io(io),
            GtiffError::Unsupported(s) => geonative_core::Error::unsupported(s),
            other => geonative_core::Error::malformed(other.to_string()),
        }
    }
}
