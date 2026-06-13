//! Pyramid (overview) generation.
//!
//! Takes a full-resolution `RasterTile` and produces a chain of
//! half-resolution tiles — one per pyramid level — using a resampling
//! kernel.
//!
//! ## Convention
//!
//! - **Level 0** is the input (full resolution).
//! - **Level N** has dimensions `width >> N` × `height >> N` (rounded
//!   down, minimum 1).
//! - We stop at the level where both dimensions are ≤ `min_dimension`
//!   (default 256) — no point in serving a 64×64 thumbnail at zoom 0 of
//!   a global mosaic.

use geonative_core::raster::{RasterTile, ResamplingMethod};

use crate::raster::resample;

#[derive(Debug, Clone)]
pub struct PyramidOptions {
    /// Resampling kernel to use for each downsample step. Default
    /// `Average` (good for continuous imagery); use `Mode` for
    /// categorical / classification rasters.
    pub method: ResamplingMethod,
    /// Stop adding levels once both dimensions are ≤ this. Default 256.
    pub min_dimension: u32,
    /// Hard cap on the number of overview levels (excludes level 0).
    /// Default 16 (covers global → 256×256 thumbnail).
    pub max_levels: u8,
}

impl Default for PyramidOptions {
    fn default() -> Self {
        Self {
            method: ResamplingMethod::Average,
            min_dimension: 256,
            max_levels: 16,
        }
    }
}

/// Produce a chain of overview tiles from `base` (treated as level 0).
/// The returned `Vec` does NOT include the base — index 0 of the result is
/// the level-1 overview (half resolution).
pub fn build_overviews(base: &RasterTile, opts: PyramidOptions) -> Vec<RasterTile> {
    let mut out = Vec::new();
    let mut current = base.clone();
    for _ in 0..opts.max_levels {
        let next_w = (current.width / 2).max(1);
        let next_h = (current.height / 2).max(1);
        if next_w <= opts.min_dimension && next_h <= opts.min_dimension {
            // Add one final overview at this scale, then stop.
            let overview = resample::resample_tile(&current, next_w, next_h, opts.method);
            out.push(overview);
            break;
        }
        if next_w == current.width && next_h == current.height {
            // No progress — input is already at minimum size
            break;
        }
        let overview = resample::resample_tile(&current, next_w, next_h, opts.method);
        out.push(overview.clone());
        current = overview;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use geonative_core::raster::{Band, BandDescriptor, GeoTransform, PixelType};
    use geonative_core::Crs;

    fn make_tile(width: u32, height: u32) -> RasterTile {
        RasterTile {
            width,
            height,
            bands: vec![Band::new(
                BandDescriptor::new(Some("v".into()), PixelType::U8),
                vec![100u8; (width * height) as usize],
            )],
            geo_transform: GeoTransform::north_up(0.0, 0.0, 1.0, 1.0),
            crs: Crs::Epsg(3857),
        }
    }

    #[test]
    fn builds_overviews_until_min_dimension() {
        let base = make_tile(1024, 1024);
        let overviews = build_overviews(
            &base,
            PyramidOptions {
                min_dimension: 256,
                ..PyramidOptions::default()
            },
        );
        // 1024 → 512 → 256 → 128 ... we stop at the first ≤256.
        // Sequence: 512, 256 (final, since both ≤ 256). So 2 levels.
        assert_eq!(overviews.len(), 2);
        assert_eq!(overviews[0].width, 512);
        assert_eq!(overviews[1].width, 256);
    }

    #[test]
    fn respects_max_levels_cap() {
        let base = make_tile(8192, 8192);
        let overviews = build_overviews(
            &base,
            PyramidOptions {
                min_dimension: 64,
                max_levels: 3,
                ..PyramidOptions::default()
            },
        );
        assert_eq!(overviews.len(), 3);
        // 8192 → 4096 → 2048 → 1024
        assert_eq!(overviews[2].width, 1024);
    }

    #[test]
    fn stops_at_min_dimension_input() {
        let base = make_tile(128, 128);
        let overviews = build_overviews(
            &base,
            PyramidOptions {
                min_dimension: 256,
                ..PyramidOptions::default()
            },
        );
        // Input is already ≤ min_dimension → one final downsample then stop
        assert_eq!(overviews.len(), 1);
    }

    #[test]
    fn pixel_size_scales_correctly_through_pyramid() {
        let base = make_tile(512, 512);
        let overviews = build_overviews(&base, PyramidOptions::default());
        // Original pixel_size [1, -1]; each level halves resolution → pixel size doubles
        // 512 → 256 (1 level), pixel size [2, -2]
        assert_eq!(overviews[0].geo_transform.pixel_size, [2.0, -2.0]);
    }
}
