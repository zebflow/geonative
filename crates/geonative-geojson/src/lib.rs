//! # geonative-geojson
//!
//! Pure-Rust reader and writer for **GeoJSON** (RFC 7946), part of the
//! [`geonative`](https://geonative.zebflow.com) geospatial library.
//!
//! ## What v0.1 covers
//!
//! Reader:
//! - Top-level `FeatureCollection`, bare `Feature`, or bare geometry object
//! - All seven RFC 7946 geometry types (Point/Multi*/Polygon/Multi*/Collection)
//! - Schema inference from `properties` (one pass; widening rules in
//!   [`properties`])
//! - CRS detection: default `EPSG:4326`, honours legacy 2008 `crs.properties.name`
//!   URNs like `urn:ogc:def:crs:EPSG::3857`
//! - String / numeric `id` → `Feature::fid` (i64 only — string ids that don't
//!   parse as integers fall back to row index)
//!
//! Writer:
//! - Streaming — O(one feature) memory regardless of input size
//! - Compact output (no whitespace); wrap your sink in a pretty-printer if
//!   you need indentation
//! - Always writes `FeatureCollection`; no `crs` member (RFC 7946 removed it)
//!
//! ## v0.1 scope cuts
//!
//! - **Z/M coordinates** silently truncated on read; never emitted on write.
//!   GeoJSON's third position is optional and underspecified across tools;
//!   v0.2 will add an opt-in `--preserve-z` switch.
//! - **Foreign members** (`bbox`, custom top-level keys) are read but not
//!   round-tripped on write — only spec-defined fields are emitted.
//! - **Streaming reader** — v0.1 loads everything into memory. A
//!   `serde_json::StreamDeserializer`-based streaming reader is on the
//!   roadmap for very large `.geojson.gz` feeds.
//!
//! ## Usage
//!
//! ```no_run
//! let r = geonative_geojson::GeoJsonReader::open("things.geojson")?;
//! println!("{} features, schema: {:?}", r.feature_count(), r.schema());
//! for f in r.features() {
//!     // f.fid, f.geometry, f.attributes
//! }
//! # Ok::<(), geonative_geojson::GeoJsonError>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod error;
pub mod geometry;
pub mod properties;
pub mod reader;
pub mod writer;

pub use error::{GeoJsonError, Result};
pub use reader::GeoJsonReader;
pub use writer::{write_features_to_path, GeoJsonWriter};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
