//! # geonative-geotiff
//!
//! Pure-Rust GeoTIFF + Cloud Optimized GeoTIFF (COG) reader for the
//! [`geonative`](https://geonative.zebflow.com) geospatial library.
//!
//! ## What v0.1 covers
//!
//! - Classic TIFF + BigTIFF headers
//! - Tiled layouts (TileWidth / TileLength / TileOffsets / TileByteCounts) —
//!   the COG-friendly arrangement
//! - Compression codecs: None, PackBits, LZW (via `weezl`), DEFLATE
//!   (via `flate2`)
//! - GeoTIFF tags (ModelPixelScale + ModelTiepoint → `GeoTransform`,
//!   GeoKeyDirectory → `Crs`)
//! - EPSG-coded CRS via `ProjectedCSTypeGeoKey` / `GeographicTypeGeoKey`
//! - **mmap-backed** file access — a 50 GB COG serves one 256×256 tile
//!   from ~50 KB of I/O, ~200 KB RAM
//! - Implements [`geonative_core::raster::RasterLayer`]
//!
//! ## v0.1 scope cuts
//!
//! - **Stripped TIFFs** — most legacy GeoTIFFs use strips; planned for
//!   Phase B2. v0.1 errors with a clear "stripped TIFFs (Phase B2)" message.
//! - **JPEG-in-TIFF** (compression 7) — deferred to v0.2
//! - **Predictor 2/3** (horizontal differencing) — deferred; most COGs use predictor=1
//! - **PlanarConfiguration=2** (planar pixel layout) — deferred; almost
//!   all real-world TIFFs are chunky / interleaved
//! - **ModelTransformation** (full 4×4 affine) — deferred to v0.2; v0.1
//!   handles north-up tiepoint+scale pairs (the universal case)
//! - **WKT-in-GeoKeys** — only EPSG codes today; arbitrary WKT projections
//!   land with `geonative-proj`'s WKT parser in v0.2
//! - **Writer** — Phase C of Sprint 13
//!
//! ## Usage
//!
//! ```no_run
//! use geonative_geotiff::GeoTiff;
//! use geonative_core::raster::RasterLayer;
//!
//! let dem = GeoTiff::open("elevation.cog")?;
//! let profile = dem.profile();
//! println!("{}×{} px, {} bands, CRS {:?}", profile.width, profile.height,
//!          profile.bands.len(), profile.crs);
//! // Read one 256×256 tile at full resolution
//! let tile = dem.read_tile(0, 5, 3)?;
//! # Ok::<(), geonative_core::Error>(())
//! ```

// We `deny` rather than `forbid` so the single mmap call in `dataset.rs`
// can explicitly opt in via `#[allow(unsafe_code)]`. Every other module
// remains safe-Rust-only.
#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod codec;
pub mod dataset;
pub mod error;
pub mod format;
pub mod geokeys;
pub mod writer;

pub use dataset::GeoTiff;
pub use error::{GtiffError, Result};
pub use writer::{profile_for_output, Compression, GeoTiffWriter, WriterOptions};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
