//! [`RasterLayer`] trait — what a raster-format reader exposes.
//!
//! Mirrors the vector [`Layer`](crate::Layer) trait but with raster
//! semantics: instead of iterating features, callers issue
//! `read_tile(level, x, y)` to pull one chunk of pixels at a chosen
//! pyramid level. The level-0 origin is the full-resolution image;
//! higher levels are coarser overviews (level 1 = ½ resolution per side,
//! level 2 = ¼, …).
//!
//! ## Object safety
//!
//! Object-safe so format-polymorphic code (`geonative-convert`'s
//! `RasterSource`) can dispatch on `Box<dyn RasterLayer>`. The trait
//! intentionally returns owned [`RasterTile`]s (not borrows) so
//! implementations can either decode into a fresh buffer or hand off a
//! pre-decoded one without lifetime gymnastics.

use crate::Result;

use super::{RasterProfile, RasterTile};

/// Read-only access to a raster layer.
pub trait RasterLayer {
    /// Layer structure (dimensions, bands, CRS, tile size, pyramid depth).
    fn profile(&self) -> &RasterProfile;

    /// Read one tile at the requested pyramid level + tile-grid (x, y)
    /// coordinate.
    ///
    /// Coordinates are in **tile space**, not pixel space:
    /// - `level = 0`: full resolution
    /// - `level = 1`: half resolution per side
    /// - `(x, y)`: tile index within the level's grid
    ///
    /// Returns an error if the level / tile coordinate is out of range.
    fn read_tile(&self, level: u8, x: u32, y: u32) -> Result<RasterTile>;

    /// Total number of pyramid levels (≥ 1). Default impl reads from the
    /// profile; readers with dynamic pyramid availability can override.
    fn pyramid_levels(&self) -> u8 {
        self.profile().pyramid_levels
    }
}

/// Resampling strategies used by `processing::raster::reproject` and
/// `processing::raster::resample`. Defined here in `core` so format
/// crates that produce overviews can pick one without depending on
/// `geonative-processing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResamplingMethod {
    /// Pick the nearest source pixel. Fastest; preserves categorical
    /// values exactly. Use for land-cover / classification rasters.
    Nearest,
    /// Average four source pixels with linear weights. Smooth output;
    /// the default for continuous imagery (DEMs, satellite RGB).
    Bilinear,
    /// 4×4 bicubic kernel. Sharper than bilinear for visual imagery;
    /// slower.
    Cubic,
    /// 6×6 (or wider) Lanczos kernel. Highest quality; slowest. Often
    /// used for the final-output downsampling step.
    Lanczos,
    /// Average all source pixels within the target. Best for building
    /// pyramid overviews of continuous data.
    Average,
    /// Most-frequent value within the target. For categorical /
    /// classification overviews.
    Mode,
}
