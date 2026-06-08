//! Rewrite a `.parquet` file with Hilbert-sorted features and bbox covering
//! columns enabled. Functionally equivalent to a parquet→parquet copy except
//! it forces `hilbert_sort: true` and `add_bbox_columns: true`, which is the
//! whole point.
//!
//! Memory ≈ peak feature count × average feature size (Hilbert sort requires
//! buffering all features before write). For larger-than-memory inputs the
//! caller should partition by tile / row-group bbox first.

use std::path::Path;
use std::time::Instant;

use crate::error::Result;
use crate::reader::GeoParquetReader;
use crate::writer::{GeoParquetWriter, WriterOptions};

#[derive(Debug, Clone)]
pub struct OptimizeOptions {
    pub batch_size: usize,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self { batch_size: 10_000 }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OptimizeReport {
    pub features: u64,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub elapsed_secs: f64,
}

pub fn optimize(input: &Path, output: &Path, opts: OptimizeOptions) -> Result<OptimizeReport> {
    let reader = GeoParquetReader::open(input)?;
    let schema = reader.schema().clone();
    let writer_opts = WriterOptions {
        batch_size: opts.batch_size,
        add_bbox_columns: true,
        hilbert_sort: true,
        ..WriterOptions::default()
    };

    let file = std::fs::File::create(output)?;
    let mut writer = GeoParquetWriter::create(file, &schema, writer_opts)?;

    let start = Instant::now();
    let mut count: u64 = 0;
    for feat in reader.into_features() {
        let feat = feat?;
        writer.write(&feat)?;
        count += 1;
    }
    writer.close()?;

    let elapsed = start.elapsed().as_secs_f64();
    let input_bytes = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
    let output_bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);

    Ok(OptimizeReport {
        features: count,
        input_bytes,
        output_bytes,
        elapsed_secs: elapsed,
    })
}
