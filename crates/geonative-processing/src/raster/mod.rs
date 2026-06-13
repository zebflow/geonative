//! # raster
//!
//! Raster operations on the `core::raster` IR — the operations Zebflow's
//! tile server composes to ingest user uploads and serve tiles.
//!
//! ## Modules
//!
//! - [`pixel`] — raw byte ↔ `f64` codec per
//!   [`PixelType`](geonative_core::raster::PixelType). The bridge that lets
//!   every kernel work in `f64` regardless of source dtype.
//! - [`resample`] — six resampling kernels (Nearest / Bilinear / Cubic /
//!   Lanczos / Average / Mode) + tile-to-tile resize. The foundation
//!   everything else builds on.
//! - [`pyramid`] — generates overview levels via repeated downsampling.
//!   Used by the writer to auto-build COG pyramids.
//! - [`warp`] — reproject a `RasterTile` to a different CRS via
//!   `geonative-proj` + a resampling kernel. The piece that lets
//!   `convert --to-crs` work for raster.

pub mod pixel;
pub mod pyramid;
pub mod resample;
pub mod warp;

pub use pyramid::{build_overviews, PyramidOptions};
pub use resample::{resample_tile, sample};
pub use warp::{compute_target_grid, warp_tile, WarpOptions};
