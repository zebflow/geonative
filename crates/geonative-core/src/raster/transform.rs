//! Affine geo-referencing — the mapping from pixel coordinates to
//! world coordinates.
//!
//! ## The GDAL convention
//!
//! GDAL canonised a 6-number representation that every tool now uses:
//! ```text
//! world_x = origin_x + col * pixel_w + row * rot_x
//! world_y = origin_y + col * rot_y   + row * pixel_h
//! ```
//! For the common "north-up, no rotation" case, `rot_x` and `rot_y` are
//! both `0`, `pixel_w` is positive, and `pixel_h` is **negative** (the
//! image's Y axis runs top-down while world Y runs bottom-up).
//!
//! ## World files
//!
//! `.tfw` / `.jgw` / `.pgw` "world file" sidecars are this same six-tuple
//! in a fixed order:
//! ```text
//! pixel_w
//! rot_y
//! rot_x
//! pixel_h        (typically negative)
//! origin_x       (centre of pixel (0, 0), per the world-file convention)
//! origin_y       (centre of pixel (0, 0))
//! ```
//! Our [`GeoTransform::origin`] follows the GDAL convention — **upper-left
//! corner** of pixel (0, 0), not its centre. The world-file reader applies
//! the half-pixel shift on parse.

use crate::Coord;

/// Pixel → world affine mapping. Six numbers + a unit semantic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoTransform {
    /// World coordinates of the upper-left corner of pixel (0, 0).
    pub origin: [f64; 2],
    /// `[pixel_width, pixel_height]` in world units. Note `pixel_height`
    /// is typically **negative** for north-up images.
    pub pixel_size: [f64; 2],
    /// `[rotation_x, rotation_y]`. Both zero for the universal north-up
    /// case. Non-zero values describe rotated or skewed rasters (rare).
    pub rotation: [f64; 2],
}

impl GeoTransform {
    /// Construct a north-up, no-rotation transform — the 99% case. Pass
    /// the upper-left corner's world coords plus the pixel size; we set
    /// `pixel_height` negative for you.
    pub fn north_up(origin_x: f64, origin_y: f64, pixel_width: f64, pixel_height: f64) -> Self {
        // Not a `const fn` because `f64::abs` only became const in 1.85
        // (our MSRV is 1.74). Trivial overhead.
        Self {
            origin: [origin_x, origin_y],
            pixel_size: [pixel_width, -pixel_height.abs()],
            rotation: [0.0, 0.0],
        }
    }

    /// Construct from the GDAL 6-tuple `[ox, pw, rx, oy, ry, ph]`. Use
    /// this when parsing TIFF tags (`ModelTiepointTag` + `ModelPixelScale`
    /// pair) or world-file ASCII.
    pub const fn from_gdal_tuple(t: [f64; 6]) -> Self {
        Self {
            origin: [t[0], t[3]],
            pixel_size: [t[1], t[5]],
            rotation: [t[2], t[4]],
        }
    }

    /// Inverse: `[ox, pw, rx, oy, ry, ph]`.
    pub const fn to_gdal_tuple(self) -> [f64; 6] {
        [
            self.origin[0],
            self.pixel_size[0],
            self.rotation[0],
            self.origin[1],
            self.rotation[1],
            self.pixel_size[1],
        ]
    }

    /// Map a (column, row) pixel coordinate to world space.
    pub fn pixel_to_world(self, col: f64, row: f64) -> Coord {
        Coord::xy(
            self.origin[0] + col * self.pixel_size[0] + row * self.rotation[0],
            self.origin[1] + col * self.rotation[1] + row * self.pixel_size[1],
        )
    }

    /// Map a world (x, y) to (column, row) — the inverse transform. Only
    /// exact when the rotation terms are zero; the closed-form 2×2 inverse
    /// covers the rotated case too.
    pub fn world_to_pixel(self, world: Coord) -> [f64; 2] {
        let dx = world.x - self.origin[0];
        let dy = world.y - self.origin[1];
        let a = self.pixel_size[0];
        let b = self.rotation[0];
        let c = self.rotation[1];
        let d = self.pixel_size[1];
        // 2×2 inverse: 1/det * [d, -b; -c, a]
        let det = a * d - b * c;
        let inv_det = 1.0 / det;
        [(d * dx - b * dy) * inv_det, (-c * dx + a * dy) * inv_det]
    }

