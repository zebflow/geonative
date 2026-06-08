//! # geonative-tile
//!
//! Slippy-map tile coordinate math — the small pile of arithmetic that turns
//! a (z, x, y) tile index into a lng/lat bbox, projects geographic
//! coordinates into a tile's local integer pixel grid, and groups tiles
//! into metatiles for efficient batch rendering.
//!
//! ## Why this crate exists
//!
//! Every spatial system that serves "tiles" (MVT vector tiles, WMS rendered
//! images, WMTS, raster XYZ tiles) does the same coordinate dance: take a
//! `(z, x, y)` index, figure out which lng/lat range it covers, then project
//! features inside that range into a fixed integer pixel grid (4096 for MVT,
//! 256/512 for raster). This crate is that math, factored out so MVT,
//! WMS, and custom raster pipelines can all consume the same primitives.
//!
//! ## What's in it
//!
//! - [`TileCoord`] — a `(z, x, y)` slippy-map tile index
//! - [`LngLat`] — a longitude/latitude pair (degrees, WGS84)
//! - [`TileCoord::bbox`] — the lng/lat envelope of a tile
//! - [`TileCoord::project_lnglat`] — project a lng/lat into the tile's
//!   integer pixel grid (`0..extent`)
//! - [`Metatile`] — a `(z, x, y, grid_size)` super-tile that bundles
//!   `grid_size × grid_size` ordinary tiles for batched rendering
//!
//! ## Clever bits
//!
//! - **Web Mercator (EPSG:3857) only.** Slippy-map tiles are universally
//!   served in Web Mercator; supporting other tile pyramids (e.g. 4326
//!   plate-carrée) is a separate concern and would live in a future
//!   `geonative-tile-4326` crate or feature flag.
//! - **Tile axis Y is top-to-bottom**, lng/lat axis Y is bottom-to-top — the
//!   conversion in [`TileCoord::project_lnglat`] flips Y so the resulting
//!   pixel coords match the conventional image origin (top-left = 0,0).
//! - **No clamping at the projection step.** If a feature pokes out of the
//!   tile envelope, the projected pixel coords go negative or beyond
//!   `extent`. Consumers can clip explicitly; MVT readers tolerate small
//!   over-runs by spec.
//! - **`Metatile`'s `sub_tile_bbox`** is in *pixel* coordinates within the
//!   metatile's combined raster — used by zebflow-style code that renders
//!   one large pixmap and slices it into individual tile PNGs.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

use std::f64::consts::PI;

/// A slippy-map tile index. `z` is the zoom level (0 = whole world in 1
/// tile, 20 ≈ ~38 cm/pixel at the equator); `x` runs west→east, `y` runs
/// **north→south**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileCoord {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

/// A longitude/latitude pair in WGS84 (degrees).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LngLat {
    pub lng: f64,
    pub lat: f64,
}

impl LngLat {
    pub const fn new(lng: f64, lat: f64) -> Self {
        Self { lng, lat }
    }
}

/// Maximum absolute latitude representable in Web Mercator. Above ~85.05°
/// the projection diverges. Slippy maps universally clamp here.
pub const WEB_MERCATOR_MAX_LATITUDE: f64 = 85.051_128_779_806_59;

impl TileCoord {
    pub const fn new(z: u8, x: u32, y: u32) -> Self {
        Self { z, x, y }
    }

    /// Number of tiles per side at this zoom (`2^z`).
    pub fn tiles_per_side(self) -> u32 {
        1u32 << self.z
    }

    /// The lng/lat bounding box of this tile as `[lng_min, lat_min, lng_max, lat_max]`.
    pub fn bbox(self) -> [f64; 4] {
        let n = self.tiles_per_side() as f64;
        let lng_min = (self.x as f64) / n * 360.0 - 180.0;
        let lng_max = ((self.x + 1) as f64) / n * 360.0 - 180.0;
        // Inverse Mercator: lat = arctan(sinh(π * (1 - 2y/n))) in radians, then to degrees.
        let lat_max = mercator_y_to_lat(1.0 - 2.0 * (self.y as f64) / n);
        let lat_min = mercator_y_to_lat(1.0 - 2.0 * ((self.y + 1) as f64) / n);
        [lng_min, lat_min, lng_max, lat_max]
    }

