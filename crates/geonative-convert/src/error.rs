//! Convert-layer error type. Wraps the per-format errors plus the
//! orchestration-specific failures (extension unknown, layer ambiguous, etc.).

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ConvertError>;

#[derive(Debug, Error)]
pub enum ConvertError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("unsupported extension '.{ext}' for {path} ({supported})")]
    UnsupportedFormat {
        ext: String,
        path: String,
        supported: &'static str,
    },

    #[error("could not detect format from path: {0} (extension required)")]
    UnknownFormat(String),

    #[error("layer ambiguous: input has {count} layers; specify with --layer NAME. Available: {available}")]
    LayerAmbiguous { count: usize, available: String },

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("filegdb: {0}")]
    FileGdb(#[from] geonative_filegdb::GdbError),

    #[error("shapefile: {0}")]
    Shapefile(#[from] geonative_shapefile::ShpError),

    #[error("geoparquet: {0}")]
    GeoParquet(#[from] geonative_geoparquet::GeoParquetError),

    #[error("geojson: {0}")]
    GeoJson(#[from] geonative_geojson::GeoJsonError),

    #[error("geotiff: {0}")]
    GeoTiff(#[from] geonative_geotiff::GtiffError),

    #[error("core: {0}")]
    Core(#[from] geonative_core::Error),

    #[error("decode feature {row}: {source}")]
    DecodeRow {
        row: u64,
        #[source]
        source: Box<ConvertError>,
    },

    #[error("write feature {row}: {source}")]
    WriteRow {
        row: u64,
        #[source]
        source: Box<ConvertError>,
    },
}

impl ConvertError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidArgument(msg.into())
    }
}
