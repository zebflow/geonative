//! # geonative-filegdb
//!
//! Pure-Rust reader for the Esri **File Geodatabase** (`.gdb`) format, part of
//! the [`geonative`](https://geonative.zebflow.com) geospatial library.
//!
//! No GDAL, no FileGDB SDK, no C/C++ dependencies. Built from the
//! reverse-engineered FGDB specification.
//!
//! **This is a placeholder release** to reserve the crate name. The real API
//! is in active development at <https://github.com/zebflow/geonative>.

#![forbid(unsafe_code)]

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
