//! # geonative
//!
//! A lightweight, pure-Rust geospatial library — built from scratch with zero
//! C/C++ dependencies.
//!
//! `geonative` is a **meta-crate**: it pulls in focused sub-crates only when
//! you opt in via Cargo features. The default build is empty:
//!
//! ```toml
//! [dependencies]
//! # zero default features — pulls nothing in
//! geonative = "0.0.1"
//!
//! # opt in to what you need
//! geonative = { version = "0.0.1", features = ["file-gdb", "file-geoparquet", "convert"] }
//! ```
//!
//! ## Planned feature → sub-crate map
//!
//! | Feature | Re-exports |
//! | --- | --- |
//! | `file-gdb` | [`geonative-filegdb`](https://crates.io/crates/geonative-filegdb) — Esri File Geodatabase reader |
//! | `file-shp` | [`geonative-shapefile`](https://crates.io/crates/geonative-shapefile) — Shapefile reader/writer |
//! | `file-geojson` | [`geonative-geojson`](https://crates.io/crates/geonative-geojson) — GeoJSON reader/writer |
//! | `file-parquet` | [`geonative-geoparquet`](https://crates.io/crates/geonative-geoparquet) — GeoParquet reader/writer |
//! | `processing` | [`geonative-processing`](https://crates.io/crates/geonative-processing) — buffer, clip, reproject, … |
//! | `convert` | [`geonative-convert`](https://crates.io/crates/geonative-convert) — ogr2ogr-style pipeline |
//!
//! **This is a placeholder release** to reserve the crate name. Sub-crate
//! re-exports will be wired up in a future version. Real API in development at
//! <https://github.com/zebflow/geonative>.

#![forbid(unsafe_code)]

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
