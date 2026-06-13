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
/// v0.1 does NOT support `--to-crs` reproject for raster (raster warp +
/// resampling lands in Phase F / v0.2). If `opts.to_crs` is set with a
/// raster input, this returns an error rather than silently ignoring it.
pub fn convert_raster(
    src: &Path,
    dst: &Path,
    opts: RasterConvertOptions,
) -> Result<RasterConvertStats> {
    if opts.to_crs.is_some() {
        return Err(ConvertError::invalid(
            "raster reproject (--to-crs) is deferred to v0.2; convert without --to-crs for now",
        ));
    }

    let source = RasterSource::open(src)?;
    let profile = source.profile_cloned();
    let mut sink = RasterSink::create(dst, &profile, opts.sink)?;

    let start = Instant::now();
    let mut tiles: u64 = 0;
    let mut max_level: u8 = 0;
    source.for_each_tile(|level, x, y, tile| {
        sink.write_tile(level, x, y, &tile)?;
        tiles += 1;
        max_level = max_level.max(level);
        Ok(())
    })?;
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

/// Options for [`convert_raster`].
#[derive(Debug, Clone, Default)]
pub struct RasterConvertOptions {
    pub sink: RasterSinkOptions,
    /// Reserved for Phase F (raster warp). Setting this in v0.1 returns an
    /// error so callers know reproject is not silently dropped.
    pub to_crs: Option<geonative_core::Crs>,
}
