//! # geonative-filegdb
//!
//! Pure-Rust reader for the Esri **File Geodatabase** (`.gdb`) format. Part of
//! the [`geonative`](https://geonative.zebflow.com) geospatial library.
//!
//! No GDAL, no FileGDB SDK, no C/C++ dependencies. Built directly against the
//! reverse-engineered FGDB-Spec.
//!
//! **v0.0.1 is a placeholder.** Real read API in active development at
//! <https://github.com/zebflow/geonative>.

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
