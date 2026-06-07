//! # geonative-geoparquet
//!
//! GeoParquet writer for the [`geonative`](https://geonative.zebflow.com)
//! geospatial library. Consumes a `geonative_core::Schema` + iterator of
//! `geonative_core::Feature` and produces a Parquet file with WKB-encoded
//! geometry and the GeoParquet 1.1 `geo` metadata key.
//!
//! Currently exposed:
//! - [`schema`] — map a core `Schema` to an Arrow `Schema`.
//! - [`error::GeoParquetError`] — error type.
//!
//! Coming in subsequent phases:
//! - Feature → `RecordBatch` builder
//! - File-level `geo` metadata
//! - End-to-end writer with ZSTD + bbox covering columns

#![forbid(unsafe_code)]

pub mod builder;
pub mod error;
pub mod hilbert;
pub mod meta;
pub mod schema;
pub mod writer;

pub use builder::RecordBatchBuilder;
pub use error::{GeoParquetError, Result};
pub use meta::{build_geo_metadata_json, GeoMetadataInput};
pub use schema::{
    arrow_type_for, gdb_days_to_unix_micros, map_schema, MappedSchema, SchemaMapOptions,
};
pub use writer::{write_features_to_path, GeoParquetWriter, WriterOptions};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
