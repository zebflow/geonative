//! # geonative-convert
//!
//! `ogr2ogr`-style format conversion pipeline for
//! [`geonative`](https://geonative.zebflow.com). Pipes features from any
//! `geonative-core::Layer` source to any `LayerWriter` sink, with optional
//! attribute/spatial filtering and reprojection.
//!
//! **This is a placeholder release** to reserve the crate name. The real API
//! is in active development at <https://github.com/zebflow/geonative>.

#![forbid(unsafe_code)]

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
