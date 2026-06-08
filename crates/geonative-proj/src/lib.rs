//! # geonative-proj
//!
//! Pure-Rust projection engine for the
//! [`geonative`](https://geonative.zebflow.com) geospatial library. No PROJ,
//! no C dep — every projection is implemented from scratch.
//!
//! ## What v0.1 covers
//!
//! - **EPSG:4326 ↔ EPSG:3857** — spherical Web Mercator
//! - **WGS84/UTM zones 1–60 N+S** (EPSG:32601–32760)
//! - **GDA2020/MGA zones 46–59** (EPSG:7846–7859) — Transverse Mercator on
//!   GRS80 ellipsoid, southern-hemisphere false-northing
//! - **EPSG:7844 ≡ EPSG:4326** for v0.1 (treated as identity; ~1.5 m
//!   visualisation-grade caveat — full Helmert 7-param transform is v0.2)
//!
//! Two projected CRSes (e.g. MGA-55 → WGS84/UTM-54) automatically pipe
//! through 4326.
//!
//! ## Accuracy
//!
//! - Web Mercator is **exact** per the spherical-Mercator definition.
//! - TM (UTM/MGA) uses **Krüger n-series, 6th order** — sub-millimetre
//!   accuracy out to ±4° from the central meridian (well past the standard
//!   ±3° zone half-width).
//!
//! ## Usage
//!
//! ```
//! use geonative_proj::Transformer;
//! use geonative_core::Coord;
//!
//! let t = Transformer::from_epsg(7855, 4326)?;   // Vicmap MGA Zone 55 → WGS84
//! let mut c = Coord::xy(500000.0, 5800000.0);
//! t.transform(&mut c)?;
//! // c.x ≈ 147.0 (central meridian), c.y ≈ -37.95
//! # Ok::<(), geonative_proj::ProjError>(())
//! ```
//!
//! ## How it composes with the rest of the stack
//!
//! `geonative-convert`'s pipeline picks up a `Transformer` from the
//! `--to-crs` flag and applies `transform_geometry` to each feature
//! mid-stream — one method call per feature, no allocation.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod ellipsoid;
pub mod epsg;
pub mod error;
pub mod spherical_mercator;
pub mod tm;
pub mod transformer;

pub use ellipsoid::{Ellipsoid, GRS80, WGS84};
pub use error::{ProjError, Result};
pub use transformer::Transformer;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
