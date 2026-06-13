//! # geonative-image
//!
//! Pure-Rust reader for raster images with world-file sidecars — JPG and
//! PNG with `.jgw` / `.pgw` / `.wld` next to them.
//!
//! ## What v0.1 covers
//!
//! - **JPEG** decode via `jpeg-decoder` (L8 grayscale + RGB24)
//! - **PNG** decode via `png` (8-bit Grayscale / Grayscale+Alpha / RGB / RGBA)
//! - **World files** (`.jgw` / `.pgw` / `.tfw` / `.wld`) — 6-line ASCII
//!   parsed into a [`geonative_core::raster::GeoTransform`]
//! - **Caller-supplied CRS** — world files don't carry CRS, so the caller
//!   passes one via [`ImageRaster::open_with_crs`] (or accepts
//!   [`geonative_core::Crs::Unknown`] via the basic [`ImageRaster::open`])
//! - Implements [`geonative_core::raster::RasterLayer`] — interchangeable
//!   with `geonative-geotiff::GeoTiff` in `geonative-convert`'s
//!   `RasterSource` dispatch
//!
//! ## Memory note
//!
//! These formats don't tile internally — `open` decodes the whole image
//! into memory. For large inputs (multi-GB satellite imagery), the
//! recommended workflow is to convert to COG once:
//!
//! ```bash
//! geonative convert big.jpg big.cog
//! ```
//!
//! Then serve from the COG via `geonative-geotiff`'s mmap-backed reader —
//! that path is Pi-friendly regardless of source size.
//!
//! ## Usage
//!
//! ```no_run
//! use geonative_core::Crs;
//! use geonative_core::raster::RasterLayer;
//! use geonative_image::ImageRaster;
//!
//! // upload.png is sitting next to upload.pgw on disk
//! let img = ImageRaster::open_with_crs("upload.png", Crs::Epsg(4326))?;
//! let p = img.profile();
//! println!("{}×{} px, {} bands", p.width, p.height, p.bands.len());
//! let tile = img.read_tile(0, 0, 0)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod dataset;
pub mod error;
pub mod worldfile;

pub use dataset::{ImageKind, ImageRaster};
pub use error::{ImageError, Result};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
