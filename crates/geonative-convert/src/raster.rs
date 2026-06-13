//! Raster pipeline — the mirror of `Source` / `Sink` / `convert` for raster
//! data. Reuses `core::raster::*` IR + the format-polymorphic dispatch
//! pattern already proven on the vector side.
//!
//! ## Usage
//!
//! ```no_run
//! use geonative_convert::raster::{RasterSource, RasterSink, RasterSinkOptions};
//!
//! let src = RasterSource::open(std::path::Path::new("upload.tif"))?;
//! let profile = src.profile_cloned();
//! let mut sink = RasterSink::create(
//!     std::path::Path::new("normalized.cog"),
//!     &profile,
//!     RasterSinkOptions::default(),
//! )?;
//! src.for_each_tile(|level, x, y, tile| sink.write_tile(level, x, y, &tile))?;
//! sink.close()?;
//! # Ok::<(), geonative_convert::ConvertError>(())
//! ```

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use geonative_core::raster::{RasterLayer, RasterProfile, RasterTile};
use geonative_geotiff::{Compression, GeoTiff, GeoTiffWriter, WriterOptions};
use geonative_image::ImageRaster;

use crate::error::{ConvertError, Result};
use crate::io::{Format, Modality};

/// An opened raster input. Mirrors [`crate::Source`] for vector.
pub enum RasterSource {
    /// GeoTIFF (regular tiled, or COG). Wrapped in `Arc` so a single open
    /// file can be shared across worker threads in tile servers.
    GeoTiff(Arc<GeoTiff>),
    /// JPEG with a `.jgw` world file. Single-tile, in-memory.
    Jpeg(Arc<ImageRaster>),
    /// PNG with a `.pgw` world file. Single-tile, in-memory.
    Png(Arc<ImageRaster>),
}

impl std::fmt::Debug for RasterSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GeoTiff(_) => f.write_str("RasterSource::GeoTiff"),
            Self::Jpeg(_) => f.write_str("RasterSource::Jpeg"),
            Self::Png(_) => f.write_str("RasterSource::Png"),
        }
    }
}

impl RasterSource {
    /// Open `path`. Dispatches on extension; only raster formats are
    /// accepted (vector formats route to `Source` instead).
    ///
    /// For JPG/PNG inputs, this defaults to `Crs::Unknown`. Use
    /// [`Self::open_with_crs`] to provide one (the typical pattern when
    /// the upload form lets users pick a CRS, or pre-converted data has
    /// a known CRS).
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_crs(path, geonative_core::Crs::Unknown)
    }

    /// Same as [`Self::open`] but with an explicit CRS that's only used
    /// for image+sidecar inputs (GeoTIFF carries its own CRS in the file).
    pub fn open_with_crs(path: &Path, image_crs: geonative_core::Crs) -> Result<Self> {
        match Format::from_path(path)? {
            Format::GeoTiff => Ok(RasterSource::GeoTiff(Arc::new(GeoTiff::open(path)?))),
            Format::Jpeg => Ok(RasterSource::Jpeg(Arc::new(ImageRaster::open_with_crs(
                path, image_crs,
            )?))),
            Format::Png => Ok(RasterSource::Png(Arc::new(ImageRaster::open_with_crs(
                path, image_crs,
            )?))),
            other if other.modality() == Modality::Vector => Err(ConvertError::invalid(format!(
                "{} is a vector format; use Source::open instead",
                other.label()
            ))),
            other => Err(ConvertError::UnsupportedFormat {
                ext: other.label().to_string(),
                path: path.display().to_string(),
                supported: ".tif, .tiff, .cog, .jpg, .png",
            }),
        }
    }

    pub fn format(&self) -> Format {
        match self {
            Self::GeoTiff(_) => Format::GeoTiff,
            Self::Jpeg(_) => Format::Jpeg,
            Self::Png(_) => Format::Png,
        }
    }

    /// Return an owned profile. Cheap to clone; the underlying reader is
    /// arc'd, this just clones the metadata struct.
    pub fn profile_cloned(&self) -> RasterProfile {
        match self {
            Self::GeoTiff(t) => t.profile().clone(),
            Self::Jpeg(i) | Self::Png(i) => i.profile().clone(),
        }
    }

    /// Stream all tiles through `on_each`, walking every pyramid level in
    /// (level, y, x) order. Per-tile errors are wrapped in
    /// [`ConvertError::Core`] via the From impl.
    pub fn for_each_tile<F>(self, mut on_each: F) -> Result<()>
    where
        F: FnMut(u8, u32, u32, RasterTile) -> Result<()>,
    {
        match self {
            Self::GeoTiff(tiff) => {
                let profile = tiff.profile().clone();
                let levels = profile.pyramid_levels.max(1);
                for level in 0..levels {
                    let scale = 1u32 << level;
                    let w = (profile.width / scale).max(1);
                    let h = (profile.height / scale).max(1);
                    let tw = profile.tile_size[0].max(1);
                    let th = profile.tile_size[1].max(1);
                    let gx = w.saturating_add(tw - 1) / tw;
                    let gy = h.saturating_add(th - 1) / th;
                    for y in 0..gy {
                        for x in 0..gx {
                            let tile = tiff.read_tile(level, x, y)?;
                            on_each(level, x, y, tile)?;
                        }
                    }
                }
                Ok(())
            }
            // JPEG / PNG are single-tile (whole-image) sources.
            Self::Jpeg(img) | Self::Png(img) => {
                let tile = img.read_tile(0, 0, 0)?;
                on_each(0, 0, 0, tile)
            }
        }
    }
}

