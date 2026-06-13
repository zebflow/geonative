//! Resampling kernels — the math that makes pyramid building and raster
//! warp possible.
//!
//! ## API shape
//!
//! Two layers of API:
//!
//! - **Point sampling** (`sample_*`): given a band + fractional `(x, y)`
//!   in source pixel space, return the resampled value. Used by `warp` to
//!   map one output pixel to its source value.
//! - **Tile-to-tile** ([`resample_tile`]): given a whole source
//!   `RasterTile` + target dimensions, return a new `RasterTile` resampled
//!   across all bands. Used by `pyramid` to downsample for overview levels.
//!
//! ## Kernels covered
//!
//! - [`ResamplingMethod::Nearest`] — fastest; preserves categorical values
//!   exactly. Use for landcover/classification.
//! - [`ResamplingMethod::Bilinear`] — 2×2 weighted average. The default for
//!   continuous imagery (DEMs, RGB photos).
//! - [`ResamplingMethod::Cubic`] — 4×4 cubic-convolution kernel. Sharper
//!   than bilinear at moderate extra cost.
//! - [`ResamplingMethod::Lanczos`] — 6×6 windowed-sinc. Highest quality;
//!   slowest. Used for final-output downsampling where artefacts matter.
//! - [`ResamplingMethod::Average`] — area-weighted mean. Best for building
//!   pyramid overviews of continuous data (preserves means under
//!   downsampling, unlike bilinear).
//! - [`ResamplingMethod::Mode`] — most-frequent value within target area.
//!   For categorical pyramid overviews.

use geonative_core::raster::{Band, RasterTile, ResamplingMethod};

use crate::raster::pixel;

/// Sample one fractional `(x, y)` from `band` using the chosen method.
///
/// `x` and `y` are in source pixel coordinates — `(0.5, 0.5)` is the centre
/// of pixel `(0, 0)`, `(1.5, 0.5)` is the centre of pixel `(1, 0)`, etc.
/// Returns `None` if out of bounds or the dtype is unsupported.
pub fn sample(
    band: &Band,
    width: u32,
    height: u32,
    x: f64,
    y: f64,
    method: ResamplingMethod,
) -> Option<f64> {
    let w = width as usize;
    let h = height as usize;
    match method {
        ResamplingMethod::Nearest => sample_nearest(band, w, h, x, y),
        ResamplingMethod::Bilinear => sample_bilinear(band, w, h, x, y),
        ResamplingMethod::Cubic => sample_cubic(band, w, h, x, y),
        ResamplingMethod::Lanczos => sample_lanczos(band, w, h, x, y, 3),
        // Average / Mode are area-based: they collapse a SOURCE region to one
        // value, not a point. For point-sampling we fall through to bilinear
        // — the caller wanted a point read, not an area collapse.
        ResamplingMethod::Average | ResamplingMethod::Mode => sample_bilinear(band, w, h, x, y),
        _ => sample_nearest(band, w, h, x, y),
    }
}

fn sample_nearest(band: &Band, w: usize, h: usize, x: f64, y: f64) -> Option<f64> {
    let c = x.floor() as isize;
    let r = y.floor() as isize;
    if c < 0 || r < 0 || (c as usize) >= w || (r as usize) >= h {
        return None;
    }
    pixel::read(band, c as usize, r as usize, w)
}

fn sample_bilinear(band: &Band, w: usize, h: usize, x: f64, y: f64) -> Option<f64> {
    // Bilinear samples a 2×2 neighbourhood at pixel-centre coords.
    // Subtract 0.5 so x/y describe distance from the centre of pixel (0, 0).
    let xc = x - 0.5;
    let yc = y - 0.5;
    let c0 = xc.floor() as isize;
    let r0 = yc.floor() as isize;
    let dx = xc - c0 as f64;
    let dy = yc - r0 as f64;
    let p00 = read_clamped(band, c0, r0, w, h)?;
    let p10 = read_clamped(band, c0 + 1, r0, w, h)?;
    let p01 = read_clamped(band, c0, r0 + 1, w, h)?;
    let p11 = read_clamped(band, c0 + 1, r0 + 1, w, h)?;
    let top = p00 * (1.0 - dx) + p10 * dx;
    let bot = p01 * (1.0 - dx) + p11 * dx;
    Some(top * (1.0 - dy) + bot * dy)
}

