//! # geonative-utils
//!
//! Geometric algorithms that operate on the geonative-core IR but don't
//! belong in `core` itself (core stays focused on data model + codec
//! primitives). When MVT/WMS/raster output needs Level-of-Detail
//! simplification, this is where the math lives.
//!
//! ## What's in it
//!
//! - [`simplify::douglas_peucker`] — the canonical line-simplification
//!   algorithm. Operates on a `&[Coord]` slice.
//! - [`simplify::simplify_geometry`] — dispatch helper: applies DP to every
//!   linear part of a `Geometry`, preserves ring closure on polygons,
//!   passes Point/MultiPoint/Empty through unchanged.
//! - [`simplify::tolerance_for_zoom`] — small preset table mapping web-map
//!   zoom levels to sensible degree-based tolerances. Calibrated for
//!   lat/lng input (≈ pixel-equivalent at the equator).
//!
//! ## Clever bits
//!
//! - **Ring closure preserved.** Polygon rings come in closed
//!   (`first == last`); naive DP could drop the duplicate. We snapshot the
//!   closing vertex and re-append it after simplification if it was lost.
//! - **No coord allocation when below the keep threshold.** Tiny rings
//!   (`len ≤ 3` for lines, `len ≤ 4` for closed rings) pass through unchanged
//!   to avoid both extra work and the risk of degeneration.
//! - **Tolerance is in input units.** Pass lng/lat tolerance for WGS84 data
//!   (use [`simplify::tolerance_for_zoom`]); pass meters for projected data.
//!   No automatic CRS-aware tolerance — that would require knowing the CRS,
//!   which we deliberately leave to the caller.

//! ## Module organization
//!
//! Algorithms are grouped by domain (`simplify`, future `measure`, `index`,
//! `orient`, …). The crate root **deliberately does not re-export** them — we
//! want callers to write `geonative_utils::simplify::douglas_peucker(...)` so
//! the domain stays visible at the call site. This pattern mirrors how `std`
//! organises its utility surfaces (`std::fmt`, `std::iter`, `std::collections`)
//! and how the `geo` crate organises algorithms.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod simplify;