    /// Project a `LngLat` into this tile's integer pixel grid of side
    /// `extent` (4096 for MVT, 256 for raster slippy tiles).
    ///
    /// Returns `(px, py)` where (0,0) is the **top-left** corner of the tile
    /// and (`extent`, `extent`) is the **bottom-right**. Values can fall
    /// outside `[0, extent]` if the input lies outside the tile.
    pub fn project_lnglat(self, p: LngLat, extent: u32) -> (i32, i32) {
        let n = self.tiles_per_side() as f64;
        // World-pixel coords at this zoom, with the tile-origin in tiles.
        let world_x = (p.lng + 180.0) / 360.0 * n;
        let world_y = (1.0 - lat_to_mercator_y_norm(p.lat)) * 0.5 * n;
        // Subtract tile origin → tile-local in [0, 1] (roughly), then scale.
        let px = (world_x - self.x as f64) * extent as f64;
        let py = (world_y - self.y as f64) * extent as f64;
        (px.round() as i32, py.round() as i32)
    }
}

/// Convert a normalized Mercator Y in `[-1, 1]` (north positive) to latitude
/// in degrees.
fn mercator_y_to_lat(merc_y: f64) -> f64 {
    let lat_rad = (PI * merc_y).sinh().atan();
    lat_rad.to_degrees()
}

/// Convert a latitude in degrees to a normalized Mercator Y in `[-1, 1]`.
/// Output > 0 is northern hemisphere.
fn lat_to_mercator_y_norm(lat: f64) -> f64 {
    let lat = lat.clamp(-WEB_MERCATOR_MAX_LATITUDE, WEB_MERCATOR_MAX_LATITUDE);
    let r = lat.to_radians();
    (r.tan() + 1.0 / r.cos()).ln() / PI
}

/// A super-tile that bundles `grid_size × grid_size` ordinary tiles, all at
/// the same zoom level. Used by zebflow-style rendering pipelines that draw
/// one large pixmap then slice into per-tile PNGs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Metatile {
    /// Zoom level shared by all sub-tiles.
    pub z: u8,
    /// Top-left sub-tile X.
    pub x: u32,
    /// Top-left sub-tile Y.
    pub y: u32,
    /// Side length in tiles (typical: 4 → 16 sub-tiles).
    pub grid_size: u32,
}

impl Metatile {
    pub const fn new(z: u8, x: u32, y: u32, grid_size: u32) -> Self {
        Self { z, x, y, grid_size }
    }

    /// The deterministic metatile that contains the given individual tile.
    /// `grid_size` must be a power of two (4 is the common choice).
    pub fn containing(tile: TileCoord, grid_size: u32) -> Self {
        let mx = (tile.x / grid_size) * grid_size;
        let my = (tile.y / grid_size) * grid_size;
        Self {
            z: tile.z,
            x: mx,
            y: my,
            grid_size,
        }
    }

    /// Number of sub-tiles in this metatile (`grid_size × grid_size`).
    pub fn count(self) -> u32 {
        self.grid_size * self.grid_size
    }

    /// Iterate the sub-tiles in row-major order (top-to-bottom, left-to-right).
    pub fn sub_tiles(self) -> impl Iterator<Item = TileCoord> {
        let z = self.z;
        let x0 = self.x;
        let y0 = self.y;
        let gs = self.grid_size;
        (0..gs).flat_map(move |dy| (0..gs).map(move |dx| TileCoord::new(z, x0 + dx, y0 + dy)))
    }

    /// Lng/lat bbox of the entire metatile.
    pub fn bbox(self) -> [f64; 4] {
        let tl = TileCoord::new(self.z, self.x, self.y).bbox();
        let br = TileCoord::new(
            self.z,
            self.x + self.grid_size - 1,
            self.y + self.grid_size - 1,
        )
        .bbox();
        [tl[0], br[1], br[2], tl[3]]
    }

