//! # geonative-filegdb
//!
//! Pure-Rust reader for the Esri **File Geodatabase** (`.gdb`) format. No
//! GDAL, no FileGDB SDK, no C/C++ dependencies. Built directly against the
//! reverse-engineered FGDB-Spec, validated against GDAL OpenFileGDB on real
//! Vicmap-style data.
//!
//! ## Module map
//!
//! - [`bytes`] — low-level LE byte primitives + the FileGDB-flavored
//!   varuint / varint encodings (sign bit is bit 0x40 of the first byte —
//!   *not* zigzag like protobuf)
//! - [`table`] — parses `.gdbtable` header + field-description section,
//!   including geometry-field metadata (xyscale, SRS WKT, sub-flags)
//! - [`tablx`] — parses `.gdbtablx` row-offset index with sparse-block
//!   bitmap support
//! - [`row`] — decodes one row blob (null bitmap + per-type attribute
//!   decoding) into `geonative_core::Value`s
//! - [`geometry`] — shape-buffer decoder (varint deltas → coords;
//!   re-orients Esri CW/CCW ring winding to OGC convention)
//! - [`catalog`] — `GDB_SystemCatalog` parse → layer name ↔ physical file map
//! - [`dataset`] — public API: `Geodatabase` + `Layer` + iterator. Mmap-backed.
//!
//! ## Quick start
//!
//! ```no_run
//! let gdb = geonative_filegdb::open("foo.gdb")?;
//! for info in gdb.layers() {
//!     let layer = gdb.layer(&info.name)?;
//!     for f in layer.read() {
//!         let f = f?;
//!         // f.fid, f.geometry, f.attributes
//!     }
//! }
//! # Ok::<(), geonative_filegdb::GdbError>(())
//! ```
//!
//! ## Scope (v0.1)
//!
//! 2D geometry only (Point / MultiPoint / Polyline / Polygon + the
//! "General" curve variants whose linear samples we keep, curves dropped).
//! Z/M, multipatch, sparse 64-bit OID v4 tables, SDC/CDF compression, and
//! `GDB_Items` XML parsing are scope cuts for v0.1 — see TODO.md backlog.

// We `deny` rather than `forbid` so the single mmap call in `dataset.rs` can
// explicitly opt in via `#[allow(unsafe_code)]`. Every other module remains
// safe-Rust-only.
#![deny(unsafe_code)]

pub mod bytes;
pub mod catalog;
pub mod dataset;
pub mod error;
pub mod geometry;
pub mod row;
pub mod table;
pub mod tablx;

pub use catalog::{open_geodatabase, physical_filename_for_fid, read_catalog, LayerInfo};
pub use dataset::{FeatureIter, Geodatabase, Layer};
pub use error::{GdbError, Result};
pub use geometry::decode_shape_buffer;
pub use row::{decode_row_blob, slice_row_blob, DecodedRow};

/// Convenience: open a `.gdb` directory. Equivalent to
/// [`Geodatabase::open`].
pub fn open(path: impl AsRef<std::path::Path>) -> Result<Geodatabase> {
    Geodatabase::open(path)
}
pub use table::{
    parse_field_section, parse_table_header, Field, FieldSection, FieldTypeCode, GeomFieldMeta,
    LayerFlags, Table, TableHeader, TableVersion,
};
pub use tablx::{parse_tablx_header, Tablx, TablxHeader, TablxVersion};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
