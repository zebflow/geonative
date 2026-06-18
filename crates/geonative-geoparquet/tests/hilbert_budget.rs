//! Hilbert memory-budget guard: must return `HilbertBudgetExceeded`
//! instead of OOM-killing the process.
//!
//! Reproduces the shape of the Vicmap SOIL_TYPE incident — many features
//! with non-trivial polygon vertex counts — but at miniature scale so the
//! test runs in <1s. The cap shape (return Err well below RAM exhaustion)
//! is what we're proving; the exact budget tuning is a caller choice.

use geonative_core::{
    Coord, Crs, Feature, FieldDef, GeomField, Geometry, GeometryType, LineString, Polygon, Schema,
    Value, ValueType,
};
use geonative_geoparquet::{GeoParquetError, GeoParquetWriter, WriterOptions};

fn schema() -> Schema {
    Schema::new(
        vec![FieldDef::new("id", ValueType::Int32, false)],
        Some(GeomField::new("geometry", GeometryType::Polygon)),
        Crs::Epsg(4326),
    )
}

/// Build a polygon with `n` exterior-ring vertices. Heavy = more vertices.
fn heavy_polygon(n: usize, offset: f64) -> Geometry {
    let mut coords: Vec<Coord> = (0..n)
        .map(|i| {
            let t = i as f64 / n as f64 * std::f64::consts::TAU;
            Coord::xy(offset + t.cos(), offset + t.sin())
        })
        .collect();
    // close the ring
    coords.push(coords[0]);
    Geometry::Polygon(Polygon::new(LineString::new(coords), Vec::new()))
}

#[test]
fn write_errors_with_clean_budget_exceeded_before_oom() {
    // 200 polygons × 5_000 vertices = 1M coords ≈ 16 MB just for geometry.
    // Set a 2 MB cap → the writer must trip on roughly the ~125th feature
    // and return Err, NOT OOM-kill the process.
    let opts = WriterOptions {
        hilbert_sort: true,
        hilbert_memory_budget_bytes: 2 * 1024 * 1024,
        ..WriterOptions::default()
    };
    let buf = Vec::<u8>::new();
    let mut w = GeoParquetWriter::create(buf, &schema(), opts).unwrap();

    let mut tripped = false;
    for i in 0..200 {
        let feat = Feature::new(
            Some(i as i64 + 1),
            Some(heavy_polygon(5_000, i as f64 * 0.01)),
            vec![Value::Int32(i + 1)],
        );
        match w.write(&feat) {
            Ok(()) => continue,
            Err(GeoParquetError::HilbertBudgetExceeded {
                budget_bytes,
                used_bytes,
                features_buffered,
            }) => {
                assert_eq!(budget_bytes, 2 * 1024 * 1024);
                assert!(
                    used_bytes <= budget_bytes,
                    "used {used_bytes} must be <= budget {budget_bytes}"
                );
                assert!(
                    features_buffered > 0,
                    "should have buffered at least one feature before tripping"
                );
                eprintln!(
                    "tripped cleanly at feature {i}: buffered={features_buffered}, used={used_bytes}/{budget_bytes}"
                );
                tripped = true;
                break;
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
    assert!(
        tripped,
        "writer should have tripped HilbertBudgetExceeded within 200 features"
    );
}

#[test]
fn small_dataset_under_budget_still_completes() {
    // Same shape, but stay under the cap. Should write successfully.
    let opts = WriterOptions {
        hilbert_sort: true,
        hilbert_memory_budget_bytes: 16 * 1024 * 1024,
        ..WriterOptions::default()
    };
    let mut buf = Vec::<u8>::new();
    {
        let mut w = GeoParquetWriter::create(&mut buf, &schema(), opts).unwrap();
        for i in 0..20 {
            let feat = Feature::new(
                Some(i as i64 + 1),
                Some(heavy_polygon(500, i as f64 * 0.01)),
                vec![Value::Int32(i + 1)],
            );
            w.write(&feat)
                .expect("each write should succeed under budget");
        }
        w.close().expect("close should succeed");
    }
    // Parquet magic at both ends — proves we produced a valid file.
    assert!(buf.len() > 100);
    assert_eq!(&buf[..4], b"PAR1");
    assert_eq!(&buf[buf.len() - 4..], b"PAR1");
}

#[test]
fn non_hilbert_path_ignores_budget() {
    // hilbert_sort=false → budget shouldn't trip even on a tiny cap,
    // because the writer streams features straight through to Arrow.
    let opts = WriterOptions {
        hilbert_sort: false,
        hilbert_memory_budget_bytes: 1, // pathologically small — ignored
        ..WriterOptions::default()
    };
    let mut buf = Vec::<u8>::new();
    {
        let mut w = GeoParquetWriter::create(&mut buf, &schema(), opts).unwrap();
        for i in 0..50 {
            let feat = Feature::new(
                Some(i as i64 + 1),
                Some(heavy_polygon(500, i as f64 * 0.01)),
                vec![Value::Int32(i + 1)],
            );
            w.write(&feat)
                .expect("streaming path should ignore the budget");
        }
        w.close().unwrap();
    }
    assert!(buf.len() > 100);
}
