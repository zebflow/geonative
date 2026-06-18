//! Format-polymorphic I/O dispatch. The two entry points here — [`Source`]
//! and [`Sink`] — are the foundation every higher-level operation in this
//! crate (convert, filter_bbox, inspect, metadata) is built on.
//!
//! Downstream services use these directly when the canned operations don't
//! quite fit:
//!
//! ```no_run
//! use geonative_convert::{Source, Sink, SinkOptions};
//! use std::path::Path;
//!
//! let source = Source::open(Path::new("in.gdb"), Some("roads"))?;
//! let schema = source.schema_cloned()?;
//! let mut sink = Sink::create(Path::new("out.parquet"), &schema, SinkOptions::default())?;
//! source.for_each(|feat| sink.write(&feat))?;
//! sink.close()?;
//! # Ok::<(), geonative_convert::ConvertError>(())
//! ```

use std::io::BufWriter;
use std::path::Path;

use geonative_core::{Feature, Schema};
use geonative_filegdb::Geodatabase;
use geonative_geojson::{GeoJsonReader, GeoJsonWriter};
use geonative_geoparquet::{GeoParquetReader, GeoParquetWriter, WriterOptions};
use geonative_shapefile::Shapefile;

use crate::error::{ConvertError, Result};

/// What kind of file a path refers to. Parsed from the extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    FileGdb,
    Shapefile,
    GeoParquet,
    GeoJson,
    /// GeoTIFF (regular or Cloud Optimized).
    GeoTiff,
    /// JPEG with a `.jgw` world file next to it.
    Jpeg,
    /// PNG with a `.pgw` world file next to it.
    Png,
}

/// Whether a format represents vector features or raster pixels.
/// Used by the top-level `convert()` to route the request to the right pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modality {
    Vector,
    Raster,
}

impl Format {
    pub fn from_path(path: &Path) -> Result<Self> {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .ok_or_else(|| ConvertError::UnknownFormat(path.display().to_string()))?;
        Self::from_extension(ext)
            .map_err(|_| ConvertError::UnknownFormat(path.display().to_string()))
    }

    /// Resolve `Format` from a bare extension (no leading dot). Used when
    /// the source lives in an object store and we only have the object
    /// `Path` to read the extension off — `Path::from_path` won't work
    /// because object_store paths aren't filesystem paths.
    pub fn from_extension(ext: &str) -> Result<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "gdb" => Ok(Self::FileGdb),
            "shp" => Ok(Self::Shapefile),
            "parquet" => Ok(Self::GeoParquet),
            "geojson" | "json" => Ok(Self::GeoJson),
            "tif" | "tiff" | "cog" => Ok(Self::GeoTiff),
            "jpg" | "jpeg" => Ok(Self::Jpeg),
            "png" => Ok(Self::Png),
            other => Err(ConvertError::UnsupportedFormat {
                ext: other.to_string(),
                path: format!("<{ext}>"),
                supported: ".gdb, .shp, .parquet, .geojson, .tif, .cog, .jpg, .png",
            }),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::FileGdb => "filegdb",
            Self::Shapefile => "shapefile",
            Self::GeoParquet => "geoparquet",
            Self::GeoJson => "geojson",
            Self::GeoTiff => "geotiff",
            Self::Jpeg => "jpeg",
            Self::Png => "png",
        }
    }

    /// Whether this format holds vector features or raster pixels.
    pub fn modality(self) -> Modality {
        match self {
            Self::FileGdb | Self::Shapefile | Self::GeoParquet | Self::GeoJson => Modality::Vector,
            Self::GeoTiff | Self::Jpeg | Self::Png => Modality::Raster,
        }
    }
}

/// An opened input. Owns its underlying reader, knows its schema, and can
/// stream its features through a per-feature callback.
pub enum Source {
    FileGdb {
        gdb: Geodatabase,
        layer_name: String,
    },
    Shapefile(Shapefile),
    GeoParquet(GeoParquetReader),
    GeoJson(GeoJsonReader),
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileGdb { layer_name, .. } => write!(f, "Source::FileGdb({layer_name})"),
            Self::Shapefile(_) => f.write_str("Source::Shapefile"),
            Self::GeoParquet(_) => f.write_str("Source::GeoParquet"),
            Self::GeoJson(_) => f.write_str("Source::GeoJson"),
        }
    }
}

