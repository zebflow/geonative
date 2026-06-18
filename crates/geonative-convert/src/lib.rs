//! # geonative-convert
//!
//! Format-polymorphic orchestration layer for
//! [`geonative`](https://geonative.zebflow.com). This is the crate downstream
//! services (Zebflow, GovEyes, your-pipeline-here) import to **do things to
//! datasets** without caring which file format they're in.
//!
//! ## What's in it
//!
//! - [`Source`] — opens any of `.gdb` / `.shp` / `.parquet` / `.geojson`
//!   and streams `Feature`s through a callback.
//! - [`Sink`] — writes `Feature`s to `.parquet` or `.geojson`, format
//!   chosen by output extension.
//! - [`convert`] — orchestrate `Source` → `Sink` (the basic copy).
//! - [`filter_bbox`] — convert + bbox-intersect filter middleware.
//! - [`inspect`] — schema / CRS / extent / field-types report (no writes).
//! - [`metadata`] — write a `.geonative.json` sidecar (= inspect + envelope).
//!
//! ## Same API the CLI uses
//!
//! Every `geonative` subcommand delegates to one of the functions above.
//! Importing this crate gives downstream Rust code the same surface the
//! binary exposes — no subprocesses, no JSON parsing.
//!
//! ```no_run
//! use geonative_convert::{convert, ConvertOptions};
//! use std::path::Path;
//!
//! let stats = convert(
//!     Path::new("vicmap.gdb"),
//!     Path::new("out.parquet"),
//!     ConvertOptions::default(),
//! )?;
//! println!("wrote {} features", stats.features);
//! # Ok::<(), geonative_convert::ConvertError>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod convert;
pub mod error;
pub mod filter;
pub mod inspect;
pub mod io;
pub mod metadata;
#[cfg(feature = "s3")]
pub mod object;
pub mod raster;

pub use convert::{convert, reproject, ConvertOptions, ConvertStats};
pub use error::{ConvertError, Result};
pub use filter::{bbox_intersects, filter_bbox, Bbox2, FilterStats};
pub use inspect::{
    inspect, CrsInspection, DatasetInspection, FieldInspection, GeometryInspection, LayerInspection,
};
pub use io::{Format, Modality, Sink, SinkOptions, Source};
pub use metadata::{build as metadata, default_sidecar_path, Sidecar};
#[cfg(feature = "s3")]
pub use object::{convert_async, AsyncConvertOptions, DataLocation};
pub use raster::{
    convert_raster, RasterConvertOptions, RasterConvertStats, RasterSink, RasterSinkOptions,
    RasterSource,
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