    /// Where within the metatile's combined pixel raster does the sub-tile
    /// at offset `(dx, dy)` from the top-left start?
    ///
    /// Returns `(px, py, size)` in pixels — useful for image-cropping the
    /// combined render into individual sub-tile PNGs.
    pub fn sub_tile_pixel_rect(self, dx: u32, dy: u32, tile_extent: u32) -> (u32, u32, u32) {
        debug_assert!(dx < self.grid_size && dy < self.grid_size);
        (dx * tile_extent, dy * tile_extent, tile_extent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn z0_covers_world() {
        let t = TileCoord::new(0, 0, 0);
        let bb = t.bbox();
        assert!((bb[0] - -180.0).abs() < 1e-9);
        assert!((bb[2] - 180.0).abs() < 1e-9);
        // Web Mercator latitude limit
        assert!((bb[3] - WEB_MERCATOR_MAX_LATITUDE).abs() < 1e-6);
        assert!((bb[1] + WEB_MERCATOR_MAX_LATITUDE).abs() < 1e-6);
    }

    #[test]
    fn z1_quartiles() {
        // At zoom 1 there are 4 tiles. Tile (0,0) is the NW quadrant.
        let nw = TileCoord::new(1, 0, 0).bbox();
        assert!((nw[0] - -180.0).abs() < 1e-9);
        assert!((nw[2] - 0.0).abs() < 1e-9);
        assert!(nw[3] > 0.0); // northern hemisphere
        let se = TileCoord::new(1, 1, 1).bbox();
        assert!((se[0] - 0.0).abs() < 1e-9);
        assert!((se[2] - 180.0).abs() < 1e-9);
        assert!(se[1] < 0.0); // southern hemisphere
    }

    #[test]
    fn project_x_is_linear_in_longitude() {
        // X mapping IS linear in lng (Web Mercator preserves meridians).
        // Quarter-lng through the tile → quarter-extent pixel X.
        let t = TileCoord::new(2, 1, 1);
        let bb = t.bbox();
        let lng_quarter = bb[0] + (bb[2] - bb[0]) * 0.25;
        let (px, _) = t.project_lnglat(LngLat::new(lng_quarter, bb[3]), 4096);
        assert_eq!(px, 1024);
    }

    #[test]
    fn project_y_is_monotonic_in_latitude() {
        // Lat decreasing → py increasing (Y axis flipped). Confirm
        // monotonicity even if the exact midpoint pixel isn't at bbox-mid-lat
        // (Mercator is non-linear in lat).
        let t = TileCoord::new(3, 4, 3);
        let bb = t.bbox();
        let lng = (bb[0] + bb[2]) * 0.5;
        let (_, py_top) = t.project_lnglat(LngLat::new(lng, bb[3]), 4096);
        let (_, py_mid) = t.project_lnglat(LngLat::new(lng, (bb[1] + bb[3]) * 0.5), 4096);
        let (_, py_bot) = t.project_lnglat(LngLat::new(lng, bb[1]), 4096);
        assert_eq!(py_top, 0);
        assert!(py_mid > py_top && py_mid < py_bot);
        assert_eq!(py_bot, 4096);
    }

    #[test]
    fn project_top_left_corner_is_zero() {
        let t = TileCoord::new(3, 2, 3);
        let bb = t.bbox();
        // Tile top-left in lng/lat is (bb[0], bb[3]).
        let (px, py) = t.project_lnglat(LngLat::new(bb[0], bb[3]), 4096);
        assert_eq!(px, 0);
        assert_eq!(py, 0);
    }

    #[test]
    fn project_bottom_right_corner_is_extent() {
        let t = TileCoord::new(3, 2, 3);
        let bb = t.bbox();
        let (px, py) = t.project_lnglat(LngLat::new(bb[2], bb[1]), 4096);
        assert_eq!(px, 4096);
        assert_eq!(py, 4096);
    }

    #[test]
    fn project_outside_tile_goes_outside_extent() {
        let t = TileCoord::new(3, 2, 3);
        // A point far north of the tile's lat range → negative py.
        let (_, py) = t.project_lnglat(LngLat::new(0.0, 80.0), 4096);
        // Could be negative or > extent depending on tile. Just check it's
        // outside the canonical [0, 4096] range.
        assert!(!(0..=4096).contains(&py), "py {py} should be outside tile");
    }

    #[test]
    fn metatile_containing_aligns_to_grid() {
        let m = Metatile::containing(TileCoord::new(10, 7, 11), 4);
        assert_eq!(m.x, 4); // floor(7/4)*4 = 4
        assert_eq!(m.y, 8); // floor(11/4)*4 = 8
        assert_eq!(m.grid_size, 4);
        assert_eq!(m.z, 10);
    }

    #[test]
    fn metatile_sub_tiles_are_row_major() {
        let m = Metatile::new(5, 10, 20, 2);
        let v: Vec<TileCoord> = m.sub_tiles().collect();
        assert_eq!(v.len(), 4);
        assert_eq!(v[0], TileCoord::new(5, 10, 20));
        assert_eq!(v[1], TileCoord::new(5, 11, 20));
        assert_eq!(v[2], TileCoord::new(5, 10, 21));
        assert_eq!(v[3], TileCoord::new(5, 11, 21));
    }

    #[test]
    fn metatile_pixel_rect() {
        let m = Metatile::new(5, 10, 20, 4);
        // dx=2, dy=1, extent=256 → (512, 256, 256)
        assert_eq!(m.sub_tile_pixel_rect(2, 1, 256), (512, 256, 256));
    }

    #[test]
    fn metatile_bbox_envelopes_all_sub_tiles() {
        let m = Metatile::new(3, 0, 0, 2);
        let mbb = m.bbox();
        for st in m.sub_tiles() {
            let bb = st.bbox();
            assert!(bb[0] >= mbb[0] - 1e-9);
            assert!(bb[1] >= mbb[1] - 1e-9);
            assert!(bb[2] <= mbb[2] + 1e-9);
            assert!(bb[3] <= mbb[3] + 1e-9);
        }
    }
}