fn sample_cubic(band: &Band, w: usize, h: usize, x: f64, y: f64) -> Option<f64> {
    // Catmull-Rom cubic kernel — 4×4 neighbourhood.
    let xc = x - 0.5;
    let yc = y - 0.5;
    let c0 = xc.floor() as isize;
    let r0 = yc.floor() as isize;
    let dx = xc - c0 as f64;
    let dy = yc - r0 as f64;

    let mut rows = [0.0f64; 4];
    for (j, row_slot) in rows.iter_mut().enumerate() {
        let r = r0 + j as isize - 1;
        let mut row = [0.0f64; 4];
        for (i, slot) in row.iter_mut().enumerate() {
            let c = c0 + i as isize - 1;
            *slot = read_clamped(band, c, r, w, h).unwrap_or(0.0);
        }
        *row_slot = cubic_interp(&row, dx);
    }
    Some(cubic_interp(&rows, dy))
}

/// Catmull-Rom cubic interpolation over 4 samples.
fn cubic_interp(p: &[f64; 4], t: f64) -> f64 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * p[1])
        + (-p[0] + p[2]) * t
        + (2.0 * p[0] - 5.0 * p[1] + 4.0 * p[2] - p[3]) * t2
        + (-p[0] + 3.0 * p[1] - 3.0 * p[2] + p[3]) * t3)
}

fn sample_lanczos(band: &Band, w: usize, h: usize, x: f64, y: f64, a: i32) -> Option<f64> {
    let xc = x - 0.5;
    let yc = y - 0.5;
    let cf = xc.floor();
    let rf = yc.floor();
    let dx = xc - cf;
    let dy = yc - rf;

    let mut num = 0.0f64;
    let mut den = 0.0f64;
    for j in -(a - 1)..=a {
        let r = rf as isize + j as isize;
        let ly = lanczos_kernel(j as f64 - dy, a);
        for i in -(a - 1)..=a {
            let c = cf as isize + i as isize;
            let lx = lanczos_kernel(i as f64 - dx, a);
            let w_ij = lx * ly;
            let p = read_clamped(band, c, r, w, h).unwrap_or(0.0);
            num += w_ij * p;
            den += w_ij;
        }
    }
    if den.abs() < 1e-12 {
        None
    } else {
        Some(num / den)
    }
}

fn lanczos_kernel(x: f64, a: i32) -> f64 {
    use std::f64::consts::PI;
    if x.abs() < 1e-12 {
        return 1.0;
    }
    let af = a as f64;
    if x.abs() >= af {
        return 0.0;
    }
    let px = PI * x;
    (af * (px).sin() * (px / af).sin()) / (px * px)
}

fn read_clamped(band: &Band, c: isize, r: isize, w: usize, h: usize) -> Option<f64> {
    let cc = c.clamp(0, w as isize - 1) as usize;
    let rr = r.clamp(0, h as isize - 1) as usize;
    pixel::read(band, cc, rr, w)
}

// --- Tile-to-tile resampling ----------------------------------------------

