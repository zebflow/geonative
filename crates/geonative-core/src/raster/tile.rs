//! [`RasterTile`] — the row-analog of [`Feature`](crate::Feature) for the
//! raster IR. One chunk of pixels with dimensions, dtype, bands,
//! geo-referencing, and CRS.
//!
//! ## Sizing
//!
//! Tiles are typically 256×256 or 512×512 px in the COG / web-map world,
//! but the type doesn't enforce a fixed size — a "tile" can be anything
//! from a single pixel to a full-image read of a small raster.
//!
//! ## Memory ownership
//!
//! `RasterTile` owns its pixel bytes (via `Band::data: Vec<u8>`). A
//! 256×256×3-band U8 tile is ~196 KB; an F32 DEM tile is ~262 KB. Cheap
//! to materialise per-request; cheap to drop. mmap-backed readers can
//! either copy into a new `Vec<u8>` per tile or expose `RasterTileBorrow`
//! (a future zero-copy variant for hot serving paths).

use crate::Crs;

use super::{Band, GeoTransform};

/// One chunk of raster data.
#[derive(Debug, Clone, PartialEq)]
pub struct RasterTile {
    /// Pixels across (column count).
    pub width: u32,
    /// Pixels down (row count).
    pub height: u32,
    /// One per band; all bands share `width × height` dimensions.
    pub bands: Vec<Band>,
    /// Pixel → world mapping for **this tile** (not the source layer). A
    /// 256×256 tile from the upper-left of a larger image has its own
    /// `GeoTransform` with the right `origin` for the tile, not the
    /// source's origin.
    pub geo_transform: GeoTransform,
    /// CRS — same `Crs` type used everywhere else in geonative.
    pub crs: Crs,
}

impl RasterTile {
    /// `width × height × sum(bands.dtype.size_bytes())`. Useful for
    /// budgeting memory before reading large tiles.
    pub fn byte_size(&self) -> usize {
        let pixels = (self.width as usize) * (self.height as usize);
        self.bands
            .iter()
            .map(|b| pixels * b.descriptor.dtype.size_bytes())
            .sum()
    }

    /// World-space bounding box of the tile.
    pub fn bounds(&self) -> [f64; 4] {
        self.geo_transform.world_bounds(self.width, self.height)
    }

    /// `true` if every band has data sized correctly for the declared
    /// dimensions. Format readers should call this before handing the
    /// tile to downstream code; it catches "I forgot to read enough
    /// bytes" bugs immediately.
    pub fn is_well_formed(&self) -> bool {
        let pixels = (self.width as usize) * (self.height as usize);
        self.bands.iter().all(|b| {
            let bpp = b.descriptor.dtype.size_bytes();
            bpp > 0 && b.data.len() == pixels * bpp
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::{BandDescriptor, PixelType};

    fn tile_256x256_rgb() -> RasterTile {
        let make_band = |name: &str| {
            Band::new(
                BandDescriptor::new(Some(name.into()), PixelType::U8),
                vec![0u8; 256 * 256],
            )
        };
        RasterTile {
            width: 256,
            height: 256,
            bands: vec![make_band("red"), make_band("green"), make_band("blue")],
            geo_transform: GeoTransform::north_up(0.0, 0.0, 1.0, 1.0),
            crs: Crs::Epsg(3857),
        }
    }

    #[test]
    fn byte_size_for_rgb_tile() {
        let t = tile_256x256_rgb();
        // 256 * 256 px * 3 bands * 1 byte/px = 196_608
        assert_eq!(t.byte_size(), 196_608);
    }

    #[test]
    fn well_formed_round_trip() {
        let t = tile_256x256_rgb();
        assert!(t.is_well_formed());
    }

    #[test]
    fn malformed_band_detected() {
        let mut t = tile_256x256_rgb();
        t.bands[0].data.truncate(100); // missing data
        assert!(!t.is_well_formed());
    }

    #[test]
    fn bounds_from_geo_transform() {
        let t = tile_256x256_rgb();
        let b = t.bounds();
        // pixel size 1.0, origin (0,0), 256 wide → x spans 0..256
        assert_eq!(b[0], 0.0);
        assert_eq!(b[2], 256.0);
    }
}