impl Source {
    /// Open `path`. If the format is multi-layer (FileGDB), `layer_hint`
    /// selects the layer; passing `None` against a multi-layer GDB returns
    /// [`ConvertError::LayerAmbiguous`] listing available names.
    pub fn open(path: &Path, layer_hint: Option<&str>) -> Result<Self> {
        match Format::from_path(path)? {
            Format::FileGdb => {
                let gdb = geonative_filegdb::open(path)?;
                let layer_name = match (layer_hint, gdb.layers()) {
                    (Some(name), _) => name.to_string(),
                    (None, [single]) => single.name.clone(),
                    (None, many) => {
                        return Err(ConvertError::LayerAmbiguous {
                            count: many.len(),
                            available: many
                                .iter()
                                .map(|l| l.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", "),
                        })
                    }
                };
                Ok(Source::FileGdb { gdb, layer_name })
            }
            Format::Shapefile => Ok(Source::Shapefile(geonative_shapefile::open(path)?)),
            Format::GeoParquet => Ok(Source::GeoParquet(GeoParquetReader::open(path)?)),
            Format::GeoJson => Ok(Source::GeoJson(GeoJsonReader::open(path)?)),
            Format::GeoTiff | Format::Jpeg | Format::Png => Err(ConvertError::invalid(
                "raster format; use RasterSource::open instead",
            )),
        }
    }

    pub fn format(&self) -> Format {
        match self {
            Source::FileGdb { .. } => Format::FileGdb,
            Source::Shapefile(_) => Format::Shapefile,
            Source::GeoParquet(_) => Format::GeoParquet,
            Source::GeoJson(_) => Format::GeoJson,
        }
    }

    /// Return an owned schema. FileGDB has to briefly open the layer to
    /// fetch it; the others have it on the reader handle. Returning an
    /// owned value sidesteps the lifetime gymnastics of `Layer::schema()`.
    pub fn schema_cloned(&self) -> Result<Schema> {
        match self {
            Source::FileGdb { gdb, layer_name } => {
                let layer = gdb.layer(layer_name)?;
                Ok(layer.schema().clone())
            }
            Source::Shapefile(s) => Ok(s.schema().clone()),
            Source::GeoParquet(r) => Ok(r.schema().clone()),
            Source::GeoJson(r) => Ok(r.schema().clone()),
        }
    }

    /// Cheap feature count if the format exposes one without scanning.
    /// `None` for GeoParquet (no per-row-group sum exposed yet on the reader).
    pub fn feature_count(&self) -> Option<i64> {
        match self {
            Source::FileGdb { gdb, layer_name } => {
                gdb.layer(layer_name).ok().map(|l| l.feature_count())
            }
            Source::Shapefile(s) => Some(s.feature_count() as i64),
            Source::GeoParquet(_) => None,
            Source::GeoJson(r) => Some(r.feature_count() as i64),
        }
    }

