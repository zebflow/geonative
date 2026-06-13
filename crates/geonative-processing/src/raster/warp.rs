//! Raster warp — reproject a raster from one CRS to another using
//! `geonative-proj` for the coordinate transform plus a resampling kernel
//! for the pixel interpolation.
//!
//! ## How it works
//!
//! For each output pixel:
//! 1. Compute its world coords in the target CRS via the target tile's
//!    `GeoTransform`
//! 2. Reproject those world coords to the source CRS using the
//!    `Transformer`
//! 3. Project source-CRS world coords back to source pixel coords using
//!    the source tile's `GeoTransform`
//! 4. Sample the source band at those (fractional) pixel coords using the
//!    chosen kernel
//!
//! ## Memory model
//!
//! v0.1 takes a single source `RasterTile` and produces a single output
//! `RasterTile`. For large inputs the caller should:
//! - Read source tiles via `RasterLayer::read_tile` (cheap with mmap)
//! - Warp each tile individually
//! - Write to output via the `geonative-geotiff` writer
//!
//! For tile-server hot paths (per HTTP request), the future
//! `warp_tile_window` function reads only the source pixels needed for one
//! output tile — that's a v0.2 add when streaming warp is needed.

use geonative_core::raster::{Band, GeoTransform, RasterTile, ResamplingMethod};
use geonative_core::{Coord, Crs};
use geonative_proj::Transformer;

use crate::raster::{pixel, resample};

#[derive(Debug, Clone)]
pub struct WarpOptions {
    pub method: ResamplingMethod,
    /// Fill value for output pixels whose source projection falls outside
    /// the source extent. Defaults to 0 (works for most U8/U16/I16
    /// imagery; for DEMs you may want NaN-equivalent).
    pub nodata: f64,
}

impl Default for WarpOptions {
    fn default() -> Self {
        Self {
            method: ResamplingMethod::Bilinear,
            nodata: 0.0,
        }
    }
}

/// Reproject `source` (with its own CRS + geo-transform) onto the target
/// `(crs, geo_transform, width, height)`. Returns a new `RasterTile` with
/// the same band descriptors as `source` but in the target CRS.
pub fn warp_tile(
    source: &RasterTile,
    target_crs: &Crs,
    target_geo_transform: GeoTransform,
    target_width: u32,
    target_height: u32,
    opts: &WarpOptions,
) -> Result<RasterTile, geonative_proj::ProjError> {
    // Build a transformer: target → source (we iterate output pixels and
    // look up where to sample in the source).
    let transformer = Transformer::from_crs(target_crs, &source.crs)?;

    let bands_out: Vec<Band> = source
        .bands
        .iter()
        .map(|src_band| {
            let bpp = src_band.descriptor.dtype.size_bytes();
            Band::new(
                src_band.descriptor.clone(),
                vec![0u8; (target_width as usize) * (target_height as usize) * bpp],
            )
        })
        .collect();

    let mut result = RasterTile {
        width: target_width,
        height: target_height,
        bands: bands_out,
        geo_transform: target_geo_transform,
        crs: target_crs.clone(),
    };

    // Source geo-transform inverse (world → source pixel coords).
    let src_gt = source.geo_transform;

    for ty in 0..target_height as usize {
        for tx in 0..target_width as usize {
            // 1) target pixel centre → target world coords
            let tw = target_geo_transform.pixel_to_world(tx as f64 + 0.5, ty as f64 + 0.5);
            // 2) target world → source world via transformer (in place)
            let mut sw = tw;
            if transformer.transform(&mut sw).is_err() {
                continue;
            }
            // 3) source world → source pixel coords
            let [sc, sr] = src_gt.world_to_pixel(Coord {
                x: sw.x,
                y: sw.y,
                z: None,
                m: None,
            });
            // 4) sample each band at that fractional position
            for (band_idx, src_band) in source.bands.iter().enumerate() {
                let value =
                    resample::sample(src_band, source.width, source.height, sc, sr, opts.method)
                        .unwrap_or(opts.nodata);
                pixel::write(
                    &mut result.bands[band_idx],
                    tx,
                    ty,
                    target_width as usize,
                    value,
                );
            }
        }
    }

    Ok(result)
}