    /// World-space bounding box of an image with the given pixel
    /// dimensions, as `[xmin, ymin, xmax, ymax]`. Handles negative
    /// `pixel_height` (north-up) by computing all four corners.
    pub fn world_bounds(self, width: u32, height: u32) -> [f64; 4] {
        let w = width as f64;
        let h = height as f64;
        let c0 = self.pixel_to_world(0.0, 0.0);
        let c1 = self.pixel_to_world(w, 0.0);
        let c2 = self.pixel_to_world(0.0, h);
        let c3 = self.pixel_to_world(w, h);
        let xs = [c0.x, c1.x, c2.x, c3.x];
        let ys = [c0.y, c1.y, c2.y, c3.y];
        let xmin = xs.iter().cloned().fold(f64::INFINITY, f64::min);
        let xmax = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let ymin = ys.iter().cloned().fold(f64::INFINITY, f64::min);
        let ymax = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        [xmin, ymin, xmax, ymax]
    }

    /// `true` if rotation terms are both zero — the simple "north-up" case
    /// most viewers handle natively.
    pub fn is_north_up(self) -> bool {
        self.rotation[0] == 0.0 && self.rotation[1] == 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn north_up_builder_negates_pixel_height() {
        let gt = GeoTransform::north_up(100.0, 200.0, 0.5, 0.5);
        assert_eq!(gt.pixel_size[0], 0.5);
        assert_eq!(gt.pixel_size[1], -0.5);
        assert_eq!(gt.rotation, [0.0, 0.0]);
    }

    #[test]
    fn pixel_to_world_origin_is_origin() {
        let gt = GeoTransform::north_up(100.0, 200.0, 0.5, 0.5);
        let p = gt.pixel_to_world(0.0, 0.0);
        assert_eq!(p.x, 100.0);
        assert_eq!(p.y, 200.0);
    }

    #[test]
    fn round_trip_pixel_world() {
        let gt = GeoTransform::north_up(144.0, -37.0, 0.001, 0.001);
        let world = gt.pixel_to_world(123.0, 456.0);
        let back = gt.world_to_pixel(world);
        assert!(approx_eq(back[0], 123.0, 1e-9));
        assert!(approx_eq(back[1], 456.0, 1e-9));
    }

    #[test]
    fn bounds_of_north_up_image() {
        // 100x50 px at origin (10, 20), pixel size 0.1 → spans 10 wide, 5 tall
        let gt = GeoTransform::north_up(10.0, 20.0, 0.1, 0.1);
        let b = gt.world_bounds(100, 50);
        assert!(approx_eq(b[0], 10.0, 1e-9));
        assert!(approx_eq(b[2], 20.0, 1e-9));
        // Y axis: origin is at top (20.0), descends to 20 - 5 = 15
        assert!(approx_eq(b[3], 20.0, 1e-9));
        assert!(approx_eq(b[1], 15.0, 1e-9));
    }

    #[test]
    fn gdal_tuple_round_trip() {
        let gt = GeoTransform {
            origin: [144.0, -37.0],
            pixel_size: [0.5, -0.5],
            rotation: [0.001, -0.001],
        };
        let t = gt.to_gdal_tuple();
        let back = GeoTransform::from_gdal_tuple(t);
        assert_eq!(back, gt);
    }

    #[test]
    fn is_north_up_detection() {
        assert!(GeoTransform::north_up(0.0, 0.0, 1.0, 1.0).is_north_up());
        let rotated = GeoTransform::from_gdal_tuple([0.0, 1.0, 0.1, 0.0, 0.0, -1.0]);
        assert!(!rotated.is_north_up());
    }
}
