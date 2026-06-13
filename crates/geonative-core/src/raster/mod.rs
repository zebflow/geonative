//! # Raster IR
//!
//! The intermediate representation for raster geospatial data — the
//! counterpart of `Feature` / `Geometry` / `Schema` on the vector side.
//! Every raster format crate (`geonative-geotiff`, future `-mbtiles`,
//! `-pmtiles`, `-zarr`) reads bytes into these types and writes them out
//! again; every raster operation (`processing::raster::reproject`,
//! `::resample`, `::clip_by_polygon`, `::hillshade`) consumes and produces
//! them.
//!
//! ## How it mirrors the vector side
//!
//! | Vector (top of `core`) | Raster (`core::raster`) |
//! | --- | --- |
//! | [`Geometry`](crate::Geometry) tree | [`RasterTile`] — pixels + dimensions + dtype |
//! | [`Feature`](crate::Feature) (fid + geom + attrs) | [`RasterTile`] *is* the row analog — one chunk of pixels |
//! | [`Schema`](crate::Schema) | [`RasterProfile`] — bands + dtype + nodata + geo-transform |
//! | [`Coord`](crate::Coord) | [`PixelType`] enum — U8/U16/I16/F32/F64/Rgb8/Rgba8 |
//! | [`Value`](crate::Value) / [`ValueType`](crate::ValueType) | [`Band`] — one channel, with name, dtype, nodata, raw bytes |
//! | [`Layer`](crate::Layer) trait | [`RasterLayer`] trait — extent + pyramid levels + `read_tile(level, x, y)` |
//!
//! ## What's shared with the vector side (the payoff for one-crate)
//!
//! - [`Crs`](crate::Crs) — same CRS system; EPSG:7855 is EPSG:7855 whether
//!   it tags a `Geometry` or a `RasterTile`
//! - [`Coord`](crate::Coord) — used by [`GeoTransform`] for the world-space
//!   origin of pixel (0, 0)
//! - [`Geometry`](crate::Geometry) — needed by cross-modal ops like
//!   `clip_by_polygon(tile: &RasterTile, polygon: &Geometry)`
//! - [`Error`](crate::Error) — same error machinery
//! - bbox `[f64; 4]` — universal "where in the world"
//!
//! ## Cross-modal ops (live in `geonative-processing`, but they're the
//! reason both IRs belong in one crate)
//!
//! ```ignore
//! // From geonative_processing::raster (future)
//! fn rasterize(features: impl Iterator<Item = Feature>, profile: &RasterProfile) -> RasterTile;
//! fn vectorize(tile: &RasterTile) -> Vec<Feature>;
//! fn clip_by_polygon(tile: &RasterTile, polygon: &Geometry) -> RasterTile;
//! fn extract_at_points(tile: &RasterTile, points: &[Coord]) -> Vec<Value>;
//! fn zonal_statistics(tile: &RasterTile, zones: &[Feature]) -> Vec<ZoneStats>;
//! ```
//!
//! All of these need *both* IRs in scope — which is why the raster IR lives
//! in `core` next to the vector IR, not in a separate `geonative-raster-core`
//! crate.
//!
//! ## Cargo feature
//!
//! This module is gated behind the `raster` Cargo feature on
//! `geonative-core`. Vector-only consumers (most format crates today,
//! anyone reading `.gdb` → writing `.parquet`) don't pay the compile cost
//! of these type definitions.

pub mod band;
pub mod layer;
pub mod profile;
pub mod tile;
pub mod transform;

pub use band::{Band, BandDescriptor, PixelType};
pub use layer::{RasterLayer, ResamplingMethod};
pub use profile::RasterProfile;
pub use tile::RasterTile;
pub use transform::GeoTransform;
