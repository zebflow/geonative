//! The `convert(src, dst, opts)` orchestrator — the canonical `Source` →
//! `Sink` pipe used by both the `geonative convert` CLI subcommand and any
//! downstream Rust service that wants to script format conversion.
//!
//! Future middleware (`--to-crs`, `--simplify-tolerance`, `--filter-bbox`)
//! attaches here as one-line transforms inside the per-feature callback.

use std::path::Path;
use std::time::Instant;

use geonative_core::Crs;
use geonative_proj::Transformer;

use crate::error::{ConvertError, Result};
use crate::io::{Sink, SinkOptions, Source};

#[derive(Clone, Default)]
pub struct ConvertOptions {
    /// FileGDB-only: which layer to extract.
    pub layer: Option<String>,
    /// Sink configuration (batch size, Hilbert sort, bbox columns).
    pub sink: SinkOptions,
    /// Reproject every feature's geometry to this CRS mid-stream. If set,
    /// the output's declared CRS is updated to match.
    pub to_crs: Option<Crs>,
    /// Optional progress callback fired every ~2s with `(features_written,
    /// expected_total_if_known)`. None means silent.
    pub progress: Option<ProgressFn>,
}

impl From<geonative_proj::ProjError> for ConvertError {
    fn from(e: geonative_proj::ProjError) -> Self {
        ConvertError::InvalidArgument(format!("reproject: {e}"))
    }
}

impl std::fmt::Debug for ConvertOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConvertOptions")
            .field("layer", &self.layer)
            .field("sink", &self.sink)
            .field("to_crs", &self.to_crs)
            .field("progress", &self.progress.as_ref().map(|_| "<callback>"))
            .finish()
    }
}

/// `(written, expected_total_or_zero)`. Receive `0` for expected_total when
/// the source can't cheaply provide one (GeoParquet today).
pub type ProgressFn = std::sync::Arc<dyn Fn(u64, i64) + Send + Sync>;

#[derive(Debug, Clone, Copy)]
pub struct ConvertStats {
    pub features: u64,
    pub elapsed_secs: f64,
    pub output_bytes: u64,
}

/// Convenience: convert with reproject, no other options. Convert preserves
/// the source layer naming etc.
pub fn reproject(src: &Path, dst: &Path, target_crs: Crs) -> Result<ConvertStats> {
    convert(
        src,
        dst,
        ConvertOptions {
            to_crs: Some(target_crs),
            ..ConvertOptions::default()
        },
    )
}

/// Convert from any supported input to any supported output. Dispatches on
/// the input's modality:
///
/// - **Vector → Vector** (current vector pipeline; the same one Zebflow uses
///   for filegdb / shapefile / geoparquet / geojson conversions today)
/// - **Raster → Raster** (delegates to [`crate::convert_raster`])
/// - **Cross-modal** (vector ↔ raster) — not yet supported; returns
///   `ConvertError::InvalidArgument`. Rasterize / vectorize land with
///   `geonative-processing` in Sprint 13 Phase F.
pub fn convert(src: &Path, dst: &Path, opts: ConvertOptions) -> Result<ConvertStats> {
    use crate::io::Modality;
    let src_modality = crate::io::Format::from_path(src)?.modality();
    let dst_modality = crate::io::Format::from_path(dst)?.modality();

    match (src_modality, dst_modality) {
        (Modality::Vector, Modality::Vector) => convert_vector(src, dst, opts),
        (Modality::Raster, Modality::Raster) => {
            let raster_opts = crate::raster::RasterConvertOptions {
                sink: crate::raster::RasterSinkOptions::default(),
                to_crs: opts.to_crs,
                build_pyramid: true,
            };
            let stats = crate::raster::convert_raster(src, dst, raster_opts)?;
            // Bridge raster stats onto the unified ConvertStats shape so
            // callers don't have to branch.
            Ok(ConvertStats {
                features: stats.tiles,
                elapsed_secs: stats.elapsed_secs,
                output_bytes: stats.output_bytes,
            })
        }
        (Modality::Vector, Modality::Raster) => Err(ConvertError::invalid(
            "cross-modal vector → raster (rasterize) lands in Sprint 13 Phase F",
        )),
        (Modality::Raster, Modality::Vector) => Err(ConvertError::invalid(
            "cross-modal raster → vector (vectorize) lands in Sprint 13 Phase F",
        )),
    }
}

fn convert_vector(src: &Path, dst: &Path, opts: ConvertOptions) -> Result<ConvertStats> {
    let source = Source::open(src, opts.layer.as_deref())?;
    let mut schema = source.schema_cloned()?;
    let expected = source.feature_count().unwrap_or(0);

    // If reprojecting, build a Transformer up-front (once) and overwrite
    // the schema's CRS so the sink writes the right metadata.
    let transformer = match &opts.to_crs {
        Some(target) => Some(Transformer::from_crs(&schema.crs, target)?),
        None => None,
    };
    if let Some(target) = &opts.to_crs {
        schema.crs = target.clone();
    }

    let mut sink = Sink::create(dst, &schema, opts.sink.clone())?;

    let start = Instant::now();
    let mut last_report = Instant::now();
    let mut count: u64 = 0;
    let progress = opts.progress.clone();
    source.for_each(|mut feat| {
        if let Some(t) = &transformer {
            if let Some(g) = feat.geometry.as_mut() {
                t.transform_geometry(g)?;
            }
        }
        sink.write(&feat)?;
        count += 1;
        if let Some(cb) = &progress {
            if last_report.elapsed().as_secs() >= 2 {
                cb(count, expected);
                last_report = Instant::now();
            }
        }
        Ok(())
    })?;
    sink.close()?;

    let elapsed = start.elapsed().as_secs_f64();
    let output_bytes = std::fs::metadata(dst).map(|m| m.len()).unwrap_or(0);
    Ok(ConvertStats {
        features: count,
        elapsed_secs: elapsed,
        output_bytes,
    })
}
