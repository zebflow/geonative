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

#![forbid(unsafe_code)]

pub mod bytes;
pub mod error;
pub mod table;
pub mod tablx;

pub use error::{GdbError, Result};
pub use table::{
    parse_field_section, parse_table_header, Field, FieldSection, FieldTypeCode, GeomFieldMeta,
    LayerFlags, Table, TableHeader, TableVersion,
};
pub use tablx::{parse_tablx_header, Tablx, TablxHeader, TablxVersion};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