/// Options for the raster output (canonical: COG).
#[derive(Debug, Clone)]
pub struct RasterSinkOptions {
    pub compression: Compression,
    pub deflate_level: u32,
}

impl Default for RasterSinkOptions {
    fn default() -> Self {
        // Match WriterOptions::default(): DEFLATE level 6 — the COG canonical.
        Self {
            compression: Compression::Deflate,
            deflate_level: 6,
        }
    }
}

// (Don't derive Default — Compression doesn't implement Default and the
// match here is the documentation we want users to read.)

/// An open raster sink — the canonical output is COG (a tiled GeoTIFF with
/// embedded overview IFDs).
pub enum RasterSink {
    Cog(Box<GeoTiffWriter<std::fs::File>>),
}

impl std::fmt::Debug for RasterSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cog(_) => f.write_str("RasterSink::Cog"),
        }
    }
}

impl RasterSink {
    pub fn create(path: &Path, profile: &RasterProfile, opts: RasterSinkOptions) -> Result<Self> {
        match Format::from_path(path)? {
            Format::GeoTiff => {
                let file = std::fs::File::create(path)?;
                let w = GeoTiffWriter::create(
                    file,
                    profile,
                    WriterOptions {
                        compression: opts.compression,
                        deflate_level: opts.deflate_level,
                    },
                )?;
                Ok(RasterSink::Cog(Box::new(w)))
            }
            other if other.modality() == Modality::Vector => Err(ConvertError::invalid(format!(
                "{} is a vector format; use Sink::create instead",
                other.label()
            ))),
            other => Err(ConvertError::UnsupportedFormat {
                ext: other.label().to_string(),
                path: path.display().to_string(),
                supported: ".tif, .tiff, .cog",
            }),
        }
    }

    pub fn write_tile(&mut self, level: u8, x: u32, y: u32, tile: &RasterTile) -> Result<()> {
        match self {
            Self::Cog(w) => w.write_tile(level, x, y, tile).map_err(Into::into),
        }
    }

    pub fn close(self) -> Result<()> {
        match self {
            Self::Cog(w) => {
                w.close()?;
                Ok(())
            }
        }
    }
}

/// Stats from a raster convert call. Mirrors `ConvertStats` for vector.
#[derive(Debug, Clone, Copy)]
pub struct RasterConvertStats {
    pub tiles: u64,
    pub levels: u8,
    pub elapsed_secs: f64,
    pub output_bytes: u64,
}

/// Convert any supported raster input to a canonical COG output.
///
/// Sprint 14a additions:
/// - `opts.to_crs` reprojects via `geonative-processing::raster::warp`
/// - `opts.build_pyramid` auto-generates overview levels via
///   `geonative-processing::raster::pyramid` (default: true)
pub fn convert_raster(
    src: &Path,
    dst: &Path,
    opts: RasterConvertOptions,
) -> Result<RasterConvertStats> {
    let source = RasterSource::open(src)?;

    // Materialise the input as a single tile-tree at level 0. For TIFF
    // sources that already have pyramids we use the existing tiles; for
    // image+sidecar inputs (which are single-tile already) this is
    // straightforward.
    let level0 = collect_level_zero(source)?;

    // If a target CRS was requested, warp first.
    let working = if let Some(target_crs) = &opts.to_crs {
        let target_grid_width = level0.width;
        let (gt, w, h) =
            geonative_processing::compute_target_grid(&level0, target_crs, target_grid_width)?;
        geonative_processing::warp_tile(
            &level0,
            target_crs,
            gt,
            w,
            h,
            &geonative_processing::WarpOptions::default(),
        )?
    } else {
        level0
    };

    // Build overview pyramid if requested and the base is large enough.
    let overviews: Vec<RasterTile> = if opts.build_pyramid {
        geonative_processing::build_overviews(
            &working,
            geonative_processing::PyramidOptions::default(),
        )
    } else {
        Vec::new()
    };

    // Build a sink profile from the (possibly warped) working tile.
    let mut sink_profile = source_profile_from_tile(&working);
    sink_profile.pyramid_levels = (overviews.len() + 1) as u8;

    let mut sink = RasterSink::create(dst, &sink_profile, opts.sink)?;

    let start = Instant::now();
    let mut tiles: u64 = 0;
    let mut max_level: u8 = 0;

    // Write level 0 (the working tile).
    sink.write_tile(0, 0, 0, &working)?;
    tiles += 1;

    // Write each overview level.
    for (i, ov) in overviews.iter().enumerate() {
        let level = (i + 1) as u8;
        sink.write_tile(level, 0, 0, ov)?;
        tiles += 1;
        max_level = level;
    }

    sink.close()?;

    let elapsed = start.elapsed().as_secs_f64();
    let output_bytes = std::fs::metadata(dst).map(|m| m.len()).unwrap_or(0);
    Ok(RasterConvertStats {
        tiles,
        levels: max_level + 1,
        elapsed_secs: elapsed,
        output_bytes,
    })
}

