//! Stream features from an input source through a bbox filter and write the
//! survivors to a `.parquet`. Uses `Geometry::bbox()` for an intersect test —
//! no exact geometry-vs-bbox clipping (a feature whose bbox touches the
//! query bbox passes through whole).
//!
//! For exact spatial predicates we'd need an R-tree + per-vertex tests; that
//! belongs in a future `clip` / `spatial-join` command, not here.

use std::path::Path;
use std::time::Instant;

use geonative_core::{Feature, Schema};
use geonative_geoparquet::{GeoParquetWriter, WriterOptions};

/// Query bbox as `[xmin, ymin, xmax, ymax]`.
pub type Bbox2 = [f64; 4];

pub struct FilterBboxArgs<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub bbox: Bbox2,
    pub layer: Option<&'a str>,
    pub batch_size: usize,
}

pub fn filter_bbox(args: FilterBboxArgs<'_>) -> Result<FilterReport, String> {
    let ext = args
        .input
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| {
            format!(
                "could not determine input format from path: {} (extension required)",
                args.input.display()
            )
        })?;

    // Each branch owns its reader, materialises a schema clone, and runs the
    // same write-loop. Macroless because the iterator types differ.
    let start = Instant::now();
    let opts = WriterOptions {
        batch_size: args.batch_size,
        add_bbox_columns: true,
        hilbert_sort: false,
        ..WriterOptions::default()
    };

    let (scanned, kept) = match ext.as_str() {
        "gdb" => filter_filegdb(args.input, args.output, args.bbox, args.layer, opts)?,
        "shp" => filter_shapefile(args.input, args.output, args.bbox, opts)?,
        "parquet" => filter_geoparquet(args.input, args.output, args.bbox, opts)?,
        other => {
            return Err(format!(
                "unsupported input extension '.{other}' (supported: .gdb, .shp, .parquet)"
            ))
        }
    };

    Ok(FilterReport {
        scanned,
        kept,
        elapsed_secs: start.elapsed().as_secs_f64(),
    })
}

fn filter_filegdb(
    input: &Path,
    output: &Path,
    bbox: Bbox2,
    layer: Option<&str>,
    opts: WriterOptions,
) -> Result<(u64, u64), String> {
    let gdb =
        geonative_filegdb::open(input).map_err(|e| format!("opening {}: {e}", input.display()))?;
    let layers = gdb.layers();
    let layer_name = match (layer, layers) {
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
    let layer = gdb
        .layer(&layer_name)
        .map_err(|e| format!("opening layer '{layer_name}': {e}"))?;
    write_filtered(layer.schema(), layer.read(), output, bbox, opts)
}

fn filter_shapefile(
    input: &Path,
    output: &Path,
    bbox: Bbox2,
    opts: WriterOptions,
) -> Result<(u64, u64), String> {
    let shp = geonative_shapefile::open(input)
        .map_err(|e| format!("opening {}: {e}", input.display()))?;
    write_filtered(shp.schema(), shp.read(), output, bbox, opts)
}

fn filter_geoparquet(
    input: &Path,
    output: &Path,
    bbox: Bbox2,
    opts: WriterOptions,
) -> Result<(u64, u64), String> {
    let reader = geonative_geoparquet::GeoParquetReader::open(input)
        .map_err(|e| format!("opening {}: {e}", input.display()))?;
    let schema = reader.schema().clone();
    write_filtered(&schema, reader.into_features(), output, bbox, opts)
}

fn write_filtered<I, E>(
    schema: &Schema,
    iter: I,
    output: &Path,
    bbox: Bbox2,
    opts: WriterOptions,
) -> Result<(u64, u64), String>
where
    I: IntoIterator<Item = Result<Feature, E>>,
    E: std::fmt::Display,
{
    let file = std::fs::File::create(output)
        .map_err(|e| format!("creating {}: {e}", output.display()))?;
    let mut writer =
        GeoParquetWriter::create(file, schema, opts).map_err(|e| format!("creating writer: {e}"))?;

    let mut scanned: u64 = 0;
    let mut kept: u64 = 0;
    for feat in iter {
        let feat = feat.map_err(|e| format!("decoding feature {scanned}: {e}"))?;
        scanned += 1;
        if let Some(geom) = feat.geometry.as_ref() {
            if let Some(fb) = geom.bbox() {
                if bbox_intersects(fb, bbox) {
                    writer
                        .write(&feat)
                        .map_err(|e| format!("writing feature {scanned}: {e}"))?;
                    kept += 1;
                }
            }
        }
    }
    writer.close().map_err(|e| format!("closing writer: {e}"))?;
    Ok((scanned, kept))
}

fn bbox_intersects(a: [f64; 4], b: Bbox2) -> bool {
    a[0] <= b[2] && a[2] >= b[0] && a[1] <= b[3] && a[3] >= b[1]
}

#[derive(Debug)]
pub struct FilterReport {
    pub scanned: u64,
    pub kept: u64,
    pub elapsed_secs: f64,
}

pub fn parse_bbox(s: &str) -> Result<Bbox2, String> {
    let parts: Vec<&str> = s.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(format!(
            "--bbox expects 4 comma-separated numbers (xmin,ymin,xmax,ymax), got: {s}"
        ));
    }
    let mut out = [0.0_f64; 4];
    for (i, p) in parts.iter().enumerate() {
        out[i] = p
            .parse::<f64>()
            .map_err(|e| format!("--bbox component {i} ('{p}'): {e}"))?;
    }
    if out[0] > out[2] || out[1] > out[3] {
        return Err(format!(
            "--bbox is degenerate (xmin>xmax or ymin>ymax): [{}, {}, {}, {}]",
            out[0], out[1], out[2], out[3]
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bbox_ok() {
        let b = parse_bbox("1,2,3,4").unwrap();
        assert_eq!(b, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn parse_bbox_with_spaces() {
        let b = parse_bbox(" 1 , 2 , 3 , 4 ").unwrap();
        assert_eq!(b, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn parse_bbox_wrong_count() {
        assert!(parse_bbox("1,2,3").is_err());
        assert!(parse_bbox("1,2,3,4,5").is_err());
    }

    #[test]
    fn parse_bbox_degenerate() {
        assert!(parse_bbox("3,2,1,4").is_err());
        assert!(parse_bbox("1,4,3,2").is_err());
    }

    #[test]
    fn bbox_intersect_cases() {
        // identical
        assert!(bbox_intersects([0.0, 0.0, 1.0, 1.0], [0.0, 0.0, 1.0, 1.0]));
        // overlap on corner (edge touch is intersection per the <= semantics)
        assert!(bbox_intersects([0.0, 0.0, 1.0, 1.0], [1.0, 1.0, 2.0, 2.0]));
        // disjoint
        assert!(!bbox_intersects([0.0, 0.0, 1.0, 1.0], [2.0, 2.0, 3.0, 3.0]));
        // a contains b
        assert!(bbox_intersects([0.0, 0.0, 10.0, 10.0], [4.0, 4.0, 5.0, 5.0]));
    }
}