/// Resize a `RasterTile` to new `(target_width, target_height)`. Used by
/// pyramid building and warp output preparation.
pub fn resample_tile(
    source: &RasterTile,
    target_width: u32,
    target_height: u32,
    method: ResamplingMethod,
) -> RasterTile {
    let sw = source.width as f64;
    let sh = source.height as f64;
    let tw = target_width.max(1);
    let th = target_height.max(1);
    let sx = sw / tw as f64;
    let sy = sh / th as f64;

    let mut bands = Vec::with_capacity(source.bands.len());
    for src_band in &source.bands {
        let mut dst_band = Band::new(
            src_band.descriptor.clone(),
            vec![0u8; (tw as usize) * (th as usize) * src_band.descriptor.dtype.size_bytes()],
        );

        // Area-based downsampling needs a different inner loop.
        let is_downsample = sx > 1.0 || sy > 1.0;
        let use_area_kernel =
            matches!(method, ResamplingMethod::Average | ResamplingMethod::Mode) && is_downsample;

        for ty in 0..th as usize {
            for tx in 0..tw as usize {
                let value = if use_area_kernel {
                    let x0 = tx as f64 * sx;
                    let x1 = (tx + 1) as f64 * sx;
                    let y0 = ty as f64 * sy;
                    let y1 = (ty + 1) as f64 * sy;
                    match method {
                        ResamplingMethod::Average => area_average(
                            src_band,
                            source.width as usize,
                            source.height as usize,
                            x0,
                            y0,
                            x1,
                            y1,
                        ),
                        ResamplingMethod::Mode => area_mode(
                            src_band,
                            source.width as usize,
                            source.height as usize,
                            x0,
                            y0,
                            x1,
                            y1,
                        ),
                        _ => unreachable!(),
                    }
                } else {
                    // Point-sample mapped from target pixel centre back to source.
                    let sxp = (tx as f64 + 0.5) * sx;
                    let syp = (ty as f64 + 0.5) * sy;
                    sample(src_band, source.width, source.height, sxp, syp, method)
                };
                if let Some(v) = value {
                    pixel::write(&mut dst_band, tx, ty, tw as usize, v);
                }
            }
        }
        bands.push(dst_band);
    }

    // Adjust geo-transform so the destination still maps to the same world
    // region. Pixel size scales by source/target ratio per axis.
    let mut gt = source.geo_transform;
    gt.pixel_size[0] *= sx;
    gt.pixel_size[1] *= sy;

    RasterTile {
        width: tw,
        height: th,
        bands,
        geo_transform: gt,
        crs: source.crs.clone(),
    }
}

fn area_average(
    band: &Band,
    w: usize,
    h: usize,
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
) -> Option<f64> {
    let c0 = (x0.floor() as isize).max(0);
    let c1 = ((x1.ceil() as isize) - 1).min(w as isize - 1);
    let r0 = (y0.floor() as isize).max(0);
    let r1 = ((y1.ceil() as isize) - 1).min(h as isize - 1);
    if c1 < c0 || r1 < r0 {
        return None;
    }
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for r in r0..=r1 {
        for c in c0..=c1 {
            if let Some(v) = pixel::read(band, c as usize, r as usize, w) {
                sum += v;
                count += 1;
            }
        }
    }
    if count == 0 {
        None
    } else {
        Some(sum / count as f64)
    }
}

