//! Format-polymorphic I/O dispatch for the CLI.
//!
//! `Source` opens any supported input (`.gdb` / `.shp` / `.parquet` /
//! `.geojson`) and exposes a uniform `for_each(Feature)` over its rows.
//! `Sink` writes to any supported output (`.parquet` / `.geojson`).
//!
//! Subcommands stay short: pick the right `Source`, run their per-feature
//! logic in the callback, write through a `Sink`.

use std::io::BufWriter;
use std::path::Path;

use geonative_core::{Feature, Schema};
use geonative_filegdb::Geodatabase;
use geonative_geojson::{GeoJsonReader, GeoJsonWriter};
use geonative_geoparquet::{GeoParquetReader, GeoParquetWriter, WriterOptions};
use geonative_shapefile::Shapefile;

/// What kind of file a path refers to. `Detect::from_path` parses the
/// extension; everything else routes off the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    FileGdb,
    Shapefile,
    GeoParquet,
    GeoJson,
}

impl Format {
    pub fn from_path(path: &Path) -> Result<Self, String> {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| {
                format!(
                    "could not determine format from path: {} (extension required)",
                    path.display()
                )
            })?;
        match ext.as_str() {
            "gdb" => Ok(Self::FileGdb),
            "shp" => Ok(Self::Shapefile),
            "parquet" => Ok(Self::GeoParquet),
            "geojson" | "json" => Ok(Self::GeoJson),
            other => Err(format!(
                "unsupported extension '.{other}' (supported: .gdb, .shp, .parquet, .geojson)"
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::FileGdb => "filegdb",
            Self::Shapefile => "shapefile",
            Self::GeoParquet => "geoparquet",
            Self::GeoJson => "geojson",
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
    /// an error listing the available layer names.
    pub fn open(path: &Path, layer_hint: Option<&str>) -> Result<Self, String> {
        match Format::from_path(path)? {
            Format::FileGdb => {
                let gdb = geonative_filegdb::open(path)
                    .map_err(|e| format!("opening {}: {e}", path.display()))?;
                let layer_name = match (layer_hint, gdb.layers()) {
                    (Some(name), _) => name.to_string(),
                    (None, [single]) => single.name.clone(),
                    (None, many) => {
                        return Err(format!(
                            "input has {} layers; specify which with --layer NAME. Available: {}",
                            many.len(),
                            many.iter()
                                .map(|l| l.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    }
                };
                Ok(Source::FileGdb { gdb, layer_name })
            }
            Format::Shapefile => geonative_shapefile::open(path)
                .map(Source::Shapefile)
                .map_err(|e| format!("opening {}: {e}", path.display())),
            Format::GeoParquet => GeoParquetReader::open(path)
                .map(Source::GeoParquet)
                .map_err(|e| format!("opening {}: {e}", path.display())),
            Format::GeoJson => GeoJsonReader::open(path)
                .map(Source::GeoJson)
                .map_err(|e| format!("opening {}: {e}", path.display())),
        }
    }

/// Return an owned schema. FileGDB has to briefly open the layer to fetch
    /// it; the others have it sitting on the reader handle. Returning an owned
    /// value sidesteps the lifetime gymnastics of FileGDB's borrow-shaped
    /// `Layer::schema()` API.
    pub fn schema_cloned(&self) -> Result<Schema, String> {
        match self {
            Source::FileGdb { gdb, layer_name } => {
                let layer = gdb
                    .layer(layer_name)
                    .map_err(|e| format!("opening layer '{layer_name}': {e}"))?;
                Ok(layer.schema().clone())
            }
            Source::Shapefile(s) => Ok(s.schema().clone()),
            Source::GeoParquet(r) => Ok(r.schema().clone()),
            Source::GeoJson(r) => Ok(r.schema().clone()),
        }
    }

    pub fn feature_count(&self) -> Option<i64> {
        match self {
            Source::FileGdb { gdb, layer_name } => gdb
                .layer(layer_name)
                .ok()
                .map(|l| l.feature_count()),
            Source::Shapefile(s) => Some(s.feature_count() as i64),
            // GeoParquetReader doesn't yet expose a cheap row-count.
            Source::GeoParquet(_) => None,
            Source::GeoJson(r) => Some(r.feature_count() as i64),
        }
    }

    /// Stream all features through `on_each`. Per-feature decode errors are
    /// converted to strings and returned via `?` — the underlying reader is
    /// consumed on success.
    pub fn for_each<F>(self, mut on_each: F) -> Result<(), String>
    where
        F: FnMut(Feature) -> Result<(), String>,
    {
        match self {
            Source::FileGdb { gdb, layer_name } => {
                let layer = gdb
                    .layer(&layer_name)
                    .map_err(|e| format!("opening layer '{layer_name}': {e}"))?;
                for (i, feat) in layer.read().enumerate() {
                    let feat = feat.map_err(|e| format!("decoding feature {i}: {e}"))?;
                    on_each(feat)?;
                }
                Ok(())
            }
            Source::Shapefile(s) => {
                for (i, feat) in s.read().enumerate() {
                    let feat = feat.map_err(|e| format!("decoding feature {i}: {e}"))?;
                    on_each(feat)?;
                }
                Ok(())
            }
            Source::GeoParquet(r) => {
                for (i, feat) in r.into_features().enumerate() {
                    let feat = feat.map_err(|e| format!("decoding feature {i}: {e}"))?;
                    on_each(feat)?;
                }
                Ok(())
            }
            Source::GeoJson(r) => {
                for feat in r.into_features() {
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

pub struct SinkOptions {
    pub batch_size: usize,
    pub hilbert_sort: bool,
    pub add_bbox_columns: bool,
}

impl Default for SinkOptions {
    fn default() -> Self {
        Self {
            batch_size: 10_000,
            hilbert_sort: false,
            add_bbox_columns: true,
        }
    }
}

impl Sink {
    pub fn create(path: &Path, schema: &Schema, opts: SinkOptions) -> Result<Self, String> {
        match Format::from_path(path)? {
            Format::GeoParquet => {
                let file = std::fs::File::create(path)
                    .map_err(|e| format!("creating {}: {e}", path.display()))?;
                let writer_opts = WriterOptions {
                    batch_size: opts.batch_size,
                    add_bbox_columns: opts.add_bbox_columns,
                    hilbert_sort: opts.hilbert_sort,
                    ..WriterOptions::default()
                };
                let w = GeoParquetWriter::create(file, schema, writer_opts)
                    .map_err(|e| format!("creating parquet writer: {e}"))?;
                Ok(Sink::GeoParquet(Box::new(w)))
            }
            Format::GeoJson => {
                let file = std::fs::File::create(path)
                    .map_err(|e| format!("creating {}: {e}", path.display()))?;
                let w = GeoJsonWriter::create(BufWriter::new(file), schema)
                    .map_err(|e| format!("creating geojson writer: {e}"))?;
                Ok(Sink::GeoJson(Box::new(w)))
            }
            other => Err(format!(
                "unsupported output format: {} (supported: .parquet, .geojson)",
                other.label()
            )),
        }
    }

    pub fn write(&mut self, feat: &Feature) -> Result<(), String> {
        match self {
            Sink::GeoParquet(w) => w.write(feat).map_err(|e| format!("write parquet row: {e}")),
            Sink::GeoJson(w) => w.write(feat).map_err(|e| format!("write geojson feature: {e}")),
        }
    }

    pub fn close(self) -> Result<(), String> {
        match self {
            Sink::GeoParquet(w) => w
                .close()
                .map(|_| ())
                .map_err(|e| format!("closing parquet writer: {e}")),
            Sink::GeoJson(w) => w
                .close()
                .map(|_| ())
                .map_err(|e| format!("closing geojson writer: {e}")),
        }
    }
}
