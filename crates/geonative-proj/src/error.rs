//! Projection-engine error type.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ProjError>;

#[derive(Debug, Error)]
pub enum ProjError {
    #[error("unsupported EPSG code: {0} (v0.1 supports 4326, 3857, 7844, 32601–32760, 7846–7859)")]
    UnsupportedEpsg(u32),

    #[error("unsupported CRS form: {0}")]
    UnsupportedCrs(String),

    #[error("coordinate out of projection domain: ({x}, {y})")]
    OutOfDomain { x: f64, y: f64 },
}

impl From<ProjError> for geonative_core::Error {
    fn from(e: ProjError) -> Self {
        match e {
            ProjError::UnsupportedEpsg(_) | ProjError::UnsupportedCrs(_) => {
                geonative_core::Error::unsupported(e.to_string())
            }
            ProjError::OutOfDomain { .. } => geonative_core::Error::malformed(e.to_string()),
        }
    }
}
