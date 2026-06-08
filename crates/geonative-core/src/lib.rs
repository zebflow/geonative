//! # geonative-core
//!
//! The data model + WKB codec + object-safe `Dataset`/`Layer` traits that
//! every other geonative crate builds on. Zero-dep (apart from `thiserror`)
//! and the optional `geo-types` feature for ecosystem interop.
//!
//! ## What's in it
//!
//! Pick one of these as your starting point depending on what you're doing:
//!
//! | Module | What it gives you |
//! | --- | --- |
//! | [`geometry`] | `Coord`, `Geometry` (Simple Features tree), `LineString`, `Polygon`, `GeometryType` |
//! | [`value`] | `Value` (attribute payload) + `ValueType` (schema discriminant) |
//! | [`schema`] | `Schema` + `FieldDef` + `GeomField` — table-of-contents for one layer |
//! | [`feature`] | `Feature` — fid + geometry + attributes |
//! | [`crs`] | `Crs` enum (Unknown / Epsg / Wkt / Projjson) + EPSG resolution |
//! | [`wkb`] | OGC SF WKB encoder + decoder + alloc-free bbox walker |
//! | [`dataset`] | Object-safe `Dataset` / `Layer` traits for format-polymorphic code |
//! | [`error`] | Crate-wide `Error` + `Result` |
//! | [`geo_types_interop`] *(feature-gated)* | Conversions to/from the `geo-types` crate |
//!
//! ## Why this crate exists
//!
//! Every format crate (filegdb, shapefile, geoparquet, mvt) reads/writes
//! its bytes into the same `Feature` / `Geometry` / `Schema` types defined
//! here. That sharing is what makes the workspace a coherent library
//! instead of seven independent format codecs.
//!
//! ## Stability
//!
//! See `STABILITY.md` at the repo root. The growable IR enums
//! (`Geometry`, `GeometryType`, `Value`, `ValueType`, `Crs`) are
//! `#[non_exhaustive]` — adding variants in a SemVer-minor release does
//! not count as a break.
//!
//! ## What's in here
//!
//! - [`Geometry`] — the geometry tree (Point/Multi*/Polygon/Collection)
//! - [`Coord`] — a single coordinate, with optional `z` and `m`
//! - [`Value`] / [`ValueType`] — attribute values and their type tags
//! - [`Feature`] — `fid` + optional geometry + indexed attribute vector
//! - [`Schema`] / [`FieldDef`] / [`GeomField`] — layer schema description
//! - [`Crs`] — coordinate reference system (EPSG / WKT / PROJJSON)
//! - [`Error`] — common error type
//!
//! ## Cargo features
//!
//! - `geo-types` *(off by default)* — `From`/`Into` conversions between this
//!   crate's [`Geometry`] and [`geo_types::Geometry`], plus `Coord`. Z/M is
//!   silently dropped because `geo-types` is 2D.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod crs;
pub mod dataset;
pub mod error;
pub mod feature;
pub mod geometry;
pub mod schema;
pub mod value;
pub mod wkb;

#[cfg(feature = "geo-types")]
pub mod geo_types_interop;

pub use crs::Crs;
pub use dataset::{Dataset, Layer, SingleLayerDataset};
pub use error::{Error, Result};
pub use feature::Feature;
pub use geometry::{Coord, Geometry, GeometryType, LineString, Polygon};
pub use schema::{FieldDef, GeomField, Schema};
pub use value::{Value, ValueType};
pub use wkb::bbox_from_bytes;

/// Crate version, for diagnostic use.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
