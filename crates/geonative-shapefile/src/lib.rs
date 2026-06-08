//! # geonative-shapefile
//!
//! Pure-Rust reader for the Esri Shapefile family (`.shp` + `.shx` + `.dbf`
//! + optional `.prj` / `.cpg`). No GDAL, no shapelib.
//!
//! ## What v0.1 covers
//!
//! - 2D shape types: `Null`, `Point`, `Polyline`, `Polygon`, `MultiPoint`
//! - DBF field types: `C` (char), `N` (numeric), `F` (float), `D` (date),
//!   `L` (logical); other DBF types come through as `Value::String` or `Null`
//! - Ring orientation flipped from Shapefile (CW outer / CCW hole) to OGC
//!   (CCW outer / CW hole) so polygons match the rest of the geonative IR
//! - `.shp` is **mmapped** for bounded RAM usage on multi-GB inputs;
//!   `.shx` + `.dbf` are read into Vecs (small)
//!
//! ## v0.1 scope cuts
//!
//! - Z/M variants (PointZ, PolygonZ, MultipointZ, etc.) return Unsupported
//! - MultiPatch (type 31) — surface-class geometry; defer
//! - Non-UTF-8 codepages (`.cpg` / LDID full table) — defer; v0.1 treats
//!   string bytes as UTF-8 with replacement for invalid sequences
//! - `.qix` / `.sbn` spatial index files — ignored
//! - Sparse / corrupt-`.shx` repair — error rather than auto-rebuild
//!
//! ## Usage
//!
//! ```no_run
//! let shp = geonative_shapefile::Shapefile::open("roads.shp")?;
//! println!("{} features, schema: {:?}", shp.feature_count(), shp.schema());
//! for f in shp.read() {
//!     let f = f?;
//!     // f.fid, f.geometry, f.attributes
//! }
//! # Ok::<(), geonative_shapefile::ShpError>(())
//! ```

// We `deny` rather than `forbid` so the single mmap call in `dataset.rs`
// can explicitly opt in via `#[allow(unsafe_code)]`. Every other module
// remains safe-Rust-only.
#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod bytes;
pub mod dataset;
pub mod dbf;
pub mod error;
pub mod header;
pub mod shape;
pub mod shx;

pub use dataset::{FeatureIter, Shapefile};
pub use error::{Result, ShpError};
pub use header::{ShapeType, ShpHeader};

/// Convenience: open a `.shp` (or any file in the shapefile triad) and
/// produce a `Shapefile` handle.
pub fn open(path: impl AsRef<std::path::Path>) -> Result<Shapefile> {
    Shapefile::open(path)
}