/// Stream every tile from `source` at level 0 into one big `RasterTile`.
/// For TIFF sources, this stitches the source's internal tiles back into
/// a single logical tile so warp / pyramid can operate on the whole image.
/// For image+sidecar sources (which are already single-tile), this is a
/// trivial copy.
fn collect_level_zero(source: RasterSource) -> Result<RasterTile> {
    use geonative_core::raster::{Band, GeoTransform, RasterProfile, RasterTile};
    let profile = source.profile_cloned();
    let total_w = profile.width;
    let total_h = profile.height;

    // Pre-allocate one big tile sized for the whole image.
    let bands_init: Vec<Band> = profile
        .bands
        .iter()
        .map(|d| {
            Band::new(
                d.clone(),
                vec![0u8; (total_w as usize) * (total_h as usize) * d.dtype.size_bytes()],
            )
        })
        .collect();
    let mut big = RasterTile {
        width: total_w,
        height: total_h,
        bands: bands_init,
        geo_transform: profile.geo_transform,
        crs: profile.crs.clone(),
    };

    let tile_w = profile.tile_size[0];
    let tile_h = profile.tile_size[1];

    // Stitch each level-0 source tile into the big buffer at its position.
    let mut got_any = false;
    source.for_each_tile(|level, tx, ty, tile| {
        if level != 0 {
            return Ok(());
        }
        got_any = true;
        let x0 = tx * tile_w;
        let y0 = ty * tile_h;
        let copy_w = (tile.width).min(total_w.saturating_sub(x0));
        let copy_h = (tile.height).min(total_h.saturating_sub(y0));
        for (band_idx, src_band) in tile.bands.iter().enumerate() {
            let bpp = src_band.descriptor.dtype.size_bytes();
            let dst_band = &mut big.bands[band_idx];
            for row in 0..copy_h as usize {
                let src_off = row * (tile.width as usize) * bpp;
                let dst_off = ((y0 as usize + row) * (total_w as usize) + x0 as usize) * bpp;
                let len = (copy_w as usize) * bpp;
                dst_band.data[dst_off..dst_off + len]
                    .copy_from_slice(&src_band.data[src_off..src_off + len]);
            }
        }
        Ok(())
    })?;
    if !got_any {
        return Err(ConvertError::invalid("source had no level-0 tiles"));
    }
    // Quiet the unused import warning in dev builds.
    let _ = GeoTransform::north_up;
    let _ = RasterProfile {
        width: 0,
        height: 0,
        bands: vec![],
        geo_transform: GeoTransform::north_up(0.0, 0.0, 1.0, 1.0),
        crs: profile.crs.clone(),
        tile_size: [1, 1],
        pyramid_levels: 1,
    };
    Ok(big)
}

/// Construct a fresh `RasterProfile` from a tile — used to seed the sink
/// when warp has changed the working dimensions / CRS / geo-transform.
fn source_profile_from_tile(tile: &RasterTile) -> geonative_core::raster::RasterProfile {
    geonative_core::raster::RasterProfile {
        width: tile.width,
        height: tile.height,
        bands: tile.bands.iter().map(|b| b.descriptor.clone()).collect(),
        geo_transform: tile.geo_transform,
        crs: tile.crs.clone(),
        tile_size: [tile.width, tile.height],
        pyramid_levels: 1,
    }
}

/// Options for [`convert_raster`].
#[derive(Debug, Clone)]
pub struct RasterConvertOptions {
    pub sink: RasterSinkOptions,
    /// If set, reproject the source raster into this CRS using
    /// `geonative-processing::raster::warp`. Sprint 14a supports this for
    /// single-tile sources (image+sidecar, small TIFFs); multi-tile TIFF
    /// stitching lands in v0.2.
    pub to_crs: Option<geonative_core::Crs>,
    /// Auto-build overview pyramid for the output. Default `true` — makes
    /// the output a true COG, not just a tiled TIFF.
    pub build_pyramid: bool,
}

impl Default for RasterConvertOptions {
    fn default() -> Self {
        Self {
            sink: RasterSinkOptions::default(),
            to_crs: None,
            build_pyramid: true,
        }
    }
}
