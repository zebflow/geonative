//! # geonative-core
//!
//! Core data model for [`geonative`](https://geonative.zebflow.com) — a
//! lightweight, pure-Rust geospatial library built from scratch.
//!
//! This crate defines the **interoperable intermediate representation** that
//! every format driver (`geonative-filegdb`, `geonative-shapefile`,
//! `geonative-geojson`, `geonative-geoparquet`, …) reads into or writes from.
//! The model is Simple-Features-shaped so WKB encoding, GeoJSON, Shapefile,
//! and GeoParquet writers all become near-trivial walks over the tree.
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
pub mod error;
pub mod feature;
pub mod geometry;
pub mod schema;
pub mod value;
pub mod wkb;

#[cfg(feature = "geo-types")]
pub mod geo_types_interop;

pub use crs::Crs;
pub use error::{Error, Result};
pub use feature::Feature;
pub use geometry::{Coord, Geometry, GeometryType, LineString, Polygon};
pub use schema::{FieldDef, GeomField, Schema};
pub use value::{Value, ValueType};

/// Crate version, for diagnostic use.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