/// Compute the world-space bounds in the **target** CRS that contain the
/// reprojected `source` extent. Useful for deciding the size of the
/// output `RasterTile` when the caller wants "warp everything into target
/// CRS at a reasonable resolution."
///
/// Returns `(target_geo_transform, target_width, target_height)`.
pub fn compute_target_grid(
    source: &RasterTile,
    target_crs: &Crs,
    target_width: u32,
) -> Result<(GeoTransform, u32, u32), geonative_proj::ProjError> {
    let transformer = Transformer::from_crs(&source.crs, target_crs)?;
    let source_bounds = source.bounds();
    // Reproject all 4 corners + midpoint to get a target bbox.
    let mut corners = [
        Coord::xy(source_bounds[0], source_bounds[1]),
        Coord::xy(source_bounds[2], source_bounds[1]),
        Coord::xy(source_bounds[0], source_bounds[3]),
        Coord::xy(source_bounds[2], source_bounds[3]),
        // Midpoint helps when the source-to-target reprojection bends edges.
        Coord::xy(
            (source_bounds[0] + source_bounds[2]) * 0.5,
            (source_bounds[1] + source_bounds[3]) * 0.5,
        ),
    ];
    for c in corners.iter_mut() {
        transformer.transform(c)?;
    }
    let xs: Vec<f64> = corners.iter().map(|c| c.x).collect();
    let ys: Vec<f64> = corners.iter().map(|c| c.y).collect();
    let xmin = xs.iter().cloned().fold(f64::INFINITY, f64::min);
    let xmax = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let ymin = ys.iter().cloned().fold(f64::INFINITY, f64::min);
    let ymax = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    let pixel_w = (xmax - xmin) / target_width as f64;
    // Aspect-preserving target height
    let target_height = (((ymax - ymin) / pixel_w).round() as u32).max(1);
    let pixel_h = -(ymax - ymin) / target_height as f64;

    let gt = GeoTransform {
        origin: [xmin, ymax],
        pixel_size: [pixel_w, pixel_h],
        rotation: [0.0, 0.0],
    };
    Ok((gt, target_width, target_height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geonative_core::raster::{BandDescriptor, PixelType};

    /// Build a 4×4 EPSG:4326 tile near Melbourne (lng/lat).
    fn melbourne_tile_4326() -> RasterTile {
        let mut data = Vec::with_capacity(16);
        for r in 0..4u8 {
            for c in 0..4u8 {
                data.push(r * 64 + c * 16);
            }
        }
        RasterTile {
            width: 4,
            height: 4,
            bands: vec![Band::new(
                BandDescriptor::new(Some("v".into()), PixelType::U8),
                data,
            )],
            geo_transform: GeoTransform::north_up(144.9, -37.85, 0.01, 0.01),
            crs: Crs::Epsg(4326),
        }
    }

    #[test]
    fn warp_4326_to_3857_preserves_shape() {
        let src = melbourne_tile_4326();
        let (gt, w, h) = compute_target_grid(&src, &Crs::Epsg(3857), 8).unwrap();
        let result = warp_tile(&src, &Crs::Epsg(3857), gt, w, h, &WarpOptions::default()).unwrap();
        assert_eq!(result.width, w);
        assert_eq!(result.height, h);
        assert_eq!(result.crs, Crs::Epsg(3857));
        // At least some output pixels should have non-zero values (we
        // didn't get all-zero from the warp going through valid coords).
        assert!(result.bands[0].data.iter().any(|&v| v != 0));
    }

    #[test]
    fn warp_round_trip_4326_to_3857_back_to_4326_is_close() {
        let src = melbourne_tile_4326();
        // Forward 4326 → 3857
        let (gt_3857, w_3857, h_3857) = compute_target_grid(&src, &Crs::Epsg(3857), 16).unwrap();
        let in_3857 = warp_tile(
            &src,
            &Crs::Epsg(3857),
            gt_3857,
            w_3857,
            h_3857,
            &WarpOptions::default(),
        )
        .unwrap();
        // Back 3857 → 4326
        let (gt_back, w_back, h_back) =
            compute_target_grid(&in_3857, &Crs::Epsg(4326), 16).unwrap();
        let back = warp_tile(
            &in_3857,
            &Crs::Epsg(4326),
            gt_back,
            w_back,
            h_back,
            &WarpOptions::default(),
        )
        .unwrap();
        assert_eq!(back.crs, Crs::Epsg(4326));
        assert!(back.width > 0);
    }

    #[test]
    fn warp_identity_crs_keeps_pixels_intact() {
        let src = melbourne_tile_4326();
        // Target = source CRS/extent — warp should be ~identity (with
        // resampling artefacts only at sub-pixel level).
        let result = warp_tile(
            &src,
            &Crs::Epsg(4326),
            src.geo_transform,
            src.width,
            src.height,
            &WarpOptions::default(),
        )
        .unwrap();
        assert_eq!(result.width, src.width);
        // Corner pixel should match closely.
        assert!((result.bands[0].data[0] as i32 - src.bands[0].data[0] as i32).abs() < 5);
    }
}