    /// Stream all features through `on_each`. Per-feature decode errors are
    /// wrapped in [`ConvertError::DecodeRow`] with the row index attached.
    /// The reader is consumed on success.
    pub fn for_each<F>(self, mut on_each: F) -> Result<()>
    where
        F: FnMut(Feature) -> Result<()>,
    {
        match self {
            Source::FileGdb { gdb, layer_name } => {
                let layer = gdb.layer(&layer_name)?;
                for (i, feat) in layer.read().enumerate() {
                    let feat = feat.map_err(|e| ConvertError::DecodeRow {
                        row: i as u64,
                        source: Box::new(ConvertError::FileGdb(e)),
                    })?;
                    on_each(feat)?;
                }
                Ok(())
            }
            Source::Shapefile(s) => {
                for (i, feat) in s.read().enumerate() {
                    let feat = feat.map_err(|e| ConvertError::DecodeRow {
                        row: i as u64,
                        source: Box::new(ConvertError::Shapefile(e)),
                    })?;
                    on_each(feat)?;
                }
                Ok(())
            }
            Source::GeoParquet(r) => {
                for (i, feat) in r.into_features().enumerate() {
                    let feat = feat.map_err(|e| ConvertError::DecodeRow {
                        row: i as u64,
                        source: Box::new(ConvertError::GeoParquet(e)),
                    })?;
                    on_each(feat)?;
                }
                Ok(())
            }
            Source::GeoJson(r) => {
                for (i, feat) in r.into_features().enumerate() {
                    let feat = feat.map_err(|e| ConvertError::DecodeRow {
                        row: i as u64,
                        source: Box::new(ConvertError::GeoJson(e)),
                    })?;
                    on_each(feat)?;
                }
                Ok(())
            }
        }
    }
}

/// An output writer. Picks parquet or GeoJSON by output extension and
/// hides the writer-shape difference behind a uniform `write(&Feature)`.
pub enum Sink {
    GeoParquet(Box<GeoParquetWriter<std::fs::File>>),
    GeoJson(Box<GeoJsonWriter<BufWriter<std::fs::File>>>),
}

impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GeoParquet(_) => f.write_str("Sink::GeoParquet"),
            Self::GeoJson(_) => f.write_str("Sink::GeoJson"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SinkOptions {
    pub batch_size: usize,
    pub hilbert_sort: bool,
    pub add_bbox_columns: bool,
    /// Forwarded to
    /// [`geonative_geoparquet::WriterOptions::hilbert_memory_budget_bytes`].
    /// Bounds peak RAM when `hilbert_sort` is enabled so heavy-geometry
    /// inputs don't OOM-kill the worker; the writer returns a clean
    /// error before crossing this. Default 512 MiB; lower it for small
    /// k8s pods, raise it on bare metal.
    pub hilbert_memory_budget_bytes: usize,
}

impl Default for SinkOptions {
    fn default() -> Self {
        Self {
            batch_size: 10_000,
            hilbert_sort: false,
            add_bbox_columns: true,
            hilbert_memory_budget_bytes: 512 * 1024 * 1024,
        }
    }
}

impl Sink {
    pub fn create(path: &Path, schema: &Schema, opts: SinkOptions) -> Result<Self> {
        match Format::from_path(path)? {
            Format::GeoParquet => {
                let file = std::fs::File::create(path)?;
                let writer_opts = WriterOptions {
                    batch_size: opts.batch_size,
                    add_bbox_columns: opts.add_bbox_columns,
                    hilbert_sort: opts.hilbert_sort,
                    hilbert_memory_budget_bytes: opts.hilbert_memory_budget_bytes,
                    ..WriterOptions::default()
                };
                let w = GeoParquetWriter::create(file, schema, writer_opts)?;
                Ok(Sink::GeoParquet(Box::new(w)))
            }
            Format::GeoJson => {
                let file = std::fs::File::create(path)?;
                let w = GeoJsonWriter::create(BufWriter::new(file), schema)?;
                Ok(Sink::GeoJson(Box::new(w)))
            }
            other => Err(ConvertError::UnsupportedFormat {
                ext: other.label().to_string(),
                path: path.display().to_string(),
                supported: ".parquet, .geojson (read also supports .gdb / .shp)",
            }),
        }
    }

    pub fn write(&mut self, feat: &Feature) -> Result<()> {
        match self {
            Sink::GeoParquet(w) => w.write(feat).map_err(ConvertError::from),
            Sink::GeoJson(w) => w.write(feat).map_err(ConvertError::from),
        }
    }

    pub fn close(self) -> Result<()> {
        match self {
            Sink::GeoParquet(w) => {
                w.close()?;
                Ok(())
            }
            Sink::GeoJson(w) => {
                w.close()?;
                Ok(())
            }
        }
    }
}
