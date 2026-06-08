//! The `convert(src, dst, opts)` orchestrator — the canonical `Source` →
//! `Sink` pipe used by both the `geonative convert` CLI subcommand and any
//! downstream Rust service that wants to script format conversion.
//!
//! Future middleware (`--to-crs`, `--simplify-tolerance`, `--filter-bbox`)
//! attaches here as one-line transforms inside the per-feature callback.

use std::path::Path;
use std::time::Instant;

use crate::error::Result;
use crate::io::{Sink, SinkOptions, Source};

#[derive(Clone, Default)]
pub struct ConvertOptions {
    /// FileGDB-only: which layer to extract.
    pub layer: Option<String>,
    /// Sink configuration (batch size, Hilbert sort, bbox columns).
    pub sink: SinkOptions,
    /// Optional progress callback fired every ~2s with `(features_written,
    /// expected_total_if_known)`. None means silent.
    pub progress: Option<ProgressFn>,
}

impl std::fmt::Debug for ConvertOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConvertOptions")
            .field("layer", &self.layer)
            .field("sink", &self.sink)
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

pub fn convert(src: &Path, dst: &Path, opts: ConvertOptions) -> Result<ConvertStats> {
    let source = Source::open(src, opts.layer.as_deref())?;
    let schema = source.schema_cloned()?;
    let expected = source.feature_count().unwrap_or(0);

    let mut sink = Sink::create(dst, &schema, opts.sink.clone())?;

    let start = Instant::now();
    let mut last_report = Instant::now();
    let mut count: u64 = 0;
    let progress = opts.progress.clone();
    source.for_each(|feat| {
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
