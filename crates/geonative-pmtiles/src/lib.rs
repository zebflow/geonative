//! # geonative-pmtiles
//!
//! Pure-Rust [PMTiles v3](https://github.com/protomaps/PMTiles/blob/main/spec/v3/spec.md)
//! reader and writer for the geonative geospatial library.
//!
//! PMTiles is a single-file tile archive: instead of a `/z/x/y.pbf`
//! directory tree (which means thousands-to-millions of tiny S3 objects),
//! the whole tileset is **one file with an internal Hilbert-ordered
//! directory**. HTTP range reads serve any tile from any cloud object
//! store with no server-side runtime.
//!
//! ## What this crate gives you
//!
//! - [`PmTilesWriter`] — build a PMTiles archive locally. Handles content
//!   dedup, Hilbert tile-id assignment, run-length merge, and adaptive
//!   leaf-dir splitting automatically.
//! - [`PmTilesReader`] — read tiles from a local file by `(z, x, y)`.
//! - [`PmTilesAsyncReader`] (behind the `s3` feature) — same, but reads
//!   from any `object_store::ObjectStore` (S3 / Azure / GCS / R2 / HTTP),
//!   range-fetching only the bytes each tile needs.
//!
//! ## Usage — write
//!
//! ```ignore
//! use geonative_pmtiles::{PmTilesWriter, WriterOptions, TileType, Compression};
//!
//! let file = std::fs::File::create("vicmap.pmtiles")?;
//! let mut w = PmTilesWriter::create(file, WriterOptions {
//!     tile_type: TileType::Mvt,
//!     tile_compression: Compression::Gzip,
//!     min_zoom: 0,
//!     max_zoom: 14,
//!     bounds: [140.0, -39.0, 150.0, -34.0], // Victoria, Australia
//!     center: (145.0, -37.0, 8),
//!     ..WriterOptions::default()
//! });
//! // Tile bytes should already be gzip-compressed MVT
//! w.add_tile(0, 0, 0, &mvt_bytes)?;
//! w.add_tile(1, 0, 0, &mvt_bytes)?;
//! w.finish()?;
//! ```
//!
//! ## Usage — read
//!
//! ```ignore
//! use geonative_pmtiles::PmTilesReader;
//!
//! let mut r = PmTilesReader::open("vicmap.pmtiles")?;
//! if let Some(bytes) = r.get_tile(8, 142, 96)? {
//!     // serve `bytes` as the response to GET /8/142/96.mvt
//! }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod codec;
pub mod directory;
pub mod error;
pub mod header;
pub mod reader;
pub mod tileid;
pub mod varint;
pub mod writer;

#[cfg(feature = "s3")]
pub mod async_reader;

pub use error::{PmtilesError, Result};
pub use header::{Compression, Header, TileType};
pub use reader::PmTilesReader;
pub use tileid::{coords_to_tile_id, tile_id_to_coords};
pub use writer::{PmTilesWriter, WriterOptions};

#[cfg(feature = "s3")]
pub use async_reader::PmTilesAsyncReader;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
