//! Rewrite a `.parquet` file with Hilbert-sorted features and bbox covering
//! columns. Functionally equivalent to `convert in.parquet out.parquet` if
//! that conversion existed — except here it always sets `hilbert_sort: true`
//! and `add_bbox_columns: true`, which is the *point* of the command.
//!
//! Reads everything into memory before writing (Hilbert sort requires
//! buffering). Memory ~= peak feature count × average feature size.

use std::path::Path;
use std::time::Instant;

use geonative_geoparquet::{GeoParquetReader, GeoParquetWriter, WriterOptions};

pub struct OptimizeArgs<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub batch_size: usize,
}

pub fn optimize(args: OptimizeArgs<'_>) -> Result<OptimizeReport, String> {
    let reader = GeoParquetReader::open(args.input)
        .map_err(|e| format!("opening {}: {e}", args.input.display()))?;

    let schema = reader.schema().clone();
    let opts = WriterOptions {
        batch_size: args.batch_size,
        add_bbox_columns: true,
        hilbert_sort: true,
        ..WriterOptions::default()
    };

    let file = std::fs::File::create(args.output)
        .map_err(|e| format!("creating {}: {e}", args.output.display()))?;
    let mut writer = GeoParquetWriter::create(file, &schema, opts)
        .map_err(|e| format!("creating writer: {e}"))?;

    let start = Instant::now();
    let mut count: u64 = 0;
    for feat in reader.into_features() {
        let feat = feat.map_err(|e| format!("decoding feature {count}: {e}"))?;
        writer
            .write(&feat)
            .map_err(|e| format!("writing feature {count}: {e}"))?;
        count += 1;
    }
    writer.close().map_err(|e| format!("closing writer: {e}"))?;

    let elapsed = start.elapsed();
    let input_size = std::fs::metadata(args.input).map(|m| m.len()).unwrap_or(0);
    let output_size = std::fs::metadata(args.output).map(|m| m.len()).unwrap_or(0);

    Ok(OptimizeReport {
        features: count,
        input_bytes: input_size,
        output_bytes: output_size,
        elapsed_secs: elapsed.as_secs_f64(),
    })
}

#[derive(Debug)]
pub struct OptimizeReport {
    pub features: u64,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub elapsed_secs: f64,
}
