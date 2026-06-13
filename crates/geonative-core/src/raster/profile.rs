//! [`RasterProfile`] — the schema-analog for a raster layer.
//!
//! A profile describes a raster's **structure** without carrying any pixel
//! data. It's the parallel of [`Schema`](crate::Schema): one per layer,
//! cheap to clone, embedded in the `RasterLayer` trait so callers can
//! describe an output before any tiles exist.

use crate::Crs;

use super::{BandDescriptor, GeoTransform};

/// Describes the structure of a raster layer: dimensions, bands, geo-
/// referencing, CRS. Does NOT carry pixel data — that flows through
/// [`RasterTile`](super::RasterTile)s on read.
#[derive(Debug, Clone)]
pub struct RasterProfile {
    /// Full-resolution width in pixels.
    pub width: u32,
    /// Full-resolution height in pixels.
    pub height: u32,
    /// One [`BandDescriptor`] per band; matches the order of
    /// `RasterTile::bands`.
    pub bands: Vec<BandDescriptor>,
    /// Pixel → world mapping for the **full-resolution** image. Tiles
    /// computed at lower pyramid levels derive their own (coarser)
    /// `GeoTransform` from this one.
    pub geo_transform: GeoTransform,
    /// CRS of the world coordinates.
    pub crs: Crs,
    /// Tile size in pixels (typically 256 or 512). Set by the format
    /// reader; influences how callers should issue `read_tile` requests.
    pub tile_size: [u32; 2],
    /// Number of pyramid levels available, including the full-resolution
    /// level 0. A COG with internal overviews down to 256×256 thumbnail
    /// over a 32 768×32 768 source has 8 levels.
    pub pyramid_levels: u8,
}

impl RasterProfile {
    /// Tiles needed to cover the full-resolution image at level 0.
    pub fn tile_grid_dimensions(&self) -> [u32; 2] {
        let tw = self.tile_size[0].max(1);
        let th = self.tile_size[1].max(1);
        // Hand-rolled ceil-div (not u32::div_ceil) because our MSRV is
        // 1.74 and div_ceil stabilised in 1.85.
        [
            self.width.saturating_add(tw - 1) / tw,
            self.height.saturating_add(th - 1) / th,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::PixelType;

    fn rgb_profile(w: u32, h: u32) -> RasterProfile {
        RasterProfile {
            width: w,
            height: h,
            bands: vec![
                BandDescriptor::new(Some("red".into()), PixelType::U8),
                BandDescriptor::new(Some("green".into()), PixelType::U8),
                BandDescriptor::new(Some("blue".into()), PixelType::U8),
            ],
            geo_transform: GeoTransform::north_up(0.0, 0.0, 1.0, 1.0),
            crs: Crs::Epsg(3857),
            tile_size: [256, 256],
            pyramid_levels: 1,
        }
    }

    #[test]
    fn tile_grid_for_aligned_image() {
        let p = rgb_profile(1024, 1024);
        assert_eq!(p.tile_grid_dimensions(), [4, 4]);
    }

    #[test]
    fn tile_grid_for_unaligned_image() {
        // 1000×1000 with 256-tile size → 4×4 grid (last tile partial)
        let p = rgb_profile(1000, 1000);
        assert_eq!(p.tile_grid_dimensions(), [4, 4]);
    }

    #[test]
    fn tile_grid_for_tiny_image() {
        let p = rgb_profile(50, 50);
        assert_eq!(p.tile_grid_dimensions(), [1, 1]);
    }
}
