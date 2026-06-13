//! # geonative-processing
//!
//! Higher-level geoprocessing operations on `Layer` / `Feature` streams. Where
//! [`geonative-utils`](https://docs.rs/geonative-utils) holds *pure
//! algorithms* (simplify, hilbert, measure — no I/O, no Layer awareness),
//! this crate holds operations that consume a stream of [`Feature`]s and
//! produce a report or a new stream:
//!
//! - [`profile`] — Schema/Layer → null counts + min/max + top-N + samples
//!   (think `pandas.describe()` but format-agnostic)
//! - **`reproject`** (Phase 12c, coming next) — Coord-by-coord transform via
//!   `geonative-proj-pure`
//! - **`clip` / `spatial-join`** — Future. Need R-tree + exact predicates.
//!
//! Format crates (`geonative-filegdb`, `geonative-shapefile`, …) produce
//! `Feature` streams; format writers consume them. This crate sits in the
//! middle, doing the *operations* on those streams that aren't tied to any
//! specific format.
//!
//! [`Feature`]: geonative_core::Feature

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod profile;
pub mod raster;

pub use profile::{profile, FieldStats, GeometryStats, ProfileOptions, ProfileReport};
pub use raster::{
    build_overviews, compute_target_grid, resample_tile, sample, warp_tile, PyramidOptions,
    WarpOptions,
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