fn area_mode(band: &Band, w: usize, h: usize, x0: f64, y0: f64, x1: f64, y1: f64) -> Option<f64> {
    use std::collections::HashMap;
    let c0 = (x0.floor() as isize).max(0);
    let c1 = ((x1.ceil() as isize) - 1).min(w as isize - 1);
    let r0 = (y0.floor() as isize).max(0);
    let r1 = ((y1.ceil() as isize) - 1).min(h as isize - 1);
    if c1 < c0 || r1 < r0 {
        return None;
    }
    let mut counts: HashMap<u64, u32> = HashMap::new();
    for r in r0..=r1 {
        for c in c0..=c1 {
            if let Some(v) = pixel::read(band, c as usize, r as usize, w) {
                // Bucket by f64 bit pattern — categorical / integer values
                // round-trip exactly through f64.
                *counts.entry(v.to_bits()).or_insert(0) += 1;
            }
        }
    }
    counts
        .into_iter()
        .max_by_key(|&(_, c)| c)
        .map(|(bits, _)| f64::from_bits(bits))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geonative_core::raster::{BandDescriptor, GeoTransform, PixelType};
    use geonative_core::Crs;

    fn u8_tile(width: u32, height: u32, fill: u8) -> RasterTile {
        let pixels = (width as usize) * (height as usize);
        RasterTile {
            width,
            height,
            bands: vec![Band::new(
                BandDescriptor::new(Some("v".into()), PixelType::U8),
                vec![fill; pixels],
            )],
            geo_transform: GeoTransform::north_up(0.0, 0.0, 1.0, 1.0),
            crs: Crs::Epsg(3857),
        }
    }

    /// A 4×4 tile with a horizontal gradient 0..3 across columns.
    fn gradient_tile() -> RasterTile {
        let mut data = Vec::with_capacity(16);
        for _r in 0..4u8 {
            for c in 0..4u8 {
                data.push(c * 80); // 0, 80, 160, 240 across the row
            }
        }
        RasterTile {
            width: 4,
            height: 4,
            bands: vec![Band::new(
                BandDescriptor::new(Some("v".into()), PixelType::U8),
                data,
            )],
            geo_transform: GeoTransform::north_up(0.0, 0.0, 1.0, 1.0),
            crs: Crs::Epsg(3857),
        }
    }

    #[test]
    fn nearest_returns_exact_pixel_value() {
        let t = gradient_tile();
        // Centre of pixel (2, 1) — should be 160.
        let v = sample(&t.bands[0], 4, 4, 2.5, 1.5, ResamplingMethod::Nearest).unwrap();
        assert_eq!(v, 160.0);
    }

    #[test]
    fn bilinear_interpolates_between_pixels() {
        let t = gradient_tile();
        // Halfway between centres of pixels (0, 0) and (1, 0) — gradient is 0 → 80.
        let v = sample(&t.bands[0], 4, 4, 1.0, 0.5, ResamplingMethod::Bilinear).unwrap();
        assert!((v - 40.0).abs() < 1e-9);
    }

    #[test]
    fn cubic_handles_smooth_gradient() {
        let t = gradient_tile();
        // Catmull-Rom cubic produces ~35 at the midpoint of a step gradient
        // (sharper curve than bilinear, deliberately so). Just verify the
        // sample is finite + in the expected ballpark.
        let v = sample(&t.bands[0], 4, 4, 1.0, 0.5, ResamplingMethod::Cubic).unwrap();
        assert!(v.is_finite());
        assert!((20.0..=60.0).contains(&v), "cubic got {v}, expected 20..60");
    }

    #[test]
    fn out_of_bounds_returns_none() {
        let t = gradient_tile();
        assert!(sample(&t.bands[0], 4, 4, -1.0, 0.0, ResamplingMethod::Nearest).is_none());
        assert!(sample(&t.bands[0], 4, 4, 0.0, 5.0, ResamplingMethod::Nearest).is_none());
    }

    #[test]
    fn resample_tile_upsamples() {
        let t = gradient_tile();
        let resized = resample_tile(&t, 8, 8, ResamplingMethod::Bilinear);
        assert_eq!(resized.width, 8);
        assert_eq!(resized.height, 8);
        // Pixel size halved
        assert_eq!(resized.geo_transform.pixel_size, [0.5, -0.5]);
    }

    #[test]
    fn resample_tile_downsamples_average() {
        // 4×4 with gradient → 2×2 with average; first 2×2 averages to (0+80)/2=40
        // but across both rows of the gradient it's still 40, so result(0,0) = 40.
        let t = gradient_tile();
        let resized = resample_tile(&t, 2, 2, ResamplingMethod::Average);
        assert_eq!(resized.width, 2);
        assert_eq!(resized.height, 2);
        // (0, 0) covers source pixels (0..2, 0..2) — values 0, 80 horizontally,
        // averaged over 4 pixels → (0 + 80 + 0 + 80) / 4 = 40.
        let v = resized.bands[0].data[0];
        assert!((v as i32 - 40).abs() < 2);
    }

    #[test]
    fn resample_tile_downsamples_mode() {
        // 4×4 with a dominant value of 5, plus one different pixel
        let mut t = u8_tile(4, 4, 5);
        t.bands[0].data[0] = 99;
        let resized = resample_tile(&t, 2, 2, ResamplingMethod::Mode);
        assert_eq!(resized.width, 2);
        // Mode of the top-left 2×2 (values: 99, 5, 5, 5) → 5
        assert_eq!(resized.bands[0].data[0], 5);
    }

    #[test]
    fn lanczos_kernel_at_zero_is_one() {
        assert!((lanczos_kernel(0.0, 3) - 1.0).abs() < 1e-12);
        // At integer offsets ≥ a, kernel is zero
        assert_eq!(lanczos_kernel(3.0, 3), 0.0);
        assert_eq!(lanczos_kernel(-3.0, 3), 0.0);
    }
}
