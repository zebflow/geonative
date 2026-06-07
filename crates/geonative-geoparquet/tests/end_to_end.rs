//! End-to-end: open a real FileGDB, write it through `GeoParquetWriter`,
//! then read the parquet back with the upstream `parquet` crate and verify
//! the file is well-formed + the data round-trips.
//!
//! Gated on the `GEONATIVE_FIXTURE_GDB` env var (same convention as the
//! filegdb crate's real-fixture tests).

use std::path::PathBuf;

use arrow::array::{Array, BinaryArray, Int32Array, StringArray};
use geonative_filegdb::open as open_gdb;
use geonative_geoparquet::{GeoParquetWriter, WriterOptions};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::file::reader::{FileReader, SerializedFileReader};

fn fixture_path() -> Option<PathBuf> {
    std::env::var_os("GEONATIVE_FIXTURE_GDB").map(PathBuf::from)
}

#[test]
fn vmfeat_gdb_round_trips_through_geoparquet() {
    let Some(gdb_path) = fixture_path() else {
        eprintln!("(skipped: set GEONATIVE_FIXTURE_GDB to enable)");
        return;
    };

    let gdb = open_gdb(&gdb_path).expect("open .gdb");
    let layer = gdb.layer("FOI_LINE").expect("FOI_LINE layer");
    let schema = layer.schema().clone();

    // Write to a temp parquet file.
    let tmpdir = tempfile::tempdir().unwrap();
    let out_path = tmpdir.path().join("foi_line.parquet");
    {
        let file = std::fs::File::create(&out_path).unwrap();
        let mut writer = GeoParquetWriter::create(file, &schema, WriterOptions::default())
            .expect("create writer");
        for f in layer.read() {
            let f = f.expect("decode feature");
            writer.write(&f).expect("write feature");
        }
        writer.close().expect("close writer");
    }

    let file_size = std::fs::metadata(&out_path).unwrap().len();
    assert!(file_size > 0);
    println!(
        "wrote {} ({} bytes)",
        out_path.display(),
        file_size
    );

    // Read back with the parquet crate.
    let file = std::fs::File::open(&out_path).unwrap();

    // 1) Verify file metadata: GeoParquet `geo` key exists and looks right.
    let bytes = std::fs::read(&out_path).unwrap();
    let reader_for_meta = SerializedFileReader::new(bytes::Bytes::from(bytes)).unwrap();
    let geo = reader_for_meta
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .as_ref()
        .and_then(|kvs| kvs.iter().find(|kv| kv.key == "geo"))
        .and_then(|kv| kv.value.clone())
        .expect("`geo` key not found in parquet metadata");
    assert!(geo.contains(r#""version":"1.1.0""#), "geo: {geo}");
    assert!(geo.contains(r#""encoding":"WKB""#), "geo: {geo}");
    assert!(geo.contains(r#""geometry_types":["MultiLineString"]"#), "geo: {geo}");
    assert!(geo.contains(r#""code":7844"#), "EPSG:7844 missing from PROJJSON: {geo}");
    assert!(geo.contains(r#""covering""#), "bbox covering missing: {geo}");

    // 2) Read all record batches, verify row count and column shape.
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
    let arrow_schema = builder.schema().clone();
    let reader = builder.build().unwrap();
    let batches: Vec<_> = reader
        .into_iter()
        .map(|b| b.expect("decode batch"))
        .collect();

    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 75, "expected 75 features from FOI_LINE");

    // Geometry column present and 0-th.
    let names: Vec<&str> = arrow_schema.fields().iter().map(|f| f.name().as_str()).collect();
    assert_eq!(names[0], "SHAPE", "geometry column should be SHAPE per source");
    assert!(names.contains(&"UFI"));
    assert!(names.contains(&"NAME"));
    assert!(names.contains(&"xmin") && names.contains(&"xmax"));

    // 3) Spot-check FID 1 attribute + geometry.
    let first_batch = &batches[0];
    let ufi_idx = arrow_schema.index_of("UFI").unwrap();
    let name_idx = arrow_schema.index_of("NAME").unwrap();
    let geom_idx = arrow_schema.index_of("SHAPE").unwrap();

    let ufi_col = first_batch
        .column(ufi_idx)
        .as_any()
        .downcast_ref::<Int32Array>()
        .unwrap();
    assert_eq!(ufi_col.value(0), 64536814, "FID 1 UFI mismatch");

    let name_col = first_batch
        .column(name_idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(name_col.value(0), "BASS GAS - LEONGARTHA AND WONTHAGGI");

    let geom_col = first_batch
        .column(geom_idx)
        .as_any()
        .downcast_ref::<BinaryArray>()
        .unwrap();
    let wkb = geom_col.value(0);
    assert!(wkb.len() > 100, "WKB should be substantial for a multi-part line");
    // Byte 0 = byte_order (1 = LE), bytes 1..5 = geometry type.
    assert_eq!(wkb[0], 1, "WKB byte order should be LE");
    let geom_type = u32::from_le_bytes(wkb[1..5].try_into().unwrap());
    assert_eq!(geom_type, 5, "geometry should be MultiLineString (type 5)");

    // 4) Spot-check the bbox columns for the first feature: lon/lat should be Victoria.
    let xmin_idx = arrow_schema.index_of("xmin").unwrap();
    let xmax_idx = arrow_schema.index_of("xmax").unwrap();
    let ymin_idx = arrow_schema.index_of("ymin").unwrap();
    let ymax_idx = arrow_schema.index_of("ymax").unwrap();
    use arrow::array::Float64Array;
    let xmin = first_batch.column(xmin_idx).as_any().downcast_ref::<Float64Array>().unwrap().value(0);
    let xmax = first_batch.column(xmax_idx).as_any().downcast_ref::<Float64Array>().unwrap().value(0);
    let ymin = first_batch.column(ymin_idx).as_any().downcast_ref::<Float64Array>().unwrap().value(0);
    let ymax = first_batch.column(ymax_idx).as_any().downcast_ref::<Float64Array>().unwrap().value(0);
    assert!(xmin >= 144.0 && xmax <= 147.0, "lon bbox: {xmin}..{xmax}");
    assert!(ymin >= -39.0 && ymax <= -36.0, "lat bbox: {ymin}..{ymax}");

    println!(
        "OK: {} features, {} batches, file size {}, FID 1 bbox xy = [{xmin:.4},{ymin:.4},{xmax:.4},{ymax:.4}]",
        total_rows,
        batches.len(),
        file_size
    );
}
