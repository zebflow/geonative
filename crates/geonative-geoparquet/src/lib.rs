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
pub mod extent;
pub mod meta;
pub mod optimize;
pub mod reader;
pub mod schema;
pub mod writer;

#[cfg(feature = "s3")]
pub mod async_reader;
#[cfg(feature = "s3")]
pub mod async_writer;

pub use builder::RecordBatchBuilder;
pub use error::{GeoParquetError, Result};
pub use extent::dataset_extent_from_stats;
pub use meta::{build_geo_metadata_json, parse_geo_metadata, GeoMetadataInput, GeoMetadataParsed};
pub use optimize::{optimize, OptimizeOptions, OptimizeReport};
pub use reader::{FeatureReadIter, GeoParquetReader};
pub use schema::{
    arrow_type_for, gdb_days_to_unix_micros, map_schema, MappedSchema, SchemaMapOptions,
};
pub use writer::{write_features_to_path, GeoParquetWriter, WriterOptions};

#[cfg(feature = "s3")]
pub use async_reader::{AsyncFeatureStream, GeoParquetAsyncReader};
#[cfg(feature = "s3")]
pub use async_writer::GeoParquetAsyncWriter;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
