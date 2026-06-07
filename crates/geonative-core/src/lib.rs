//! # geonative-core
//!
//! Core data model and driver traits for [`geonative`](https://geonative.zebflow.com),
//! a lightweight pure-Rust geospatial library built from scratch.
//!
//! This crate will define the common `Feature`, `Geometry`, and `Schema` types
//! along with the `Dataset` / `Layer` / `LayerWriter` traits that every format
//! driver (`geonative-filegdb`, `geonative-shapefile`, `geonative-geojson`,
//! `geonative-geoparquet`, …) implements.
//!
//! **This is a placeholder release** to reserve the crate name. The real API
//! is in active development at <https://github.com/zebflow/geonative>.

#![forbid(unsafe_code)]

/// Crate version, for diagnostic use.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
