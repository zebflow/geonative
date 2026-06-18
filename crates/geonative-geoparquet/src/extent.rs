//! O(1) dataset extent via parquet column statistics.
//!
//! ## Why this exists
//!
//! Computing a dataset bbox by iterating every feature works but is
//! O(features × decode-cost) — pathological on multi-GB GeoParquet with
//! heavy polygon geometry (a real Zebflow incident OOM-killed a worker
//! pod doing exactly this on a 1.32 GB / 822k-feature parquet).
//!
//! When the file was written with `add_bbox_columns: true` (the
//! `GeoParquetWriter` default), the writer emitted `xmin`, `ymin`,
//! `xmax`, `ymax` as `Float64` covering columns and parquet's row-group
//! statistics automatically captured per-group min/max for each. The
//! dataset bbox is then `(min(xmin), min(ymin), max(xmax), max(ymax))`
//! across all row groups — derivable from the footer alone, **no
//! feature decode**.
//!
//! Cost: one open + parse-footer (~tens of KB regardless of file size).
//!
//! Returns `None` (caller should scan) when:
//! - the file doesn't have the four flat bbox columns, or
//! - any of those columns has no statistics (unusual for Float64; can
//!   happen if a non-default writer wrote the file).

use std::path::Path;

use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::file::statistics::Statistics;

use crate::error::Result;

const BBOX_COL_NAMES: [&str; 4] = ["xmin", "ymin", "xmax", "ymax"];

/// Read `[xmin, ymin, xmax, ymax]` from row-group statistics on the
/// flat bbox covering columns. See module docs for when this returns
/// `None`.
pub fn dataset_extent_from_stats(path: impl AsRef<Path>) -> Result<Option<[f64; 4]>> {
    let file = std::fs::File::open(path.as_ref())?;
    let reader = SerializedFileReader::new(file)?;
    let metadata = reader.metadata();
    let schema = metadata.file_metadata().schema_descr();

    // Locate the four covering columns by name. If any is missing, the
    // file wasn't written with our bbox-covering convention and the
    // caller has to scan.
    let mut col_idx = [usize::MAX; 4];
    for (i, &name) in BBOX_COL_NAMES.iter().enumerate() {
        let mut found = None;
        for c in 0..schema.num_columns() {
            if schema.column(c).name() == name {
                found = Some(c);
                break;
            }
        }
        let Some(c) = found else { return Ok(None) };
        col_idx[i] = c;
    }

    // Aggregate min/max across all row groups. Min of `xmin` column +
    // min of `ymin` column + max of `xmax` column + max of `ymax`
    // column gives the dataset bbox.
    let mut acc: Option<[f64; 4]> = None;
    for rg in metadata.row_groups() {
        let xmin = stat_min(rg.column(col_idx[0]).statistics());
        let ymin = stat_min(rg.column(col_idx[1]).statistics());
        let xmax = stat_max(rg.column(col_idx[2]).statistics());
        let ymax = stat_max(rg.column(col_idx[3]).statistics());
        let (Some(x0), Some(y0), Some(x1), Some(y1)) = (xmin, ymin, xmax, ymax) else {
            // A row group missing one of the four stat blocks
            // disqualifies the whole answer — falling back to None
            // tells the caller "scan instead".
            return Ok(None);
        };
        acc = Some(match acc {
            None => [x0, y0, x1, y1],
            Some([a0, a1, a2, a3]) => [a0.min(x0), a1.min(y0), a2.max(x1), a3.max(y1)],
        });
    }
    Ok(acc)
}

fn stat_min(stats: Option<&Statistics>) -> Option<f64> {
    match stats {
        Some(Statistics::Double(s)) => s.min_opt().copied(),
        _ => None,
    }
}

fn stat_max(stats: Option<&Statistics>) -> Option<f64> {
    match stats {
        Some(Statistics::Double(s)) => s.max_opt().copied(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GeoParquetWriter, WriterOptions};
    use geonative_core::{
        Coord, Crs, Feature, FieldDef, GeomField, Geometry, GeometryType, LineString, Polygon,
        Schema, Value, ValueType,
    };

    fn schema() -> Schema {
        Schema::new(
            vec![FieldDef::new("id", ValueType::Int32, false)],
            Some(GeomField::new("geometry", GeometryType::Polygon)),
            Crs::Epsg(4326),
        )
    }

    fn square_at(cx: f64, cy: f64, half: f64) -> Geometry {
        Geometry::Polygon(Polygon::new(
            LineString::new(vec![
                Coord::xy(cx - half, cy - half),
                Coord::xy(cx + half, cy - half),
                Coord::xy(cx + half, cy + half),
                Coord::xy(cx - half, cy + half),
                Coord::xy(cx - half, cy - half),
            ]),
            Vec::new(),
        ))
    }

    #[test]
    fn extent_from_stats_matches_dataset_bbox() {
        // Write 30 polygons spanning a known bbox, then read it back
        // via stats alone.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let file = std::fs::File::create(tmp.path()).unwrap();
            let mut w = GeoParquetWriter::create(
                file,
                &schema(),
                WriterOptions {
                    batch_size: 10, // multiple row groups → tests aggregation
                    ..WriterOptions::default()
                },
            )
            .unwrap();
            for i in 0..30 {
                let cx = i as f64 * 0.5;
                let cy = (i % 5) as f64 * 0.25;
                let feat = Feature::new(
                    Some(i as i64 + 1),
                    Some(square_at(cx, cy, 0.1)),
                    vec![Value::Int32(i as i32 + 1)],
                );
                w.write(&feat).unwrap();
            }
            w.close().unwrap();
        }

        let bbox = dataset_extent_from_stats(tmp.path()).unwrap().unwrap();
        // Expected: xmin = -0.1, xmax = 29*0.5 + 0.1 = 14.6
        //           ymin = -0.1, ymax = 4*0.25 + 0.1 = 1.1
        assert!((bbox[0] - -0.1).abs() < 1e-9, "xmin: {}", bbox[0]);
        assert!((bbox[1] - -0.1).abs() < 1e-9, "ymin: {}", bbox[1]);
        assert!((bbox[2] - 14.6).abs() < 1e-9, "xmax: {}", bbox[2]);
        assert!((bbox[3] - 1.1).abs() < 1e-9, "ymax: {}", bbox[3]);
    }

    #[test]
    fn returns_none_when_bbox_columns_absent() {
        // Write a parquet with add_bbox_columns=false → no xmin/ymin/etc.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let file = std::fs::File::create(tmp.path()).unwrap();
            let mut w = GeoParquetWriter::create(
                file,
                &schema(),
                WriterOptions {
                    add_bbox_columns: false,
                    ..WriterOptions::default()
                },
            )
            .unwrap();
            w.write(&Feature::new(
                Some(1),
                Some(square_at(0.0, 0.0, 1.0)),
                vec![Value::Int32(1)],
            ))
            .unwrap();
            w.close().unwrap();
        }
        assert!(dataset_extent_from_stats(tmp.path()).unwrap().is_none());
    }
}
